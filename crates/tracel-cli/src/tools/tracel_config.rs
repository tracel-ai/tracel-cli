use serde::Serialize;
use std::path::{Path, PathBuf};
use std::{fs, io};

/// A project on the Tracel Console, as `tracel.toml` at the workspace root names it with
/// the keys `namespace` and `project`, as the SDK reads them.
#[derive(Serialize, Debug, Clone)]
pub struct TracelProject {
    pub namespace: String,
    #[serde(rename = "project")]
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

    /// Load `tracel.toml` from `workspace_root`, which must hold both keys, or `None` when
    /// there is none.
    pub fn load(workspace_root: &Path) -> io::Result<Option<Self>> {
        let Some(TracelToml { namespace, project }) = TracelToml::load(workspace_root)? else {
            return Ok(None);
        };
        let missing = |key| invalid_data(format!("missing key `{key}`"));
        Ok(Some(Self {
            namespace: namespace.ok_or_else(|| missing("namespace"))?,
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
    fn given_namespace_and_project_when_parsing_tracel_toml_then_both_are_read() {
        let parsed = TracelToml::parse("namespace = 'alice'\nproject = 'demo'").unwrap();

        assert_eq!(
            parsed,
            TracelToml {
                namespace: Some("alice".into()),
                project: Some("demo".into()),
            }
        );
    }

    #[test]
    fn given_only_project_when_parsing_tracel_toml_then_namespace_is_left_to_the_environment() {
        let parsed = TracelToml::parse("project = 'demo'").unwrap();

        assert_eq!(parsed.namespace, None);
    }

    #[test]
    fn given_an_unknown_key_when_parsing_tracel_toml_then_the_error_names_it() {
        let error = TracelToml::parse("owner = 'alice'").unwrap_err();

        assert!(error.contains("`owner`"), "{error}");
    }

    #[test]
    fn given_a_key_that_is_not_a_string_when_parsing_tracel_toml_then_it_is_refused() {
        let error = TracelToml::parse("namespace = 1").unwrap_err();

        assert!(error.contains("`namespace`"), "{error}");
    }

    #[test]
    fn given_a_saved_project_when_loading_it_then_it_is_the_same_project() {
        let directory = tempfile::tempdir().unwrap();
        let project = TracelProject {
            namespace: "alice".into(),
            name: "demo".into(),
        };
        project.save(directory.path()).unwrap();

        let loaded = TracelProject::load(directory.path()).unwrap().unwrap();

        assert_eq!(
            (loaded.namespace.as_str(), loaded.name.as_str()),
            ("alice", "demo")
        );
    }

    #[test]
    fn given_no_tracel_toml_when_loading_the_project_then_there_is_none() {
        let directory = tempfile::tempdir().unwrap();

        let loaded = TracelProject::load(directory.path()).unwrap();

        assert!(loaded.is_none());
    }

    #[test]
    fn given_a_tracel_toml_without_namespace_when_loading_the_project_then_it_is_invalid() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(TracelProject::path(directory.path()), "project = 'demo'").unwrap();

        let error = TracelProject::load(directory.path()).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("`namespace`"), "{error}");
    }
}
