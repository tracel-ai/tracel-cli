use std::path::{Path, PathBuf};

use crate::entity::projects::tracel_project::TracelProject;

pub mod tracel_project;

#[derive(Debug)]
pub enum ErrorKind {
    ManifestNotFound,
    Parsing,
    ProjectInitialization,
    ProjectNotLinked,
    Unexpected,
}

#[derive(thiserror::Error, Debug)]
pub struct ProjectContextError {
    message: String,
    kind: ErrorKind,
    #[source]
    source: Option<anyhow::Error>,
}

impl ProjectContextError {
    pub fn new(message: String, kind: ErrorKind, source: Option<anyhow::Error>) -> Self {
        Self {
            message,
            kind,
            source,
        }
    }

    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }

    pub fn is_project_not_linked(&self) -> bool {
        matches!(self.kind, ErrorKind::ProjectNotLinked)
    }
}

impl std::fmt::Display for ProjectContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

pub struct ProjectContext {
    pub workspace_info: WorkspaceInfo,
    pub build_profile: String,
    pub project: TracelProject,
}

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

impl ProjectContext {
    pub fn load_workspace_info(manifest_path: &Path) -> Result<WorkspaceInfo, ProjectContextError> {
        WorkspaceInfo::load_from_path(manifest_path)
    }

    pub fn load(manifest_path: &Path) -> Result<Self, ProjectContextError> {
        let workspace_info = WorkspaceInfo::load_from_path(manifest_path)?;

        let project = TracelProject::load(&workspace_info.workspace_root)
            .map_err(|e| {
                ProjectContextError::new(
                    "Failed to read tracel.toml".to_string(),
                    ErrorKind::Parsing,
                    Some(e.into()),
                )
            })?
            .ok_or_else(|| {
                ProjectContextError::new(
                    "No Burn Central project linked to this repository".to_string(),
                    ErrorKind::ProjectNotLinked,
                    None,
                )
            })?;

        Ok(Self {
            workspace_info,
            build_profile: "release".to_string(),
            project,
        })
    }

    pub fn init(project: TracelProject, manifest_path: &Path) -> Result<Self, ProjectContextError> {
        let workspace_info = WorkspaceInfo::load_from_path(manifest_path)?;

        project.save(&workspace_info.workspace_root).map_err(|e| {
            ProjectContextError::new(
                "Failed to write tracel.toml".to_string(),
                ErrorKind::ProjectInitialization,
                Some(e.into()),
            )
        })?;

        Ok(Self {
            workspace_info,
            build_profile: "release".to_string(),
            project,
        })
    }

    pub fn unlink(manifest_path: &Path) -> Result<(), ProjectContextError> {
        let workspace_info = WorkspaceInfo::load_from_path(manifest_path)?;

        TracelProject::remove(&workspace_info.workspace_root).map_err(|e| {
            ProjectContextError::new(
                "Failed to remove tracel.toml".to_string(),
                ErrorKind::Unexpected,
                Some(e.into()),
            )
        })?;

        Ok(())
    }

    pub fn get_project(&self) -> &TracelProject {
        &self.project
    }

    pub fn get_workspace_name(&self) -> &str {
        &self.workspace_info.workspace_name
    }

    pub fn get_workspace_root(&self) -> &Path {
        &self.workspace_info.workspace_root
    }

    pub fn get_manifest_path(&self) -> PathBuf {
        self.workspace_info.get_manifest_path()
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
