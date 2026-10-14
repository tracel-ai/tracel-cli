//! Moving files to and from presigned URLs with tracel-artifact, under one progress bar of
//! the bytes moved.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use tracel_artifact::TransferObserver;
use tracel_artifact::download::DownloadError;
use tracel_artifact::upload::{
    MultipartUploadFile, MultipartUploadSource, UploadError,
    upload_bundle_multipart_with_client_and_observer,
};

use crate::error::{CliError, ErrorKind};
use crate::ui::{Progress, Terminal};

/// Upload `files`, keyed by the path each `plan` entry names, to the URLs of `plan`, under a
/// progress bar saying `message`.
pub fn upload_files(
    terminal: &Terminal,
    message: impl std::fmt::Display,
    files: &BTreeMap<String, PathBuf>,
    plan: &[MultipartUploadFile],
) -> anyhow::Result<()> {
    let total = plan
        .iter()
        .flat_map(|file| &file.parts)
        .map(|part| part.size_bytes)
        .sum();
    let mut progress = TransferProgress::new(terminal.transfer(message, total));
    upload_bundle_multipart_with_client_and_observer(
        &tracel_artifact::ReqwestTransferClient::new(),
        &LocalFiles(files),
        plan,
        &mut progress,
    )
    .map_err(upload_failure)?;
    progress.finish();
    Ok(())
}

/// Shows on one bar the bytes every file of a transfer has moved.
pub struct TransferProgress {
    progress: Progress,
    moved: HashMap<String, u64>,
}

impl TransferProgress {
    pub fn new(progress: Progress) -> Self {
        Self {
            progress,
            moved: HashMap::new(),
        }
    }

    /// The bytes `rel_path` moved, once it has.
    pub fn moved(&self, rel_path: &str) -> Option<u64> {
        self.moved.get(rel_path).copied()
    }

    /// Take the bar off the screen.
    pub fn finish(&self) {
        self.progress.clear();
    }

    fn record(&mut self, rel_path: &str, bytes: u64) {
        self.moved.insert(rel_path.to_string(), bytes);
        self.progress.set_position(self.moved.values().sum());
    }
}

impl TransferObserver for TransferProgress {
    fn file_progress(&mut self, rel_path: &str, transferred_bytes: u64) {
        self.record(rel_path, transferred_bytes);
    }

    fn file_completed(&mut self, rel_path: &str, transferred_bytes: u64) {
        self.record(rel_path, transferred_bytes);
    }
}

/// Local files, by the path the Tracel Console knows each by, read a part at a time.
struct LocalFiles<'a>(&'a BTreeMap<String, PathBuf>);

impl LocalFiles<'_> {
    fn path(&self, rel_path: &str) -> Result<&PathBuf, UploadError> {
        self.0.get(rel_path).ok_or_else(|| {
            UploadError::InvalidMultipart(format!(
                "The Tracel Console asked for '{rel_path}', which isn't being uploaded."
            ))
        })
    }
}

impl MultipartUploadSource for LocalFiles<'_> {
    fn file_len(&self, rel_path: &str) -> Result<u64, UploadError> {
        let path = self.path(rel_path)?;
        let metadata = std::fs::metadata(path).map_err(|error| UploadError::MultipartReader {
            rel_path: rel_path.to_string(),
            source: Box::new(error),
        })?;
        Ok(metadata.len())
    }

    fn open_part(
        &self,
        rel_path: &str,
        offset: u64,
        size: u64,
    ) -> Result<Box<dyn Read + Send>, UploadError> {
        let unreadable = |error: std::io::Error| UploadError::MultipartReader {
            rel_path: rel_path.to_string(),
            source: Box::new(error),
        };
        let mut file = File::open(self.path(rel_path)?).map_err(unreadable)?;
        file.seek(SeekFrom::Start(offset)).map_err(unreadable)?;
        Ok(Box::new(file.take(size)))
    }
}

/// A failed upload: `UNAVAILABLE` when a part couldn't be sent.
fn upload_failure(error: UploadError) -> anyhow::Error {
    match error {
        UploadError::Transfer {
            part_index,
            total_parts,
            rel_path,
            source,
        } => CliError::new(
            ErrorKind::Unavailable,
            format!("Can't upload '{rel_path}' (part {part_index} of {total_parts}): {source}"),
        )
        .into(),
        error => anyhow::Error::new(error).context("Can't upload the files"),
    }
}

/// A failed download: `UNAVAILABLE` when a file couldn't be fetched.
pub fn download_failure(error: DownloadError) -> anyhow::Error {
    match error {
        DownloadError::Transfer { rel_path, source } => CliError::new(
            ErrorKind::Unavailable,
            format!("Can't download '{rel_path}': {source}"),
        )
        .into(),
        error => anyhow::Error::new(error).context("Can't download the files"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_a_file_when_opening_a_part_then_it_reads_only_that_range() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("weights.bin");
        std::fs::write(&path, b"abcdef").unwrap();
        let files = BTreeMap::from([("weights.bin".to_string(), path)]);
        let mut part = String::new();

        LocalFiles(&files)
            .open_part("weights.bin", 2, 3)
            .unwrap()
            .read_to_string(&mut part)
            .unwrap();

        assert_eq!(part, "cde");
    }

    #[test]
    fn given_a_file_not_being_uploaded_when_the_console_asks_for_it_then_the_upload_is_refused() {
        let files = BTreeMap::new();

        let length = LocalFiles(&files).file_len("other.bin");

        assert!(matches!(length, Err(UploadError::InvalidMultipart(_))));
    }
}
