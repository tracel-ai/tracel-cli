use anyhow::Context;

use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::helpers::require_linked_project;

pub fn handle_command(context: CliContext) -> anyhow::Result<()> {
    context.terminal().command_title("Project Information");

    let project = require_linked_project()?;
    let client = get_client_and_login_if_needed(&context)?;

    let bc_project = project.get_project();
    let project = match client.get_project(&bc_project.owner, &bc_project.name) {
        Ok(project) => project,
        Err(e) if e.is_not_found() => anyhow::bail!(
            "Project {}/{} not found on Tracel Console. Run 'tracel init --force' to link another project.",
            bc_project.owner,
            bc_project.name
        ),
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

    Ok(())
}
