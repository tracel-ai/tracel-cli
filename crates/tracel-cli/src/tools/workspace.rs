use std::path::{Path, PathBuf};

use crate::tools::project_context::{ErrorKind, ProjectContextError};

pub struct WorkspaceInfo {
    pub workspace_name: String,
    pub workspace_root: PathBuf,
    pub metadata: cargo_metadata::Metadata,
}

impl WorkspaceInfo {
    pub fn load_from_path(manifest_path: &Path) -> Result<Self, ProjectContextError> {
        if !manifest_path.is_file() {
            return Err(ProjectContextError::new(
                format!("No Cargo.toml at '{}'", manifest_path.display()),
                ErrorKind::ManifestNotFound,
                None,
            ));
        }
        let metadata = cargo_metadata::MetadataCommand::new()
            .manifest_path(manifest_path)
            .no_deps()
            .exec()
            .map_err(|e| {
                ProjectContextError::new(
                    format!(
                        "Can't read the cargo metadata of '{}'",
                        manifest_path.display()
                    ),
                    ErrorKind::Parsing,
                    Some(anyhow::anyhow!(e)),
                )
            })?;

        let workspace_root = metadata.workspace_root.clone().into_std_path_buf();

        let workspace_toml_path = workspace_root.join("Cargo.toml");
        if !workspace_toml_path.exists() {
            return Err(ProjectContextError::new(
                format!("No Cargo.toml at '{}'", workspace_toml_path.display()),
                ErrorKind::ManifestNotFound,
                None,
            ));
        }

        let toml_str = std::fs::read_to_string(&workspace_toml_path).map_err(|e| {
            ProjectContextError::new(
                format!("Can't read '{}'", workspace_toml_path.display()),
                ErrorKind::Parsing,
                Some(anyhow::anyhow!(e)),
            )
        })?;

        let workspace_name = extract_workspace_name_from_toml(&toml_str).unwrap_or_else(|| {
            workspace_root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("workspace")
                .to_string()
        });

        Ok(WorkspaceInfo {
            workspace_name,
            workspace_root,
            metadata,
        })
    }

    pub fn get_manifest_path(&self) -> PathBuf {
        self.workspace_root.join(PathBuf::from("Cargo.toml"))
    }
}

/// Read the name in `[workspace.package]`, else in `[package]`.
fn extract_workspace_name_from_toml(toml_content: &str) -> Option<String> {
    let workspace_document = toml_content.parse::<toml::Value>().ok()?;

    workspace_document
        .get("workspace")
        .and_then(|ws| ws.get("package"))
        .and_then(|pkg| pkg.get("name"))
        .and_then(|name| name.as_str())
        .or_else(|| {
            workspace_document
                .get("package")
                .and_then(|pkg| pkg.get("name"))
                .and_then(|name| name.as_str())
        })
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn given_a_workspace_package_name_when_reading_the_manifest_then_it_names_the_workspace() {
        let manifest = r#"
[workspace]
members = ["trainer"]

[workspace.package]
name = "my-workspace"
"#;

        let name = extract_workspace_name_from_toml(manifest);

        assert_eq!(name.as_deref(), Some("my-workspace"));
    }

    #[test]
    fn given_a_single_package_when_reading_the_manifest_then_its_name_names_the_workspace() {
        let manifest = r#"
[package]
name = "trainer"
"#;

        let name = extract_workspace_name_from_toml(manifest);

        assert_eq!(name.as_deref(), Some("trainer"));
    }

    #[test]
    fn given_both_names_when_reading_the_manifest_then_the_workspace_package_name_wins() {
        let manifest = r#"
[workspace.package]
name = "my-workspace"

[package]
name = "trainer"
"#;

        let name = extract_workspace_name_from_toml(manifest);

        assert_eq!(name.as_deref(), Some("my-workspace"));
    }

    #[rstest]
    #[case::no_name("[workspace]\nmembers = [\"trainer\"]\n")]
    #[case::empty("")]
    #[case::invalid_toml("this is not valid toml { [ }")]
    fn given_a_manifest_without_a_name_when_reading_it_then_there_is_none(#[case] manifest: &str) {
        let name = extract_workspace_name_from_toml(manifest);

        assert_eq!(name, None);
    }
}
