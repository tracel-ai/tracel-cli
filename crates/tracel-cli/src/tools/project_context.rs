use std::path::{Path, PathBuf};

use crate::tools::{tracel_config::TracelProject, workspace::WorkspaceInfo};

/// What went wrong with a workspace or its link.
#[derive(Debug)]
pub enum ErrorKind {
    ManifestNotFound,
    Parsing,
    ProjectInitialization,
    ProjectNotLinked,
    Unexpected,
}

/// An error about a workspace or its link.
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
}

impl std::fmt::Display for ProjectContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// A workspace and the project it is linked to.
pub struct ProjectContext {
    pub workspace_info: WorkspaceInfo,
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
                    "Can't read tracel.toml".to_string(),
                    ErrorKind::Parsing,
                    Some(e.into()),
                )
            })?
            .ok_or_else(|| {
                ProjectContextError::new(
                    "This workspace isn't linked to a project".to_string(),
                    ErrorKind::ProjectNotLinked,
                    None,
                )
            })?;

        Ok(Self {
            workspace_info,
            project,
        })
    }

    pub fn init(project: TracelProject, manifest_path: &Path) -> Result<Self, ProjectContextError> {
        let workspace_info = WorkspaceInfo::load_from_path(manifest_path)?;

        project.save(&workspace_info.workspace_root).map_err(|e| {
            ProjectContextError::new(
                "Can't write tracel.toml".to_string(),
                ErrorKind::ProjectInitialization,
                Some(e.into()),
            )
        })?;

        Ok(Self {
            workspace_info,
            project,
        })
    }

    pub fn unlink(manifest_path: &Path) -> Result<(), ProjectContextError> {
        let workspace_info = WorkspaceInfo::load_from_path(manifest_path)?;

        TracelProject::remove(&workspace_info.workspace_root).map_err(|e| {
            ProjectContextError::new(
                "Can't remove tracel.toml".to_string(),
                ErrorKind::Unexpected,
                Some(e.into()),
            )
        })?;

        Ok(())
    }

    pub fn get_project(&self) -> &TracelProject {
        &self.project
    }

    pub fn get_workspace_root(&self) -> &Path {
        &self.workspace_info.workspace_root
    }

    pub fn get_manifest_path(&self) -> PathBuf {
        self.workspace_info.get_manifest_path()
    }
}
