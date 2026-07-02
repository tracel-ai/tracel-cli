use std::path::{Path, PathBuf};

use crate::tools::{tracel_config::TracelProject, workspace::WorkspaceInfo};

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
                    "No Tracel Console project linked to this repository".to_string(),
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
