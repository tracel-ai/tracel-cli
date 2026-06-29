mod entity;
pub mod event;
pub mod execution;
pub mod logging;
pub mod tools;

pub use entity::projects::tracel_project::TracelProject;
pub use entity::projects::{ErrorKind, ProjectContext, ProjectContextError, WorkspaceInfo};

pub type Result<T> = anyhow::Result<T>;
