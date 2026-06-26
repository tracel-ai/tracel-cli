use std::path::PathBuf;
use std::{fs, io};

use crate::entity::projects::burn_dir::cache::CacheState;

pub mod cache;
pub mod project;

/// Scratch/cache directory for build artifacts, packaged crates and the cache
/// state. It lives under the cargo target directory (which is already
/// gitignored), so it is never committed and is not tied to an environment.
pub struct BurnDir {
    root: PathBuf,
}

impl BurnDir {
    pub fn new(root: PathBuf) -> Self {
        BurnDir { root }
    }

    pub fn init(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.root)?;
        Ok(())
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }

    pub fn crates_dir(&self) -> PathBuf {
        self.root.join("crates")
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.root.join("artifacts")
    }

    pub fn target_dir(&self) -> PathBuf {
        self.root.join("target")
    }

    pub fn load_cache(&self) -> io::Result<CacheState> {
        CacheState::load(&self.root)
    }

    pub fn save_cache(&self, cache: &CacheState) -> io::Result<()> {
        cache.save(&self.root)
    }

    pub fn root(&self) -> &PathBuf {
        &self.root
    }
}
