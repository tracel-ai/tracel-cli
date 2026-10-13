use std::fs::{self, File};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::Context;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tracel_client::console::Client;

use crate::error::{CliError, ErrorKind};
use crate::ui::{Terminal, counted, short_digest};

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

    fn verify(&self, file: &DownloadFile) -> anyhow::Result<()> {
        if let Some(size) = file.size_bytes.filter(|size| *size != 0) {
            if self.bytes != size {
                anyhow::bail!(
                    "Size mismatch for '{}': expected {size} bytes, received {}.",
                    file.rel_path,
                    self.bytes
                );
            }
        }
        if let Some(checksum) = file
            .checksum
            .as_deref()
            .filter(|checksum| !checksum.is_empty())
        {
            let actual = format!("{:x}", self.hasher.clone().finalize());
            if !actual.eq_ignore_ascii_case(checksum) {
                anyhow::bail!(
                    "Checksum mismatch for '{}': expected {}, received {}.",
                    file.rel_path,
                    short_digest(checksum),
                    short_digest(&actual)
                );
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

/// Download `files` into `directory` under a spinner, after checking every path and,
/// unless `force`, that none of them exists.
pub fn download_files(
    client: &Client,
    terminal: &Terminal,
    directory: &Path,
    files: &[DownloadFile],
    force: bool,
) -> anyhow::Result<Vec<DownloadResult>> {
    check_destinations(directory, files, force)?;
    let spinner = terminal.spinner(format!(
        "Downloading {}",
        counted(files.len() as u64, ["file", "files"])
    ));
    let results = download_each(client, directory, files)?;
    spinner.clear();
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
            .with_context(|| format!("Can't create '{}'", parent.display()))?;
        let mut name = path
            .file_name()
            .expect("Validated relative file has a name")
            .to_os_string();
        name.push(".part");
        let part = path.with_file_name(name);
        let output =
            File::create(&part).with_context(|| format!("Can't create '{}'", part.display()))?;
        let result = (|| -> anyhow::Result<u64> {
            let mut writer = HashingWriter::new(output);
            client.download_from_url(&file.url, &mut writer)?;
            writer.flush()?;
            writer.verify(file)?;
            let bytes = writer.bytes;
            drop(writer);
            fs::rename(&part, &path).with_context(|| format!("Can't save '{}'", path.display()))?;
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
    use crate::error::classify;

    #[test]
    fn a_download_path_stays_inside_its_directory() {
        for (path, safe) in [
            ("a", true),
            ("nested/weights.bin", true),
            ("a-b/c_d", true),
            ("", false),
            (".", false),
            ("./a", false),
            ("a/./b", false),
            ("..", false),
            ("../a", false),
            ("a/../b", false),
            ("/a", false),
            ("C:\\a", false),
            ("C:a", false),
            ("C:/a", false),
            ("\\\\server\\file", false),
            ("\\a", false),
            ("a\0b", false),
        ] {
            match validate_rel_path(path) {
                Ok(()) => assert!(safe, "{path:?}"),
                Err(error) => {
                    assert!(!safe, "{path:?}");
                    assert_eq!(classify(&error), ErrorKind::Internal, "{path:?}");
                    assert!(error.to_string().contains(path), "{path:?}");
                }
            }
        }
    }

    fn file(size_bytes: Option<u64>, checksum: Option<&str>) -> DownloadFile {
        DownloadFile {
            rel_path: "hello".into(),
            url: String::new(),
            size_bytes,
            checksum: checksum.map(str::to_string),
        }
    }

    /// A writer that takes two bytes at a time, as a socket may.
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

    #[test]
    fn the_writer_hashes_only_the_bytes_written() {
        let mut writer = HashingWriter::new(PartialWriter(Vec::new()));

        writer.write_all(b"hello world").unwrap();

        assert_eq!(writer.bytes, 11);
        assert_eq!(writer.inner.0, b"hello world");
        assert_eq!(
            format!("{:x}", writer.hasher.clone().finalize()),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn a_download_must_match_its_declared_size_and_checksum() {
        let checksum = "B94D27B9934D3E08A52E52D7DA7DABFAC484EFE37A5380EE9088F7ACE2EFCDE9";
        let mut writer = HashingWriter::new(Vec::new());
        writer.write_all(b"hello world").unwrap();
        for (declared, matches) in [
            (file(Some(11), Some(checksum)), true),
            (file(Some(0), Some("")), true),
            (file(None, None), true),
            (file(Some(10), None), false),
            (file(None, Some("bad")), false),
        ] {
            let case = (declared.size_bytes, declared.checksum.clone());
            assert_eq!(writer.verify(&declared).is_ok(), matches, "{case:?}");
        }
    }

    #[test]
    fn every_path_is_checked_before_any_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("hello"), b"existing").unwrap();
        let new = || DownloadFile {
            rel_path: "new".into(),
            ..file(None, None)
        };
        let unsafe_path = DownloadFile {
            rel_path: "../unsafe".into(),
            ..file(None, None)
        };

        let existing = check_destinations(directory.path(), &[new(), file(None, None)], false);
        let forced = check_destinations(directory.path(), &[new(), file(None, None)], true);
        let unsafe_first =
            check_destinations(directory.path(), &[file(None, None), unsafe_path], false);

        assert_eq!(classify(&existing.unwrap_err()), ErrorKind::Conflict);
        assert!(forced.is_ok());
        assert_eq!(classify(&unsafe_first.unwrap_err()), ErrorKind::Internal);
        assert!(!directory.path().join("new").exists());
    }
}
