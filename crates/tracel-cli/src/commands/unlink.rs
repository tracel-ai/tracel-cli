use anyhow::Context;
use clap::Args;
use serde_json::{Value, json};

use crate::{
    context::CliContext,
    error::{CliError, ErrorKind},
    helpers::require_linked_project,
    tools::project_context::ProjectContext,
};

#[derive(Args, Debug)]
pub struct UnlinkArgs {
    /// Unlink without asking for confirmation
    #[arg(long, short = 'y')]
    pub yes: bool,
}

pub fn handle_command(args: UnlinkArgs, context: CliContext) -> anyhow::Result<Value> {
    let project = require_linked_project()?;

    context.terminal().command_title("Unlink");

    if !args.yes {
        if !context.terminal().is_interactive() {
            return Err(CliError::new(
                ErrorKind::Usage,
                "Unlinking needs confirmation: pass --yes to unlink without a prompt.",
            )
            .into());
        }

        let confirmed = match context.terminal().confirm(
            "Are you sure you want to unlink the Tracel Console project from this repository?",
        ) {
            Ok(confirmed) => confirmed,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::Interrupted) =>
            {
                false
            }
            Err(error) => return Err(error),
        };
        if !confirmed {
            context.terminal().cancel_finalize("Cancelled");
            return Ok(json!({"unlinked": false}));
        }
    }

    ProjectContext::unlink(&project.get_manifest_path()).context("Failed to unlink project")?;
    context.terminal().finalize("Project unlinked successfully");

    Ok(json!({
        "unlinked": true,
        "namespace": project.get_project().owner,
        "name": project.get_project().name,
    }))
}
