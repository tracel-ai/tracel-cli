//! Project helpers for CLI operations

use anyhow::Context;

use crate::{
    context::CliContext,
    error::{CliError, ErrorKind as CliErrorKind},
    tools::{
        cargo::try_locate_manifest,
        project_context::{ErrorKind, ProjectContext, ProjectContextError},
        workspace::WorkspaceInfo,
    },
};
use tracel_client::console::Client;

pub fn find_manifest() -> anyhow::Result<std::path::PathBuf> {
    try_locate_manifest().ok_or_else(|| {
        anyhow::anyhow!(
            "Could not locate Cargo.toml manifest. Please run this command inside a Burn project directory."
        )
    })
}

/// Check if current directory has a linked Tracel Console project
pub fn is_tracel_project_linked() -> bool {
    let manifest_path = find_manifest();
    match manifest_path {
        Err(_) => false,
        Ok(p) => ProjectContext::load(&p).is_ok(),
    }
}

/// One error that says what went wrong, why, and what to do next.
fn explain_project_context_error(e: ProjectContextError) -> anyhow::Error {
    let hint = match e.kind() {
        ErrorKind::ManifestNotFound => "Run this command from a Rust project directory.",
        ErrorKind::ProjectNotLinked => "Run 'tracel init' to link it.",
        ErrorKind::Parsing => "Check that Cargo.toml and tracel.toml are valid.",
        ErrorKind::ProjectInitialization => "Re-link the project with 'tracel init --force'.",
        ErrorKind::Unexpected => "Check your project setup.",
    };

    let mut message = e.to_string();
    let mut cause = std::error::Error::source(&e);
    while let Some(err) = cause {
        message.push_str(&format!(": {err}"));
        cause = err.source();
    }

    anyhow::anyhow!("{message}. {hint}")
}

/// Require a linked Tracel Console project.
pub fn require_linked_project() -> anyhow::Result<ProjectContext> {
    let manifest_path = find_manifest()?;
    ProjectContext::load(&manifest_path).map_err(explain_project_context_error)
}

/// Resolve a namespace/project pair, using explicit overrides where given and
/// falling back to the linked project's namespace/name for whichever is omitted.
pub fn resolve_namespace_project(
    namespace: Option<String>,
    project: Option<String>,
) -> anyhow::Result<(String, String)> {
    if let (Some(ns), Some(proj)) = (&namespace, &project) {
        return Ok((ns.clone(), proj.clone()));
    }

    let linked = require_linked_project()?;
    let bc_project = linked.get_project();

    Ok((
        namespace.unwrap_or_else(|| bc_project.owner.clone()),
        project.unwrap_or_else(|| bc_project.name.clone()),
    ))
}

/// Require a Cargo workspace (with or without Tracel Console linkage)
pub fn require_cargo_workspace() -> anyhow::Result<WorkspaceInfo> {
    let manifest_path = find_manifest()?;
    ProjectContext::load_workspace_info(&manifest_path).map_err(explain_project_context_error)
}

/// Whether `tracel init` should go ahead. Fails outside a Rust project, and stops
/// without error when the project is already linked and `force` is off.
pub fn can_initialize_project(context: &CliContext, force: bool) -> anyhow::Result<bool> {
    find_manifest()?;

    if is_tracel_project_linked() && !force {
        context
            .terminal()
            .print("Project is already linked to Tracel Console.");
        context
            .terminal()
            .print("Use --force flag to reinitialize.");
        return Ok(false);
    }

    Ok(true)
}

/// Validate that the linked project exists on Tracel Console server
pub fn validate_project_exists_on_server(
    context: &CliContext,
    project: &ProjectContext,
    client: &Client,
) -> anyhow::Result<()> {
    let bc_project = project.get_project();

    match client.get_project(&bc_project.owner, &bc_project.name) {
        Ok(_) => Ok(()),
        Err(e) if e.is_not_found() => {
            context.terminal().print_err(&format!(
                "Project {}/{} does not exist on Tracel Console.",
                bc_project.owner, bc_project.name
            ));
            context
                .terminal()
                .print("The linked project may have been deleted or renamed on the server.");
            context.terminal().print(
                "Run 'tracel init --force' to reinitialize and link to a different project.",
            );
            Err(CliError::new(
                CliErrorKind::NotFound,
                format!(
                    "Project {}/{} not found on Tracel Console",
                    bc_project.owner, bc_project.name
                ),
            )
            .with_hint("Run 'tracel init --force' to link another project.")
            .into())
        }
        Err(e) => {
            context.terminal().print_err(&format!(
                "Failed to verify project on Tracel Console: {}",
                e
            ));
            Err(e).context("Failed to verify project exists on server")
        }
    }
}
