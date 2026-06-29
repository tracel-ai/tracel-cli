use std::path::PathBuf;
use std::{fs, io};

use crate::entity::projects::burn_dir::project::BurnCentralProject;

pub mod project;

pub struct BurnDir {
    root: PathBuf,
}

impl BurnDir {
    pub fn new(root: PathBuf) -> Self {
        BurnDir { root }
    }

    pub fn init(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.root)?;
        fs::write(self.root.join(".gitignore"), "*\n!project.toml")?;
        Ok(())
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.root.join("artifacts")
    }

    pub fn load_project(&self) -> io::Result<Option<BurnCentralProject>> {
        BurnCentralProject::load(&self.root)
            .map(Some)
            .or_else(|err| {
                if err.kind() == io::ErrorKind::NotFound {
                    Ok(None)
                } else {
                    Err(err)
                }
            })
    }

    pub fn save_project(&self, project: &BurnCentralProject) -> io::Result<()> {
        project.save(&self.root)
    }
    pub fn root(&self) -> &PathBuf {
        &self.root
    }
}
