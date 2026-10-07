//! Workspace packaging for Tracel Console.
//!
//! Packages an entire workspace as a single zip archive, respecting ignore files,
//! and computes the code version digest from the packaged files.

use std::{
    collections::BTreeMap,
    fs::{File, Metadata},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use colored::Colorize;
use ignore::{WalkBuilder, overrides::OverrideBuilder};
use zip::{DateTime, ZipWriter, write::SimpleFileOptions};

use crate::error::{CliError, ErrorKind};
use crate::tools::event::Reporter;
use crate::tools::fs::{file_sha256_and_size, manifest_digest};
use crate::tools::workspace::WorkspaceInfo;

/// Paths never packaged, whatever the ignore files say: git metadata, `target` at the
/// workspace root, and any `target` directory below it (gitignore syntax).
const EXCLUDED: [&str; 3] = [".git", "/target", "target/"];

#[derive(Debug)]
pub struct ArchiveMetadata {
    /// The code version digest of the packaged files (see `source_digest`).
    pub digest: String,
    pub path: PathBuf,
    /// SHA-256 of the archive file.
    pub checksum: String,
    pub size: u64,
}

pub struct PackageEvent {
    pub message: String,
}

type PackageEventReporter = dyn Reporter<PackageEvent>;

/// Package the entire workspace as a single zip archive, skipping ignored files.
pub fn package_workspace(
    workspace: &WorkspaceInfo,
    event_reporter: Arc<PackageEventReporter>,
) -> anyhow::Result<ArchiveMetadata> {
    let workspace_root = workspace
        .workspace_root
        .canonicalize()
        .context("Failed to canonicalize workspace root")?;

    tracing::info!(
        "Packaging workspace at: {}",
        workspace_root.display().to_string().bold()
    );

    // List all files in the workspace respecting ignore files
    event_reporter.report_event(PackageEvent {
        message: "Discovering files (respecting .gitignore)".to_string(),
    });

    let files = list_workspace_files(&workspace_root)?;

    tracing::info!("Found {} files to package", files.len());

    event_reporter.report_event(PackageEvent {
        message: format!("Discovered {} files", files.len()),
    });

    event_reporter.report_event(PackageEvent {
        message: "Computing digest".to_string(),
    });

    let digest = source_digest(&files)?;

    // Create the archive
    event_reporter.report_event(PackageEvent {
        message: "Creating compressed archive".to_string(),
    });

    // Write the archive under the cargo target directory, the idiomatic home for
    // build artifacts (kept out of the archive itself by the `target/` exclusion).
    let output_dir = workspace
        .metadata
        .target_directory
        .as_std_path()
        .join("tracel")
        .join("package");
    let archive_path = output_dir.join(format!("{}.zip", workspace.workspace_name));

    std::fs::create_dir_all(&output_dir)?;

    let uncompressed_size = write_zip_archive(&files, File::create(&archive_path)?)?;

    event_reporter.report_event(PackageEvent {
        message: format!(
            "Archive created: {}",
            human_readable_bytes(uncompressed_size)
        ),
    });

    // Calculate checksum
    event_reporter.report_event(PackageEvent {
        message: "Computing checksum".to_string(),
    });

    let (checksum, size) = file_sha256_and_size(&archive_path)?;

    Ok(ArchiveMetadata {
        digest,
        path: archive_path,
        checksum,
        size,
    })
}

/// The code version digest of a source package: SHA-256 over the sorted `path:sha256` lines
/// of the packaged files (see [`manifest_digest`]).
fn source_digest(files: &BTreeMap<String, PathBuf>) -> anyhow::Result<String> {
    let mut lines = Vec::with_capacity(files.len());
    for (name, file_path) in files {
        let (checksum, _) = file_sha256_and_size(file_path)?;
        lines.push((name.as_str(), checksum));
    }
    let lines = lines
        .iter()
        .map(|(name, checksum)| (*name, checksum.as_str()));
    Ok(manifest_digest(lines))
}

/// Lists the files to package as a map from their `/`-separated path relative to
/// `workspace_root` to their full path. `workspace_root` must be canonical.
///
/// Ignore files apply with or without a git repository: `.gitignore` and `.ignore` in the
/// workspace and its parent directories, `.git/info/exclude`, and git's global excludes file.
/// [`EXCLUDED`] paths are never packaged. Hidden files are packaged in a git repository unless
/// ignored, and skipped outside one. Links to files are packaged as the files they point to;
/// links to directories are not followed. Fails with a usage error when no file is left.
fn list_workspace_files(workspace_root: &Path) -> anyhow::Result<BTreeMap<String, PathBuf>> {
    let in_git_repository = workspace_root
        .ancestors()
        .any(|dir| dir.join(".git").exists());

    let mut excluded = OverrideBuilder::new(workspace_root);
    for glob in EXCLUDED {
        excluded.add(&format!("!{glob}"))?;
    }
    let walk = WalkBuilder::new(workspace_root)
        .require_git(false)
        .hidden(!in_git_repository)
        .overrides(excluded.build()?)
        .build();

    let mut files = BTreeMap::new();
    for entry in walk {
        let entry = entry.context("Failed to list the workspace files")?;
        if let Some(error) = entry.error() {
            tracing::warn!("{error}");
        }
        if !entry.path().is_file() {
            continue;
        }
        let relative_path = entry
            .path()
            .strip_prefix(workspace_root)
            .context("Failed to strip workspace root prefix")?;
        let name = relative_path
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        files.insert(name, entry.into_path());
    }

    if files.is_empty() {
        return Err(CliError::new(
            ErrorKind::Usage,
            format!("No files to package in {}.", workspace_root.display()),
        )
        .with_hint("Check the ignore files that apply to the workspace.")
        .into());
    }
    Ok(files)
}

/// Writes `files` to a zip archive in path order, under their workspace-relative names so the
/// workspace root is the archive root, and returns their total size. Entries have a fixed
/// timestamp and keep only the executable bit of their permissions, so the same files always
/// produce the same archive.
fn write_zip_archive(files: &BTreeMap<String, PathBuf>, dst: File) -> anyhow::Result<u64> {
    let mut zip = ZipWriter::new(dst);
    let mut uncompressed_size = 0;

    for (name, file_path) in files {
        let mut file = File::open(file_path)?;
        let metadata = file.metadata()?;
        let mode = if is_executable(&metadata) {
            0o755
        } else {
            0o644
        };
        let options = SimpleFileOptions::default()
            .last_modified_time(DateTime::default())
            .unix_permissions(mode)
            .large_file(metadata.len() > u64::from(u32::MAX));

        zip.start_file(name.as_str(), options)?;
        uncompressed_size += std::io::copy(&mut file, &mut zip)?;
    }

    zip.finish()?;
    Ok(uncompressed_size)
}

#[cfg(unix)]
fn is_executable(metadata: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &Metadata) -> bool {
    false
}

/// Formats a byte count into a human-readable string.
fn human_readable_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;

    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }

    format!("{:.2} {}", size, UNITS[unit_idx])
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A uniquely named directory under the system temp directory, removed on drop.
    struct TempWorkspace(PathBuf);

    impl TempWorkspace {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "tracel-cli-packager-{name}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, rel_path: &str, contents: &str) {
            let path = self.0.join(rel_path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }

        /// Makes `rel_path` the root of a git repository, as packaging detects one: a `.git`
        /// entry, here a directory with some content.
        fn git_init(&self, rel_path: &str) {
            self.write(&format!("{rel_path}/.git/HEAD"), "ref: refs/heads/main\n");
        }

        /// The files packaged from the workspace at `rel_root`.
        fn files(&self, rel_root: &str) -> BTreeMap<String, PathBuf> {
            list_workspace_files(&self.0.join(rel_root).canonicalize().unwrap()).unwrap()
        }

        fn digest(&self) -> String {
            source_digest(&self.files("")).unwrap()
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sha256(contents: &str) -> String {
        format!("{:x}", Sha256::digest(contents))
    }

    #[test]
    fn digest_hashes_workspace_relative_paths_and_contents() {
        let workspace = TempWorkspace::new("format");
        workspace.write("Cargo.toml", "[package]");
        workspace.write("src/main.rs", "fn main() {}");

        let expected = manifest_digest([
            ("Cargo.toml", sha256("[package]").as_str()),
            ("src/main.rs", sha256("fn main() {}").as_str()),
        ]);
        assert_eq!(workspace.digest(), expected);
    }

    #[test]
    fn digest_does_not_depend_on_file_creation_order() {
        let first = TempWorkspace::new("order-first");
        first.write("Cargo.toml", "[package]");
        first.write("src/main.rs", "fn main() {}");
        let second = TempWorkspace::new("order-second");
        second.write("src/main.rs", "fn main() {}");
        second.write("Cargo.toml", "[package]");

        assert_eq!(first.digest(), second.digest());
    }

    #[test]
    fn digest_changes_with_file_content_or_name() {
        let workspace = TempWorkspace::new("changes");
        workspace.write("src/main.rs", "fn main() {}");
        let original = workspace.digest();

        workspace.write("src/main.rs", "fn main() { println!(); }");
        let edited = workspace.digest();
        assert_ne!(edited, original);

        std::fs::rename(
            workspace.0.join("src/main.rs"),
            workspace.0.join("src/lib.rs"),
        )
        .unwrap();
        assert_ne!(workspace.digest(), edited);
    }

    #[test]
    fn digest_ignores_excluded_files() {
        let workspace = TempWorkspace::new("excluded");
        workspace.write("src/main.rs", "fn main() {}");
        let original = workspace.digest();

        workspace.write("target/debug/app", "binary");
        workspace.write(".env", "TOKEN=1");
        assert_eq!(workspace.digest(), original);

        workspace.write("src/lib.rs", "");
        assert_ne!(workspace.digest(), original);
    }

    #[test]
    fn digest_ignores_gitignored_files() {
        for in_repository in [false, true] {
            let workspace = TempWorkspace::new("gitignored");
            if in_repository {
                workspace.git_init(".");
            }
            workspace.write(".gitignore", "*.log\n");
            workspace.write("src/main.rs", "fn main() {}");
            let original = workspace.digest();

            workspace.write("train.log", "epoch 1");
            workspace.write("target/debug/app", "binary");
            assert_eq!(workspace.digest(), original);

            workspace.write("src/lib.rs", "");
            assert_ne!(workspace.digest(), original);
        }
    }

    #[test]
    fn ignore_files_and_git_excludes_apply() {
        let workspace = TempWorkspace::new("ignore-files");
        workspace.git_init(".");
        workspace.write(".git/info/exclude", "*.bak\n");
        workspace.write("data/.ignore", "*.csv\n!keep.csv\n");
        workspace.write("data/train.csv", "1,2");
        workspace.write("data/keep.csv", "3,4");
        workspace.write("notes.bak", "draft");
        workspace.write("src/main.rs", "fn main() {}");

        assert_eq!(
            workspace.files("").keys().collect::<Vec<_>>(),
            ["data/.ignore", "data/keep.csv", "src/main.rs"]
        );
    }

    #[test]
    fn hidden_files_are_packaged_only_in_a_repository() {
        let workspace = TempWorkspace::new("hidden");
        workspace.write(".gitignore", ".env\n");
        workspace.write(".env", "TOKEN=1");
        workspace.write(".cargo/config.toml", "[build]");
        workspace.write("src/main.rs", "fn main() {}");
        assert_eq!(
            workspace.files("").keys().collect::<Vec<_>>(),
            ["src/main.rs"]
        );

        workspace.git_init(".");
        assert_eq!(
            workspace.files("").keys().collect::<Vec<_>>(),
            [".cargo/config.toml", ".gitignore", "src/main.rs"]
        );
    }

    #[test]
    fn packaging_nothing_is_a_usage_error() {
        let workspace = TempWorkspace::new("nothing");
        workspace.write(".ignore", "*\n");
        workspace.write("Cargo.toml", "[package]");

        let root = workspace.0.canonicalize().unwrap();
        let error = list_workspace_files(&root).unwrap_err();
        assert_eq!(crate::error::classify(&error), ErrorKind::Usage);
        assert!(error.to_string().contains("No files to package"));
    }

    #[test]
    fn workspace_in_a_repository_subdirectory_packages_its_own_files() {
        let nested = TempWorkspace::new("subdir");
        nested.git_init(".");
        nested.write(".git/info/exclude", "*.bak\n");
        nested.write(".gitignore", "*.tmp\n");
        nested.write("README.md", "outside the workspace");
        nested.write("ws/.gitignore", "*.log\n");
        nested.write("ws/Cargo.toml", "[package]");
        nested.write("ws/src/main.rs", "fn main() {}");
        nested.write("ws/train.log", "epoch 1");
        nested.write("ws/cache.tmp", "cache");
        nested.write("ws/notes.bak", "draft");
        nested.write("ws/target/debug/app", "binary");
        nested.git_init("ws/vendor/lib");
        nested.write("ws/vendor/lib/lib.rs", "pub fn lib() {}");

        let root = TempWorkspace::new("subdir-root");
        root.git_init(".");
        root.write(".gitignore", "*.log\n");
        root.write("Cargo.toml", "[package]");
        root.write("src/main.rs", "fn main() {}");
        root.write("train.log", "epoch 1");
        root.write("target/debug/app", "binary");
        root.git_init("vendor/lib");
        root.write("vendor/lib/lib.rs", "pub fn lib() {}");

        let files = nested.files("ws");
        assert_eq!(
            files.keys().collect::<Vec<_>>(),
            [
                ".gitignore",
                "Cargo.toml",
                "src/main.rs",
                "vendor/lib/lib.rs"
            ]
        );
        assert_eq!(
            source_digest(&files).unwrap(),
            source_digest(&root.files("")).unwrap()
        );
    }

    #[test]
    fn archive_holds_the_workspace_at_its_root() {
        let contents = [
            ("Cargo.toml", "[package]"),
            ("scripts/build.sh", "#!/bin/sh\n"),
            ("src/main.rs", "fn main() {}"),
        ];
        let output = TempWorkspace::new("archive-output");
        let write_archive = |name: &str, reversed: bool| {
            let workspace = TempWorkspace::new(name);
            let mut contents = contents.to_vec();
            if reversed {
                contents.reverse();
            }
            for (rel_path, text) in contents {
                workspace.write(rel_path, text);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let script = workspace.0.join("scripts/build.sh");
                std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let path = output.0.join(format!("{name}.zip"));
            write_zip_archive(&workspace.files(""), File::create(&path).unwrap()).unwrap();
            path
        };
        let first = write_archive("archive-first", false);
        let second = write_archive("archive-second", true);
        assert_eq!(
            std::fs::read(&first).unwrap(),
            std::fs::read(&second).unwrap()
        );

        let mut archive = zip::ZipArchive::new(File::open(&first).unwrap()).unwrap();
        let entries: Vec<(String, Option<u32>)> = (0..archive.len())
            .map(|index| {
                let entry = archive.by_index(index).unwrap();
                assert_eq!(entry.last_modified(), Some(DateTime::default()));
                (entry.name().to_string(), entry.unix_mode())
            })
            .collect();
        let script_mode = if cfg!(unix) { 0o100755 } else { 0o100644 };
        assert_eq!(
            entries,
            [
                ("Cargo.toml".to_string(), Some(0o100644)),
                ("scripts/build.sh".to_string(), Some(script_mode)),
                ("src/main.rs".to_string(), Some(0o100644)),
            ]
        );

        let extracted = output.0.join("extracted");
        archive.extract(&extracted).unwrap();
        for (rel_path, text) in contents {
            assert_eq!(
                std::fs::read_to_string(extracted.join(rel_path)).unwrap(),
                text
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = std::fs::metadata(extracted.join("scripts/build.sh")).unwrap();
            assert_eq!(script.permissions().mode() & 0o777, 0o755);
        }
    }
}
