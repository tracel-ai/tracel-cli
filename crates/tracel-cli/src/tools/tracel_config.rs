use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::{fs, io};

/// A project on the Tracel Console, as `tracel.toml` at the workspace root names it.
/// The file's keys are `namespace` and `project`, as the SDK reads them, and the older
/// `owner` and `name` still read.
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct TracelProject {
    #[serde(alias = "owner")]
    pub namespace: String,
    #[serde(rename = "project", alias = "name")]
    pub name: String,
}

impl TracelProject {
    const FILENAME: &'static str = "tracel.toml";

    /// Locate `tracel.toml` in `workspace_root`.
    pub fn path(workspace_root: &Path) -> PathBuf {
        workspace_root.join(Self::FILENAME)
    }

    pub fn save(&self, workspace_root: &Path) -> io::Result<()> {
        let contents =
            toml::to_string(self).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(Self::path(workspace_root), contents)
    }

    /// Load `tracel.toml` from `workspace_root`, or `None` when there is none.
    pub fn load(workspace_root: &Path) -> io::Result<Option<Self>> {
        let path = Self::path(workspace_root);
        if !path.exists() {
            return Ok(None);
        }
        let contents = fs::read_to_string(path)?;
        let project =
            toml::from_str(&contents).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(Some(project))
    }

    pub fn remove(workspace_root: &Path) -> io::Result<()> {
        fs::remove_file(Self::path(workspace_root))
    }
}
