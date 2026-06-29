use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::{fs, io};

/// Project identity persisted to `tracel.toml` at the workspace root.
///
/// The on-disk keys are `namespace`/`project` to match the Tracel SDK's reader;
/// `owner`/`name` are accepted as aliases for backwards compatibility. The Rust
/// field names stay `owner`/`name` so call sites read `.owner` / `.name`.
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct TracelProject {
    #[serde(rename = "namespace", alias = "owner")]
    pub owner: String,
    #[serde(rename = "project", alias = "name")]
    pub name: String,
}

impl TracelProject {
    pub const FILENAME: &'static str = "tracel.toml";

    /// Path to the `tracel.toml` for the given workspace root.
    pub fn path(workspace_root: &Path) -> PathBuf {
        workspace_root.join(Self::FILENAME)
    }

    pub fn save(&self, workspace_root: &Path) -> io::Result<()> {
        let contents =
            toml::to_string(self).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(Self::path(workspace_root), contents)
    }

    /// Load the project from `workspace_root/tracel.toml`, or `None` if the file
    /// does not exist.
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
