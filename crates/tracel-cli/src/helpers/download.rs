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
    use rstest::rstest;

    use super::*;
    use crate::error::classify;

    const HELLO_WORLD_SHA256: &str =
        "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";

    fn file(rel_path: &str, size_bytes: Option<u64>, checksum: Option<&str>) -> DownloadFile {
        DownloadFile {
            rel_path: rel_path.into(),
            url: String::new(),
            size_bytes,
            checksum: checksum.map(str::to_string),
        }
    }

    fn written(contents: &[u8]) -> HashingWriter<Vec<u8>> {
        let mut writer = HashingWriter::new(Vec::new());
        writer.write_all(contents).unwrap();
        writer
    }

    struct TwoBytesAtATime(Vec<u8>);

    impl Write for TwoBytesAtATime {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            let count = data.len().min(2);
            self.0.extend_from_slice(&data[..count]);
            Ok(count)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
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
    fn given_a_writer_that_takes_two_bytes_at_a_time_when_writing_then_only_written_bytes_are_hashed()
     {
        let mut writer = HashingWriter::new(TwoBytesAtATime(Vec::new()));

        writer.write_all(b"hello world").unwrap();

        assert_eq!(writer.inner.0, b"hello world");
        assert_eq!(writer.bytes, 11);
        assert_eq!(
            format!("{:x}", writer.hasher.finalize()),
            HELLO_WORLD_SHA256
        );
    }

    #[test]
    fn given_the_declared_size_and_checksum_when_verifying_a_download_then_it_passes() {
        let writer = written(b"hello world");
        let declared = file("hello", Some(11), Some(&HELLO_WORLD_SHA256.to_uppercase()));

        let verified = writer.verify(&declared);

        assert!(verified.is_ok());
    }

    #[rstest]
    #[case::none(None, None)]
    #[case::zero_and_empty(Some(0), Some(""))]
    fn given_no_declared_size_or_checksum_when_verifying_a_download_then_it_passes(
        #[case] size_bytes: Option<u64>,
        #[case] checksum: Option<&str>,
    ) {
        let writer = written(b"hello world");

        let verified = writer.verify(&file("hello", size_bytes, checksum));

        assert!(verified.is_ok());
    }

    #[test]
    fn given_another_size_when_verifying_a_download_then_it_fails() {
        let writer = written(b"hello world");

        let verified = writer.verify(&file("hello", Some(10), None));

        assert!(verified.is_err());
    }

    #[test]
    fn given_another_checksum_when_verifying_a_download_then_it_fails() {
        let writer = written(b"hello world");

        let verified = writer.verify(&file("hello", None, Some("0123abcd")));

        assert!(verified.is_err());
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
