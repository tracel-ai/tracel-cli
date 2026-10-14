//! Packaging the workspace's source as one compressed archive, without what git ignores.

use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use sha2::Digest as _;
use sha2::Sha256;

use crate::tools::event::Reporter;
use crate::ui::{counted, size};

/// A packaged archive: where it is, its SHA-256 checksum and its size.
#[derive(Debug)]
pub struct ArchiveMetadata {
    pub path: PathBuf,
    pub checksum: String,
    pub size: u64,
}

/// A step of packaging, shown as it happens.
pub struct PackageEvent {
    pub message: String,
}

type PackageEventReporter = dyn Reporter<PackageEvent>;

/// Package the workspace as one compressed archive, without what git ignores.
pub fn package_workspace(
    workspace_name: &str,
    event_reporter: Arc<PackageEventReporter>,
) -> anyhow::Result<ArchiveMetadata> {
    let metadata = cargo_metadata::MetadataCommand::new().exec()?;
    let workspace_root = metadata
        .workspace_root
        .canonicalize()
        .context("Can't find the workspace root")?;

    event_reporter.report_event(PackageEvent {
        message: "Listing files".to_string(),
    });
    let files = list_workspace_files(&workspace_root)?;

    event_reporter.report_event(PackageEvent {
        message: format!(
            "Compressing {}",
            counted(files.len() as u64, ["file", "files"])
        ),
    });

    // The archive goes in the target directory, which it leaves out.
    let output_dir = metadata
        .target_directory
        .as_std_path()
        .join("tracel")
        .join("package");
    let archive_path = output_dir.join(workspace_name);

    std::fs::create_dir_all(&output_dir)?;

    let archive_file = File::create(&archive_path)?;

    let uncompressed_size =
        create_workspace_archive(&workspace_root, &files, &archive_file, workspace_name)?;

    event_reporter.report_event(PackageEvent {
        message: format!("Computing the checksum of {}", size(uncompressed_size)),
    });
    let archive_data = std::fs::read(&archive_path)?;
    Ok(ArchiveMetadata {
        path: archive_path,
        checksum: format!("{:x}", Sha256::digest(&archive_data)),
        size: archive_data.len() as u64,
    })
}

/// List the workspace files to package, without what git ignores.
fn list_workspace_files(workspace_root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let git_repo = discover_gix_repo(workspace_root)?;

    let mut exclude_builder = ignore::gitignore::GitignoreBuilder::new(workspace_root);

    // Without git, leave out hidden files.
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

        !ignore_exclude
            .matched_path_or_any_parents(relative_path, is_dir)
            .is_ignore()
    };

    if let Some(repo) = git_repo {
        list_files_gix(workspace_root, &repo, &filter)
    } else {
        let mut files = Vec::new();
        list_files_walk(workspace_root, &mut files, &filter)?;
        Ok(files)
    }
}

/// Write `files` to `dst` as a tar.gz archive, each under `{package_prefix}/` as in
/// cargo's crate archives, and return their total size.
fn create_workspace_archive(
    workspace_root: &Path,
    files: &[PathBuf],
    dst: &File,
    package_prefix: &str,
) -> anyhow::Result<u64> {
    let encoder = flate2::GzBuilder::new().write(dst, flate2::Compression::best());

    let mut ar = tar::Builder::new(encoder);
    let mut uncompressed_size: u64 = 0;

    for file_path in files {
        let relative_path = file_path
            .strip_prefix(workspace_root)
            .context("A packaged file is outside the workspace")?;

        if file_path.is_file() {
            let mut file = File::open(file_path)?;
            let metadata = file.metadata()?;

            let mut header = tar::Header::new_gnu();
            header.set_metadata_in_mode(&metadata, tar::HeaderMode::Deterministic);
            header.set_cksum();

            let prefixed_path = PathBuf::from(package_prefix).join(relative_path);
            ar.append_data(&mut header, &prefixed_path, &mut file)?;
            uncompressed_size += metadata.len();
        }
    }

    let encoder = ar.into_inner()?;
    encoder.finish()?;

    Ok(uncompressed_size)
}

/// Find the git repository whose work tree holds `root`, if any.
fn discover_gix_repo(root: &Path) -> anyhow::Result<Option<gix::Repository>> {
    let repo = match gix::ThreadSafeRepository::discover(root) {
        Ok(repo) => repo.to_thread_local(),
        Err(_) => return Ok(None),
    };

    let repo_root = repo.workdir().ok_or_else(|| {
        anyhow::format_err!("The git repository at {} is bare.", repo.path().display())
    })?;

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

/// List the files git tracks or doesn't ignore, through nested repositories.
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

/// List the files under `path` that `filter` keeps, without git.
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
