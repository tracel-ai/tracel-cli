use serde::Serialize;
use std::path::{Path, PathBuf};
use std::{fs, io};

/// Project identity persisted to `tracel.toml` at the workspace root.
///
/// The on-disk keys are `namespace`/`project` to match the Tracel SDK's reader. The Rust
/// field names stay `owner`/`name` so call sites read `.owner` / `.name`.
#[derive(Serialize, Debug, Clone)]
pub struct TracelProject {
    #[serde(rename = "namespace")]
    pub owner: String,
    #[serde(rename = "project")]
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

    /// Load the project from `workspace_root/tracel.toml`, which must hold both keys, or
    /// `None` if the file does not exist.
    pub fn load(workspace_root: &Path) -> io::Result<Option<Self>> {
        let Some(TracelToml { namespace, project }) = TracelToml::load(workspace_root)? else {
            return Ok(None);
        };
        let missing = |key| invalid_data(format!("missing key `{key}`"));
        Ok(Some(Self {
            owner: namespace.ok_or_else(|| missing("namespace"))?,
            name: project.ok_or_else(|| missing("project"))?,
        }))
    }

    pub fn remove(workspace_root: &Path) -> io::Result<()> {
        fs::remove_file(Self::path(workspace_root))
    }
}

/// The keys of a `tracel.toml`. Either may be missing, for the environment to provide.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TracelToml {
    pub namespace: Option<String>,
    pub project: Option<String>,
}

impl TracelToml {
    /// Read `workspace_root/tracel.toml`, or `None` if the file does not exist.
    pub fn load(workspace_root: &Path) -> io::Result<Option<Self>> {
        match fs::read_to_string(TracelProject::path(workspace_root)) {
            Ok(contents) => Self::parse(&contents).map(Some).map_err(invalid_data),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Parse the contents of a `tracel.toml`, whose only keys are the strings `namespace`
    /// and `project`.
    pub fn parse(contents: &str) -> Result<Self, String> {
        let table: toml::Table = toml::from_str(contents).map_err(|error| error.to_string())?;
        let mut config = Self::default();
        for (key, value) in table {
            let slot = match key.as_str() {
                "namespace" => &mut config.namespace,
                "project" => &mut config.project,
                _ => {
                    return Err(format!(
                        "unknown key `{key}`; expected keys are `namespace` and `project`"
                    ));
                }
            };
            let toml::Value::String(value) = value else {
                return Err(format!("`{key}` must be a string"));
            };
            *slot = Some(value);
        }
        Ok(config)
    }
}

fn invalid_data(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_either_key() {
        for (contents, namespace, project) in [
            (
                "namespace = 'alice'\nproject = 'demo'",
                Some("alice"),
                Some("demo"),
            ),
            ("project = 'demo'", None, Some("demo")),
            ("namespace = 'alice'", Some("alice"), None),
            ("", None, None),
        ] {
            assert_eq!(
                TracelToml::parse(contents).unwrap(),
                TracelToml {
                    namespace: namespace.map(str::to_owned),
                    project: project.map(str::to_owned),
                }
            );
        }
    }

    #[test]
    fn parse_rejects_other_keys_naming_the_expected_ones() {
        for (contents, key) in [
            ("owner = 'alice'\nname = 'demo'", "name"),
            ("namespace = 'alice'\nname = 'demo'", "name"),
            ("owner = 'alice'\nproject = 'demo'", "owner"),
        ] {
            assert_eq!(
                TracelToml::parse(contents).unwrap_err(),
                format!("unknown key `{key}`; expected keys are `namespace` and `project`")
            );
        }
        assert_eq!(
            TracelToml::parse("namespace = 1").unwrap_err(),
            "`namespace` must be a string"
        );
    }

    #[test]
    fn load_requires_both_keys() {
        let dir = tempfile::tempdir().unwrap();
        assert!(TracelProject::load(dir.path()).unwrap().is_none());

        let project = TracelProject {
            owner: "alice".into(),
            name: "demo".into(),
        };
        project.save(dir.path()).unwrap();
        let loaded = TracelProject::load(dir.path()).unwrap().unwrap();
        assert_eq!(
            (loaded.owner.as_str(), loaded.name.as_str()),
            ("alice", "demo")
        );

        fs::write(TracelProject::path(dir.path()), "project = 'demo'").unwrap();
        let error = TracelProject::load(dir.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "missing key `namespace`");

        fs::write(
            TracelProject::path(dir.path()),
            "owner = 'alice'\nname = 'demo'",
        )
        .unwrap();
        let error = TracelProject::load(dir.path()).unwrap_err();
        assert!(error.to_string().contains("`namespace` and `project`"));
    }
}
