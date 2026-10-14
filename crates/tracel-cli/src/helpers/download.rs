use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use tracel_artifact::ReqwestTransferClient;
use tracel_artifact::bundle::FsBundle;
use tracel_artifact::download::{
    ArtifactDownloadFile, download_artifacts_to_sink_with_client_and_observer,
};

use super::transfer::{TransferProgress, download_failure};
use crate::error::{CliError, ErrorKind};
use crate::ui::{Terminal, counted};

/// A file to download into a directory, with the size and checksum to check it against.
pub struct DownloadFile {
    pub rel_path: String,
    pub url: String,
    pub size_bytes: Option<u64>,
    pub checksum: Option<String>,
}

/// A downloaded file: where it went and how many bytes it has.
#[derive(Debug, Serialize)]
pub struct DownloadResult {
    pub rel_path: String,
    pub path: PathBuf,
    pub bytes: u64,
}

/// Fail unless `rel_path` is a plain relative path that stays inside its directory.
pub fn validate_rel_path(rel_path: &str) -> anyhow::Result<()> {
    let path = Path::new(rel_path);
    let windows_prefix = rel_path.as_bytes().get(1) == Some(&b':');
    if rel_path.is_empty()
        || rel_path.contains(['\\', '\0'])
        || windows_prefix
        || rel_path.split('/').any(|part| part == ".")
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        anyhow::bail!("Unsafe download path '{rel_path}'.");
    }
    Ok(())
}

fn check_destinations(directory: &Path, files: &[DownloadFile], force: bool) -> anyhow::Result<()> {
    for file in files {
        validate_rel_path(&file.rel_path)?;
    }
    for file in files {
        let path = directory.join(&file.rel_path);
        if !force && path.try_exists()? {
            return Err(CliError::new(
                ErrorKind::Conflict,
                format!("Destination '{}' already exists.", path.display()),
            )
            .with_hint("Pass --force to overwrite.")
            .into());
        }
    }
    Ok(())
}

/// Download `files` into `directory` under a progress bar, after checking every path and,
/// unless `force`, that none of them exists.
///
/// The files are downloaded beside `directory`, and checked against their size and
/// checksum, before any of them moves into it, so a failed download leaves nothing in it.
pub fn download_files(
    terminal: &Terminal,
    directory: &Path,
    files: &[DownloadFile],
    force: bool,
) -> anyhow::Result<Vec<DownloadResult>> {
    check_destinations(directory, files, force)?;
    let staging_parent = match directory.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let mut staging = FsBundle::temp_in(staging_parent, ".tracel-download-")?;
    let downloads: Vec<ArtifactDownloadFile> = files
        .iter()
        .map(|file| ArtifactDownloadFile {
            rel_path: file.rel_path.clone(),
            url: file.url.clone(),
            size_bytes: file.size_bytes,
            checksum: file.checksum.clone(),
        })
        .collect();
    let total = files.iter().filter_map(|file| file.size_bytes).sum();
    let message = format!(
        "Downloading {}",
        counted(files.len() as u64, ["file", "files"])
    );
    let mut progress = TransferProgress::new(terminal.transfer(message, total));
    download_artifacts_to_sink_with_client_and_observer(
        &ReqwestTransferClient::new(),
        &mut staging,
        &downloads,
        &mut progress,
    )
    .map_err(download_failure)?;
    progress.finish();
    staging.move_into(directory)?;

    Ok(files
        .iter()
        .map(|file| DownloadResult {
            rel_path: file.rel_path.clone(),
            path: directory.join(&file.rel_path),
            bytes: progress.moved(&file.rel_path).unwrap_or_default(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rstest::rstest;

    use super::*;
    use crate::error::classify;

    fn file(rel_path: &str, size_bytes: Option<u64>, checksum: Option<&str>) -> DownloadFile {
        DownloadFile {
            rel_path: rel_path.into(),
            url: String::new(),
            size_bytes,
            checksum: checksum.map(str::to_string),
        }
    }

    #[rstest]
    #[case::file("weights.bin")]
    #[case::nested("nested/weights.bin")]
    fn given_a_relative_path_when_validating_it_then_it_is_safe(#[case] rel_path: &str) {
        let validated = validate_rel_path(rel_path);

        assert!(validated.is_ok());
    }

    #[rstest]
    #[case::empty("")]
    #[case::current_directory("./a")]
    #[case::parent("../a")]
    #[case::inner_parent("a/../b")]
    #[case::absolute("/a")]
    #[case::windows_drive("C:a")]
    #[case::backslash(r"a\b")]
    #[case::nul("a\0b")]
    fn given_a_path_that_can_leave_its_directory_when_validating_it_then_it_is_refused(
        #[case] rel_path: &str,
    ) {
        let validated = validate_rel_path(rel_path);

        assert!(validated.is_err());
    }

    #[test]
    fn given_an_existing_file_when_checking_destinations_then_it_is_a_conflict() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("hello"), b"existing").unwrap();

        let error =
            check_destinations(directory.path(), &[file("hello", None, None)], false).unwrap_err();

        assert_eq!(classify(&error), ErrorKind::Conflict);
    }

    #[test]
    fn given_force_when_checking_destinations_then_an_existing_file_may_be_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("hello"), b"existing").unwrap();

        let checked = check_destinations(directory.path(), &[file("hello", None, None)], true);

        assert!(checked.is_ok());
    }

    #[test]
    fn given_an_unsafe_path_after_an_existing_file_when_checking_destinations_then_the_unsafe_path_fails_first()
     {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("hello"), b"existing").unwrap();
        let files = [file("hello", None, None), file("../escape", None, None)];

        let error = check_destinations(directory.path(), &files, false).unwrap_err();

        assert!(error.to_string().contains("'../escape'"), "{error}");
    }
}
