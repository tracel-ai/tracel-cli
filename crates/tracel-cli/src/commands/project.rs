use anyhow::Context;
use serde_json::{Value, json};

use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::resolve_namespace_project;

pub fn handle_command(context: CliContext) -> anyhow::Result<Value> {
    context.terminal().command_title("Project Information");

    let resolved = resolve_namespace_project(&context)?;
    let client = get_client_and_login_if_needed(&context)?;

    let bc_project = &resolved.project;
    let project = match client.get_project(&bc_project.owner, &bc_project.name) {
        Ok(project) => project,
        Err(e) if e.is_not_found() => {
            return Err(CliError::new(
                ErrorKind::NotFound,
                format!(
                    "Project {}/{} not found on Tracel Console.",
                    bc_project.owner, bc_project.name
                ),
            )
            .with_hint("Run 'tracel init --force' to link another project.")
            .into());
        }
        Err(e) => {
            return Err(e).with_context(|| {
                format!(
                    "Failed to retrieve project {}/{}",
                    bc_project.owner, bc_project.name
                )
            });
        }
    };

    let terminal = context.terminal();
    terminal.print(&format!("Project: {}", project.project_name));
    terminal.print(&format!("Namespace: {}", project.namespace_name));
    terminal.print(&format!("Description: {}", project.description));
    terminal.print(&format!("Created By: {}", project.created_by));
    terminal.finalize("Project information retrieved successfully.");

    Ok(json!({
        "namespace": project.namespace_name,
        "name": project.project_name,
        "description": project.description,
        "created_by": project.created_by,
        "visibility": project.visibility,
        "source": resolved.source,
    }))
}
