use std::fs::{self, File};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::Context;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tracel_client::console::Client;

use crate::error::{CliError, ErrorKind};
use crate::terminal::Terminal;

pub struct DownloadFile {
    pub rel_path: String,
    pub url: String,
    pub size_bytes: Option<u64>,
    pub checksum: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DownloadResult {
    pub rel_path: String,
    pub path: PathBuf,
    pub bytes: u64,
}

pub fn validate_rel_path(rel_path: &str) -> Result<(), CliError> {
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
        return Err(CliError::new(
            ErrorKind::Internal,
            format!("Unsafe download path '{rel_path}'."),
        ));
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

struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
    bytes: u64,
}

impl<W: Write> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes: 0,
        }
    }

    fn verify(&self, file: &DownloadFile) -> Result<(), CliError> {
        if let Some(size) = file.size_bytes.filter(|size| *size != 0) {
            if self.bytes != size {
                return Err(CliError::new(
                    ErrorKind::Internal,
                    format!(
                        "Size mismatch for '{}': expected {size} bytes, received {}.",
                        file.rel_path, self.bytes
                    ),
                ));
            }
        }
        if let Some(checksum) = file
            .checksum
            .as_deref()
            .filter(|checksum| !checksum.is_empty())
        {
            let actual = format!("{:x}", self.hasher.clone().finalize());
            if !actual.eq_ignore_ascii_case(checksum) {
                return Err(CliError::new(
                    ErrorKind::Internal,
                    format!(
                        "Checksum mismatch for '{}': expected {checksum}, received {actual}.",
                        file.rel_path
                    ),
                ));
            }
        }
        Ok(())
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        self.bytes += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub fn download_files(
    client: &Client,
    terminal: &Terminal,
    directory: &Path,
    files: &[DownloadFile],
    force: bool,
) -> anyhow::Result<Vec<DownloadResult>> {
    check_destinations(directory, files, force)?;
    let spinner = terminal.spinner();
    spinner.start(format!("Downloading {} file(s)...", files.len()));
    let results = download_each(client, directory, files)
        .inspect_err(|_| spinner.error("Download failed."))?;
    let bytes: u64 = results.iter().map(|result| result.bytes).sum();
    spinner.stop(format!(
        "Downloaded {} file(s), {bytes} bytes.",
        results.len()
    ));
    Ok(results)
}

fn download_each(
    client: &Client,
    directory: &Path,
    files: &[DownloadFile],
) -> anyhow::Result<Vec<DownloadResult>> {
    let mut results = Vec::with_capacity(files.len());
    for file in files {
        let path = directory.join(&file.rel_path);
        let parent = path.parent().expect("Validated relative file has a parent");
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create '{}'.", parent.display()))?;
        let mut name = path
            .file_name()
            .expect("Validated relative file has a name")
            .to_os_string();
        name.push(".part");
        let part = path.with_file_name(name);
        let output = File::create(&part)
            .with_context(|| format!("Failed to create '{}'.", part.display()))?;
        let result = (|| -> anyhow::Result<u64> {
            let mut writer = HashingWriter::new(output);
            client.download_from_url(&file.url, &mut writer)?;
            writer.flush()?;
            writer.verify(file)?;
            let bytes = writer.bytes;
            drop(writer);
            fs::rename(&part, &path)
                .with_context(|| format!("Failed to save '{}'.", path.display()))?;
            Ok(bytes)
        })();
        let bytes = result.inspect_err(|_| {
            let _ = fs::remove_file(&part);
        })?;
        results.push(DownloadResult {
            rel_path: file.rel_path.clone(),
            path,
            bytes,
        });
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_must_contain_only_normal_relative_components() {
        for path in ["a", "nested/weights.bin", "a-b/c_d"] {
            assert!(validate_rel_path(path).is_ok());
        }
        for path in [
            "",
            ".",
            "./a",
            "a/./b",
            "..",
            "../a",
            "a/../b",
            "/a",
            "C:\\a",
            "C:a",
            "C:/a",
            "\\\\server\\file",
            "\\a",
            "a\0b",
        ] {
            let error = validate_rel_path(path).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Internal);
            assert!(error.to_string().contains(path));
        }
    }

    fn file(size_bytes: Option<u64>, checksum: Option<String>) -> DownloadFile {
        DownloadFile {
            rel_path: "hello".into(),
            url: String::new(),
            size_bytes,
            checksum,
        }
    }

    #[test]
    fn writer_hashes_and_counts_only_written_bytes() {
        struct PartialWriter(Vec<u8>);
        impl Write for PartialWriter {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                let count = data.len().min(2);
                self.0.extend_from_slice(&data[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = HashingWriter::new(PartialWriter(Vec::new()));
        writer.write_all(b"hello world").unwrap();
        assert_eq!(writer.bytes, 11);
        assert_eq!(writer.inner.0, b"hello world");
        let checksum = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(
            writer
                .verify(&file(Some(11), Some(checksum.to_uppercase())))
                .is_ok()
        );
        assert!(writer.verify(&file(Some(0), Some(String::new()))).is_ok());
        assert!(writer.verify(&file(None, None)).is_ok());
        assert_eq!(
            writer.verify(&file(Some(10), None)).unwrap_err().kind,
            ErrorKind::Internal
        );
        assert_eq!(
            writer
                .verify(&file(None, Some("bad".into())))
                .unwrap_err()
                .kind,
            ErrorKind::Internal
        );
    }

    #[test]
    fn preflight_validates_all_paths_before_checking_all_conflicts() {
        let directory = std::env::temp_dir().join(format!(
            "tracel-download-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("hello"), b"existing").unwrap();
        let mut files = vec![
            DownloadFile {
                rel_path: "new".into(),
                ..file(None, None)
            },
            file(None, None),
        ];
        let error = check_destinations(&directory, &files, false).unwrap_err();
        assert_eq!(crate::error::classify(&error), ErrorKind::Conflict);
        assert!(error.to_string().contains("hello"));
        assert!(!directory.join("new").exists());
        assert!(check_destinations(&directory, &files, true).is_ok());
        files.push(DownloadFile {
            rel_path: "../unsafe".into(),
            ..file(None, None)
        });
        assert_eq!(
            crate::error::classify(&check_destinations(&directory, &files, false).unwrap_err()),
            ErrorKind::Internal
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
