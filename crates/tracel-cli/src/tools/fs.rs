use anyhow::Context;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// List the files under `directory` by their relative path, written with `/`.
pub fn collect_files(directory: &Path) -> anyhow::Result<BTreeMap<String, PathBuf>> {
    let base_dir = std::fs::canonicalize(directory)
        .with_context(|| format!("Can't find directory '{}'", directory.display()))?;

    if !base_dir.is_dir() {
        anyhow::bail!("'{}' isn't a directory.", base_dir.display());
    }

    let mut files = BTreeMap::new();

    for entry in walkdir::WalkDir::new(&base_dir).follow_links(false) {
        let entry =
            entry.with_context(|| format!("Can't list directory '{}'", base_dir.display()))?;

        if !entry.file_type().is_file() {
            continue;
        }

        let absolute = entry.path().to_path_buf();
        let rel_path = absolute
            .strip_prefix(&base_dir)
            .expect("walked entry should be under base_dir")
            .to_string_lossy()
            .replace('\\', "/");

        files.insert(rel_path, absolute);
    }

    if files.is_empty() {
        anyhow::bail!("No files in '{}'.", base_dir.display());
    }

    Ok(files)
}

/// A file's relative path, size and SHA-256 checksum.
pub struct FileMeta {
    pub rel_path: String,
    pub size_bytes: u64,
    pub checksum: String,
}

/// Hash a file with SHA-256, and measure it.
pub fn file_sha256_and_size(path: &Path) -> anyhow::Result<(String, u64)> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("Can't open '{}'", path.display()))?;

    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    let mut size = 0u64;

    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("Can't read '{}'", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }

    Ok((format!("{:x}", hasher.finalize()), size))
}

/// Measure and hash each of `files`.
pub fn build_file_specs(files: &BTreeMap<String, PathBuf>) -> anyhow::Result<Vec<FileMeta>> {
    let mut specs = Vec::with_capacity(files.len());

    for (rel_path, absolute_path) in files {
        let (checksum, size_bytes) = file_sha256_and_size(absolute_path)?;
        specs.push(FileMeta {
            rel_path: rel_path.clone(),
            size_bytes,
            checksum,
        });
    }

    Ok(specs)
}

/// Hash the `name:checksum` lines of `files`, sorted by name, one line per file, with
/// SHA-256.
pub fn manifest_digest<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let mut lines: Vec<(&str, &str)> = files.into_iter().collect();
    lines.sort_unstable();

    let mut hasher = Sha256::new();
    for (name, checksum) in lines {
        hasher.update(name.as_bytes());
        hasher.update(b":");
        hasher.update(checksum.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn given_a_nested_directory_when_collecting_its_files_then_each_is_named_by_its_relative_path()
    {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("nested")).unwrap();
        fs::write(directory.path().join("a.txt"), b"a").unwrap();
        fs::write(directory.path().join("nested").join("b.txt"), b"b").unwrap();

        let files = collect_files(directory.path()).unwrap();

        assert_eq!(files.keys().collect::<Vec<_>>(), ["a.txt", "nested/b.txt"]);
    }

    #[test]
    fn given_an_empty_directory_when_collecting_its_files_then_there_is_nothing_to_upload() {
        let directory = tempfile::tempdir().unwrap();

        let collected = collect_files(directory.path());

        assert!(collected.is_err());
    }

    #[test]
    fn given_a_file_when_describing_it_then_it_has_its_size_and_sha256() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hello.txt");
        fs::write(&path, b"hello world").unwrap();

        let specs = build_file_specs(&BTreeMap::from([("hello.txt".to_string(), path)])).unwrap();

        assert_eq!(specs[0].rel_path, "hello.txt");
        assert_eq!(specs[0].size_bytes, 11);
        assert_eq!(
            specs[0].checksum,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }
}
