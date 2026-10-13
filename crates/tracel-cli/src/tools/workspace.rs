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

    pub fn get_ws_root(&self) -> PathBuf {
        self.metadata.workspace_root.clone().into_std_path_buf()
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
    use super::*;

    #[test]
    fn the_workspace_name_comes_from_workspace_package_then_package() {
        for (toml, expected) in [
            (
                "[workspace]\nmembers = ['a']\n[workspace.package]\nname = 'workspace-name'\n",
                Some("workspace-name"),
            ),
            ("[package]\nname = 'single-crate'\n", Some("single-crate")),
            (
                "[workspace.package]\nname = 'workspace-name'\n[package]\nname = 'package-name'\n",
                Some("workspace-name"),
            ),
            ("[workspace]\nmembers = ['a']\n", None),
            ("this is not valid toml { [ }", None),
            ("", None),
        ] {
            assert_eq!(
                extract_workspace_name_from_toml(toml).as_deref(),
                expected,
                "{toml:?}"
            );
        }
    }
}
