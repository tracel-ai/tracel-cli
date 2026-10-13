use anyhow::Context;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tracel_client::ClientError;

use crate::ui::{Terminal, counted};

use super::parts::PartUploadTask;

const UPLOAD_WORKER_COUNT: usize = 8;

/// Uploads one part's bytes to its presigned URL.
pub trait PartUploader: Send + Sync {
    fn upload(&self, url: &str, bytes: Vec<u8>) -> Result<(), ClientError>;
}

impl PartUploader for tracel_client::console::Client {
    fn upload(&self, url: &str, bytes: Vec<u8>) -> Result<(), ClientError> {
        self.upload_bytes_to_url(url, bytes)
    }
}

fn read_part_bytes(path: &Path, offset: u64, size: u64) -> anyhow::Result<Vec<u8>> {
    let mut file = File::open(path).with_context(|| format!("Can't open '{}'", path.display()))?;
    file.seek(SeekFrom::Start(offset))
        .with_context(|| format!("Can't seek in '{}'", path.display()))?;

    let mut buffer = vec![0u8; size as usize];
    file.read_exact(&mut buffer)
        .with_context(|| format!("Can't read '{}'", path.display()))?;

    Ok(buffer)
}

/// Split `tasks` into up to `worker_count` contiguous chunks, one per worker.
fn chunk_tasks(tasks: Vec<PartUploadTask>, worker_count: usize) -> Vec<Vec<PartUploadTask>> {
    if tasks.is_empty() {
        return Vec::new();
    }

    let chunk_size = tasks.len().div_ceil(worker_count);
    let mut iter = tasks.into_iter();
    let mut chunks = Vec::new();

    loop {
        let chunk: Vec<_> = iter.by_ref().take(chunk_size).collect();
        if chunk.is_empty() {
            break;
        }
        chunks.push(chunk);
    }

    chunks
}

/// Upload a worker's parts in order, counting each in `completed`, until one fails or
/// another worker's failure sets `cancelled`.
fn upload_chunk(
    chunk: Vec<PartUploadTask>,
    uploader: &impl PartUploader,
    cancelled: &AtomicBool,
    completed: &AtomicU64,
) -> anyhow::Result<()> {
    for task in chunk {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }

        let result =
            read_part_bytes(&task.absolute_path, task.offset, task.size_bytes).and_then(|bytes| {
                uploader
                    .upload(&task.url, bytes)
                    .map_err(|e| anyhow::anyhow!(e))
            });

        match result {
            Ok(()) => {
                completed.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                cancelled.store(true, Ordering::Relaxed);
                return Err(e.context(format!(
                    "Can't upload '{}' (part {})",
                    task.rel_path, task.part
                )));
            }
        }
    }

    Ok(())
}

/// Upload `tasks` with up to 8 workers under one progress bar, stopping at the first
/// failure.
pub fn upload_parts(
    uploader: &impl PartUploader,
    tasks: Vec<PartUploadTask>,
    terminal: &Terminal,
) -> anyhow::Result<()> {
    let total_parts = tasks.len();
    let files: BTreeSet<&str> = tasks.iter().map(|task| task.rel_path.as_str()).collect();
    let progress = terminal.progress(
        format!(
            "Uploading {}",
            counted(files.len() as u64, ["file", "files"])
        ),
        total_parts as u64,
    );
    let worker_count = UPLOAD_WORKER_COUNT.min(total_parts.max(1));
    let chunks = chunk_tasks(tasks, worker_count);

    let cancelled = AtomicBool::new(false);
    let completed = AtomicU64::new(0);

    let failure = std::thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                let cancelled = &cancelled;
                let completed = &completed;
                scope.spawn(move || upload_chunk(chunk, uploader, cancelled, completed))
            })
            .collect();

        let mut reported = 0u64;
        while handles.iter().any(|handle| !handle.is_finished()) {
            let done = completed.load(Ordering::Relaxed);
            if done > reported {
                progress.set_position(done);
                reported = done;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        // Join each worker here, so a panic becomes an error instead of unwinding.
        handles
            .into_iter()
            .filter_map(|handle| match handle.join() {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e),
                Err(_) => Some(anyhow::anyhow!("An upload worker stopped unexpectedly.")),
            })
            .next()
    });

    match failure {
        Some(error) => Err(error),
        None => {
            progress.clear();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::quiet_terminal;
    use std::fs;
    use std::sync::Mutex;

    /// Records the URLs it uploads to, and fails on `fail_url`.
    struct RecordingUploader {
        calls: Mutex<Vec<String>>,
        fail_url: Option<&'static str>,
    }

    impl RecordingUploader {
        fn new(fail_url: Option<&'static str>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail_url,
            }
        }
    }

    impl PartUploader for RecordingUploader {
        fn upload(&self, url: &str, _bytes: Vec<u8>) -> Result<(), ClientError> {
            self.calls.lock().unwrap().push(url.to_string());
            if self.fail_url == Some(url) {
                return Err(ClientError::NotFound);
            }
            Ok(())
        }
    }

    fn tasks(directory: &Path) -> Vec<PartUploadTask> {
        ["a", "b"]
            .into_iter()
            .map(|name| {
                let path = directory.join(format!("{name}.bin"));
                fs::write(&path, b"data").unwrap();
                PartUploadTask {
                    rel_path: format!("{name}.bin"),
                    absolute_path: path,
                    part: 1,
                    url: format!("https://example.com/{name}"),
                    offset: 0,
                    size_bytes: 4,
                }
            })
            .collect()
    }

    #[test]
    fn every_part_is_uploaded_once() {
        let directory = tempfile::tempdir().unwrap();
        let uploader = RecordingUploader::new(None);

        upload_parts(&uploader, tasks(directory.path()), &quiet_terminal()).unwrap();

        let mut calls = uploader.calls.into_inner().unwrap();
        calls.sort();
        assert_eq!(calls, ["https://example.com/a", "https://example.com/b"]);
    }

    #[test]
    fn a_failed_part_fails_the_upload_naming_the_part() {
        let directory = tempfile::tempdir().unwrap();
        let uploader = RecordingUploader::new(Some("https://example.com/b"));

        let error =
            upload_parts(&uploader, tasks(directory.path()), &quiet_terminal()).unwrap_err();

        assert!(error.to_string().contains("'b.bin' (part 1)"), "{error}");
    }
}
