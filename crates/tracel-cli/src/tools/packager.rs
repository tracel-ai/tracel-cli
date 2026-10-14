//! Workspace packaging for Tracel Console.
//!
//! Packages an entire workspace as a single zip archive, respecting ignore files,
//! and computes the code version digest from the files a source package holds.

use std::{
    collections::BTreeMap,
    fs::{File, Metadata},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use ignore::{WalkBuilder, overrides::OverrideBuilder};
use zip::{DateTime, ZipWriter, write::SimpleFileOptions};

use crate::error::{CliError, ErrorKind};
use crate::tools::event::Reporter;
use crate::tools::fs::{file_sha256_and_size, manifest_digest};
use crate::tools::workspace::WorkspaceInfo;
use crate::ui::{counted, size};

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
    let workspace_root = canonical_root(workspace)?;

    event_reporter.report_event(PackageEvent {
        message: "Listing files".to_string(),
    });
    let files = list_workspace_files(&workspace_root)?;
    let digest = source_digest(&files)?;

    event_reporter.report_event(PackageEvent {
        message: format!(
            "Compressing {}",
            counted(files.len() as u64, ["file", "files"])
        ),
    });
    // The archive goes in the target directory, which it leaves out.
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
        message: format!("Computing the checksum of {}", size(uncompressed_size)),
    });
    let (checksum, size) = file_sha256_and_size(&archive_path)?;
    Ok(ArchiveMetadata {
        digest,
        path: archive_path,
        checksum,
        size,
    })
}

/// Compute the code version digest of the workspace without packaging it: the digest of
/// the files a source package holds. Binary packages use it too, so every package of the
/// same source, in either mode and for any target, belongs to one code version.
pub fn workspace_digest(workspace: &WorkspaceInfo) -> anyhow::Result<String> {
    source_digest(&list_workspace_files(&canonical_root(workspace)?)?)
}

fn canonical_root(workspace: &WorkspaceInfo) -> anyhow::Result<PathBuf> {
    workspace
        .workspace_root
        .canonicalize()
        .context("Can't find the workspace root")
}

/// Compute the code version digest of a source package: SHA-256 over the sorted
/// `path:sha256` lines of the packaged files (see [`manifest_digest`]).
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

/// List the files to package as a map from their `/`-separated path relative to
/// `workspace_root` to their full path. `workspace_root` must be canonical.
///
/// Ignore files apply with or without a git repository: `.gitignore` and `.ignore` in the
/// workspace and its parent directories, `.git/info/exclude`, and git's global excludes
/// file. [`EXCLUDED`] paths are never packaged. Hidden files are packaged in a git
/// repository unless ignored, and skipped outside one. Links to files are packaged as the
/// files they point to; links to directories are not followed. Fails with a usage error
/// when no file is left.
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
        let entry = entry.context("Can't list the workspace files")?;
        if !entry.path().is_file() {
            continue;
        }
        let relative_path = entry
            .path()
            .strip_prefix(workspace_root)
            .context("A packaged file is outside the workspace")?;
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

/// Write `files` to a zip archive in path order, under their workspace-relative names so
/// the workspace root is the archive root, and return their total size. Entries have a
/// fixed timestamp and keep only the executable bit of their permissions, so the same files
/// always make the same archive.
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

#[cfg(test)]
mod tests {
    use super::*;

    struct TempWorkspace(tempfile::TempDir);

    impl TempWorkspace {
        fn new() -> Self {
            Self(tempfile::tempdir().unwrap())
        }

        fn path(&self) -> &Path {
            self.0.path()
        }

        fn write(&self, rel_path: &str, contents: &str) {
            let path = self.path().join(rel_path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }

        fn git_init(&self, rel_path: &str) {
            self.write(&format!("{rel_path}/.git/HEAD"), "ref: refs/heads/main\n");
        }

        fn files_under(&self, rel_root: &str) -> Vec<String> {
            let root = self.path().join(rel_root).canonicalize().unwrap();
            list_workspace_files(&root).unwrap().into_keys().collect()
        }

        fn files(&self) -> Vec<String> {
            self.files_under(".")
        }

        fn digest(&self) -> String {
            let root = self.path().canonicalize().unwrap();
            source_digest(&list_workspace_files(&root).unwrap()).unwrap()
        }

        fn archive(&self, output: &Path) -> Vec<u8> {
            let root = self.path().canonicalize().unwrap();
            let files = list_workspace_files(&root).unwrap();
            write_zip_archive(&files, File::create(output).unwrap()).unwrap();
            std::fs::read(output).unwrap()
        }
    }

    #[test]
    fn given_the_same_files_written_in_another_order_when_digesting_then_the_digest_is_the_same() {
        let first = TempWorkspace::new();
        first.write("Cargo.toml", "[package]");
        first.write("src/main.rs", "fn main() {}");
        let second = TempWorkspace::new();
        second.write("src/main.rs", "fn main() {}");
        second.write("Cargo.toml", "[package]");

        let (first, second) = (first.digest(), second.digest());

        assert_eq!(first, second);
    }

    #[test]
    fn given_an_edited_file_when_digesting_then_the_digest_changes() {
        let workspace = TempWorkspace::new();
        workspace.write("src/main.rs", "fn main() {}");
        let original = workspace.digest();

        workspace.write("src/main.rs", "fn main() { println!(); }");

        assert_ne!(workspace.digest(), original);
    }

    #[test]
    fn given_a_renamed_file_when_digesting_then_the_digest_changes() {
        let workspace = TempWorkspace::new();
        workspace.write("src/main.rs", "fn main() {}");
        let original = workspace.digest();

        std::fs::rename(
            workspace.path().join("src/main.rs"),
            workspace.path().join("src/lib.rs"),
        )
        .unwrap();

        assert_ne!(workspace.digest(), original);
    }

    #[test]
    fn given_a_binary_package_when_digesting_it_then_it_shares_the_source_packages_digest() {
        let workspace = TempWorkspace::new();
        workspace.write(
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        workspace.write("src/main.rs", "fn main() {}");
        let metadata = cargo_metadata::MetadataCommand::new()
            .manifest_path(workspace.path().join("Cargo.toml"))
            .no_deps()
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .exec()
            .unwrap();
        let info = WorkspaceInfo {
            workspace_name: "demo".to_string(),
            workspace_root: metadata.workspace_root.clone().into_std_path_buf(),
            metadata,
        };
        let source = package_workspace(&info, Arc::new(())).unwrap();

        let binary_digest = workspace_digest(&info).unwrap();

        assert_eq!(binary_digest, source.digest);
    }

    #[test]
    fn given_build_output_when_listing_the_files_to_package_then_it_is_left_out() {
        let workspace = TempWorkspace::new();
        workspace.write("src/main.rs", "fn main() {}");
        workspace.write("target/release/trainer", "binary");

        let files = workspace.files();

        assert_eq!(files, ["src/main.rs"]);
    }

    #[test]
    fn given_a_gitignore_outside_a_repository_when_listing_the_files_to_package_then_it_applies() {
        let workspace = TempWorkspace::new();
        workspace.write("src/main.rs", "fn main() {}");
        workspace.write("train.log", "epoch 1");
        workspace.write(".gitignore", "*.log\n");

        let files = workspace.files();

        assert_eq!(files, ["src/main.rs"]);
    }

    #[test]
    fn given_an_ignore_file_that_reincludes_a_file_when_listing_the_files_to_package_then_only_that_file_is_kept()
     {
        let workspace = TempWorkspace::new();
        workspace.write("data/.ignore", "*.csv\n!keep.csv\n");
        workspace.write("data/train.csv", "1,2");
        workspace.write("data/keep.csv", "3,4");

        let files = workspace.files();

        assert_eq!(files, ["data/keep.csv"]);
    }

    #[test]
    fn given_a_repository_exclude_file_when_listing_the_files_to_package_then_it_applies() {
        let workspace = TempWorkspace::new();
        workspace.git_init(".");
        workspace.write(".git/info/exclude", "*.bak\n");
        workspace.write("src/main.rs", "fn main() {}");
        workspace.write("notes.bak", "draft");

        let files = workspace.files();

        assert_eq!(files, ["src/main.rs"]);
    }

    #[test]
    fn given_hidden_files_outside_a_repository_when_listing_the_files_to_package_then_they_are_left_out()
     {
        let workspace = TempWorkspace::new();
        workspace.write("src/main.rs", "fn main() {}");
        workspace.write(".cargo/config.toml", "[build]");

        let files = workspace.files();

        assert_eq!(files, ["src/main.rs"]);
    }

    #[test]
    fn given_hidden_files_in_a_repository_when_listing_the_files_to_package_then_they_are_packaged()
    {
        let workspace = TempWorkspace::new();
        workspace.git_init(".");
        workspace.write("src/main.rs", "fn main() {}");
        workspace.write(".cargo/config.toml", "[build]");

        let files = workspace.files();

        assert_eq!(files, [".cargo/config.toml", "src/main.rs"]);
    }

    #[test]
    fn given_a_workspace_in_a_repository_subdirectory_when_listing_the_files_to_package_then_only_its_files_are_named_from_its_root()
     {
        let repository = TempWorkspace::new();
        repository.git_init(".");
        repository.write(".gitignore", "*.log\n");
        repository.write("README.md", "outside the workspace");
        repository.write("trainer/Cargo.toml", "[package]");
        repository.write("trainer/train.log", "epoch 1");

        let files = repository.files_under("trainer");

        assert_eq!(files, ["Cargo.toml"]);
    }

    #[test]
    fn given_a_repository_nested_in_the_workspace_when_listing_the_files_to_package_then_its_files_are_packaged()
     {
        let workspace = TempWorkspace::new();
        workspace.git_init(".");
        workspace.git_init("vendor/lib");
        workspace.write("vendor/lib/lib.rs", "pub fn lib() {}");

        let files = workspace.files();

        assert_eq!(files, ["vendor/lib/lib.rs"]);
    }

    #[test]
    fn given_nothing_left_to_package_when_listing_the_files_then_it_is_a_usage_error() {
        let workspace = TempWorkspace::new();
        workspace.write(".ignore", "*\n");
        workspace.write("Cargo.toml", "[package]");

        let error = list_workspace_files(&workspace.path().canonicalize().unwrap()).unwrap_err();

        assert_eq!(crate::error::classify(&error), ErrorKind::Usage);
    }

    #[test]
    fn given_the_same_files_written_in_another_order_when_archiving_then_the_archive_is_the_same() {
        let output = tempfile::tempdir().unwrap();
        let first = TempWorkspace::new();
        first.write("Cargo.toml", "[package]");
        first.write("src/main.rs", "fn main() {}");
        let second = TempWorkspace::new();
        second.write("src/main.rs", "fn main() {}");
        second.write("Cargo.toml", "[package]");

        let first = first.archive(&output.path().join("first.zip"));
        let second = second.archive(&output.path().join("second.zip"));

        assert_eq!(first, second);
    }

    #[test]
    fn given_a_workspace_when_archiving_it_then_its_files_are_at_the_archive_root() {
        let output = tempfile::tempdir().unwrap();
        let workspace = TempWorkspace::new();
        workspace.write("Cargo.toml", "[package]");
        workspace.write("src/main.rs", "fn main() {}");

        let bytes = workspace.archive(&output.path().join("source.zip"));

        let archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        assert_eq!(
            archive.file_names().collect::<Vec<_>>(),
            ["Cargo.toml", "src/main.rs"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn given_an_executable_script_when_archiving_it_then_it_stays_executable() {
        use std::os::unix::fs::PermissionsExt;

        let output = tempfile::tempdir().unwrap();
        let workspace = TempWorkspace::new();
        workspace.write("build.sh", "#!/bin/sh\n");
        std::fs::set_permissions(
            workspace.path().join("build.sh"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();

        let bytes = workspace.archive(&output.path().join("source.zip"));

        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        assert_eq!(
            archive.by_name("build.sh").unwrap().unix_mode(),
            Some(0o100755)
        );
    }
}
