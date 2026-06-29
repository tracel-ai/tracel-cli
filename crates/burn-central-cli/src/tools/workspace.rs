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
                format!(
                    "Cargo.toml not found at specified path '{}'",
                    manifest_path.display()
                ),
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
                        "Failed to load cargo metadata for manifest at '{}': {}",
                        manifest_path.display(),
                        e
                    ),
                    ErrorKind::Parsing,
                    Some(anyhow::anyhow!(e)),
                )
            })?;

        let workspace_root = metadata.workspace_root.clone().into_std_path_buf();

        // Determine workspace name from workspace Cargo.toml
        let workspace_toml_path = workspace_root.join("Cargo.toml");
        if !workspace_toml_path.exists() {
            return Err(ProjectContextError::new(
                format!(
                    "Cargo.toml not found at workspace root '{}'. This is not a valid cargo project.",
                    workspace_toml_path.display()
                ),
                ErrorKind::ManifestNotFound,
                None,
            ));
        }

        let toml_str = std::fs::read_to_string(&workspace_toml_path).map_err(|e| {
            ProjectContextError::new(
                format!(
                    "Failed to read Cargo.toml at '{}': {}",
                    workspace_toml_path.display(),
                    e
                ),
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

    pub fn get_ws_root(&self) -> PathBuf {
        self.metadata.workspace_root.clone().into_std_path_buf()
    }

    pub fn get_manifest_path(&self) -> PathBuf {
        self.workspace_root.join(PathBuf::from("Cargo.toml"))
    }
}

/// Extract workspace name from TOML content
/// Returns None if:
/// - TOML content cannot be parsed
/// - Neither workspace.package.name nor package.name exists
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
    use super::*;

    #[test]
    fn test_extract_workspace_name_from_workspace_package() {
        let toml_content = r#"
[workspace]
members = ["crate1", "crate2"]

[workspace.package]
name = "my-awesome-workspace"
version = "0.1.0"
"#;
        let result = extract_workspace_name_from_toml(toml_content);
        assert_eq!(result, Some("my-awesome-workspace".to_string()));
    }

    #[test]
    fn test_extract_workspace_name_from_package() {
        let toml_content = r#"
[package]
name = "single-crate-project"
version = "0.1.0"
"#;
        let result = extract_workspace_name_from_toml(toml_content);
        assert_eq!(result, Some("single-crate-project".to_string()));
    }

    #[test]
    fn test_extract_workspace_name_prefers_workspace_over_package() {
        let toml_content = r#"
[workspace]
members = ["crate1"]

[workspace.package]
name = "workspace-name"

[package]
name = "package-name"
"#;
        let result = extract_workspace_name_from_toml(toml_content);
        assert_eq!(result, Some("workspace-name".to_string()));
    }

    #[test]
    fn test_extract_workspace_name_returns_none_when_no_name() {
        let toml_content = r#"
[workspace]
members = ["crate1", "crate2"]
"#;
        let result = extract_workspace_name_from_toml(toml_content);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_workspace_name_returns_none_for_invalid_toml() {
        let toml_content = "this is not valid toml { [ }";
        let result = extract_workspace_name_from_toml(toml_content);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_workspace_name_returns_none_for_empty_toml() {
        let toml_content = "";
        let result = extract_workspace_name_from_toml(toml_content);
        assert_eq!(result, None);
    }
}
