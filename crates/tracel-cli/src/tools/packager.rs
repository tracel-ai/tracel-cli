//! Workspace packaging for Tracel Console.
//!
//! Packages an entire workspace as a single compressed archive, respecting gitignore rules,
//! and computes the code version digest from the packaged files.

use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
};

use colored::Colorize;

use crate::tools::event::Reporter;
use crate::tools::fs::{file_sha256_and_size, manifest_digest};
use crate::tools::workspace::WorkspaceInfo;

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

/// Package the entire workspace as a single compressed archive with gitignore applied.
pub fn package_workspace(
    workspace: &WorkspaceInfo,
    event_reporter: Arc<PackageEventReporter>,
) -> anyhow::Result<ArchiveMetadata> {
    let workspace_root = workspace
        .workspace_root
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("Failed to canonicalize workspace root: {}", e))?;

    tracing::info!(
        "Packaging workspace at: {}",
        workspace_root.display().to_string().bold()
    );

    // List all files in the workspace respecting gitignore
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
    let archive_path = output_dir.join(&workspace.workspace_name);

    std::fs::create_dir_all(&output_dir)?;

    let archive_file = File::create(&archive_path)?;

    // Inside the archive, files live under a `{workspace_name}/` directory to match the standard
    // cargo crate format.
    let uncompressed_size =
        create_workspace_archive(&files, &archive_file, &workspace.workspace_name)?;

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
/// of the packaged files, with `/`-separated relative paths (see [`manifest_digest`]).
fn source_digest(files: &BTreeMap<PathBuf, PathBuf>) -> anyhow::Result<String> {
    let mut lines = Vec::with_capacity(files.len());
    for (relative_path, file_path) in files {
        let name = relative_path
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let (checksum, _) = file_sha256_and_size(file_path)?;
        lines.push((name, checksum));
    }
    Ok(manifest_digest(lines.iter().map(|(name, checksum)| {
        (name.as_str(), checksum.as_str())
    })))
}

/// Lists the files to package, respecting gitignore rules, as a map from their path relative
/// to `workspace_root` to their full path.
fn list_workspace_files(workspace_root: &Path) -> anyhow::Result<BTreeMap<PathBuf, PathBuf>> {
    let git_repo = discover_gix_repo(workspace_root)?;

    if let Some(ref repo) = git_repo {
        tracing::info!(
            "Git repository found at {}",
            repo.path().display().to_string().bold()
        );
    }

    // Build gitignore matcher
    let mut exclude_builder = ignore::gitignore::GitignoreBuilder::new(workspace_root);

    // Add default excludes if not using git
    if git_repo.is_none() {
        exclude_builder.add_line(None, ".*")?;
    }

    exclude_builder.add_line(None, "target/")?;

    let ignore_exclude = exclude_builder.build()?;

    let filter = |path: &Path, is_dir: bool| {
        let Ok(relative_path) = path.strip_prefix(workspace_root) else {
            return false;
        };

        if let Some(first_component) = relative_path.components().next() {
            let component_str = first_component.as_os_str().to_string_lossy();
            if component_str == "target" {
                return false;
            }
        }

        // Check gitignore rules
        !ignore_exclude
            .matched_path_or_any_parents(relative_path, is_dir)
            .is_ignore()
    };

    // Use git if available, otherwise walk the filesystem
    let paths = if let Some(repo) = git_repo {
        list_files_gix(workspace_root, &repo, &filter)?
    } else {
        let mut paths = Vec::new();
        list_files_walk(workspace_root, &mut paths, &filter)?;
        paths
    };

    let mut files = BTreeMap::new();
    for path in paths {
        if !path.is_file() {
            continue;
        }
        let relative_path = path
            .strip_prefix(workspace_root)
            .map_err(|e| anyhow::anyhow!("Failed to strip workspace root prefix: {}", e))?;
        files.insert(relative_path.to_path_buf(), path);
    }
    Ok(files)
}

/// Creates a compressed tar.gz archive of `files`, in path order.
///
/// All files are prefixed with `{package_prefix}/` to match the standard cargo crate format.
fn create_workspace_archive(
    files: &BTreeMap<PathBuf, PathBuf>,
    dst: &File,
    package_prefix: &str,
) -> anyhow::Result<u64> {
    let encoder = flate2::GzBuilder::new().write(dst, flate2::Compression::best());

    let mut ar = tar::Builder::new(encoder);
    let mut uncompressed_size: u64 = 0;

    for (relative_path, file_path) in files {
        let mut file = File::open(file_path)?;
        let metadata = file.metadata()?;

        let mut header = tar::Header::new_gnu();
        header.set_metadata_in_mode(&metadata, tar::HeaderMode::Deterministic);

        let prefixed_path = Path::new(package_prefix).join(relative_path);
        ar.append_data(&mut header, &prefixed_path, &mut file)?;
        uncompressed_size += metadata.len();
    }

    let encoder = ar.into_inner()?;
    encoder.finish()?;

    Ok(uncompressed_size)
}

/// Discovers a git repository starting from the given path.
fn discover_gix_repo(root: &Path) -> anyhow::Result<Option<gix::Repository>> {
    let repo = match gix::ThreadSafeRepository::discover(root) {
        Ok(repo) => repo.to_thread_local(),
        Err(_) => return Ok(None),
    };

    let repo_root = repo.workdir().ok_or_else(|| {
        anyhow::format_err!(
            "Did not expect repo at {} to be bare",
            repo.path().display()
        )
    })?;

    // Verify the repository contains the workspace root
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let canon_repo_root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());

    if canon_root.starts_with(&canon_repo_root) {
        Ok(Some(repo))
    } else {
        Ok(None)
    }
}

/// Lists files using git to respect .gitignore rules.
fn list_files_gix(
    workspace_root: &Path,
    repo: &gix::Repository,
    filter: &impl Fn(&Path, bool) -> bool,
) -> anyhow::Result<Vec<PathBuf>> {
    let options = repo
        .dirwalk_options()?
        .emit_untracked(gix::dir::walk::EmissionMode::Matching)
        .emit_ignored(None)
        .emit_tracked(true)
        .recurse_repositories(false)
        .symlinks_to_directories_are_ignored_like_directories(true);

    let index = repo.index_or_empty()?;
    let mut files = Vec::new();

    for entry in repo.dirwalk_iter(index.clone(), None::<&str>, Default::default(), options)? {
        let entry = entry?;

        let file_path = workspace_root.join(gix::path::from_bstr(entry.entry.rela_path));
        let is_dir = file_path.is_dir();

        if filter(&file_path, is_dir) {
            if !is_dir {
                files.push(file_path);
            } else {
                // Recursively walk directories
                match gix::open(&file_path) {
                    Ok(sub_repo) => {
                        files.extend(list_files_gix(workspace_root, &sub_repo, filter)?);
                    }
                    Err(_) => {
                        list_files_walk(&file_path, &mut files, filter)?;
                    }
                }
            }
        }
    }

    Ok(files)
}

/// Lists files by walking the filesystem (fallback when git is not available).
fn list_files_walk(
    path: &Path,
    files: &mut Vec<PathBuf>,
    filter: &impl Fn(&Path, bool) -> bool,
) -> anyhow::Result<()> {
    if !path.is_dir() {
        return Ok(());
    }

    let walker = walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                filter(e.path(), true)
            } else {
                true
            }
        });

    for entry in walker {
        match entry {
            Ok(entry) => {
                let file_path = entry.path();

                if file_path.is_file() && filter(file_path, false) {
                    files.push(file_path.to_path_buf());
                }
            }
            Err(err) => match err.path() {
                Some(path) if !filter(path, path.is_dir()) => {}
                Some(path) => files.push(path.to_path_buf()),
                None => return Err(err.into()),
            },
        }
    }

    Ok(())
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

        fn digest(&self) -> String {
            let root = self.0.canonicalize().unwrap();
            source_digest(&list_workspace_files(&root).unwrap()).unwrap()
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
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
        let workspace = TempWorkspace::new("gitignored");
        gix::init(&workspace.0).unwrap();
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
