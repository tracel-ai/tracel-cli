use anyhow::Context;
use clap::Args;
use serde_json::{Value, json};

use crate::{
    context::CliContext, helpers::require_linked_project, tools::project_context::ProjectContext,
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
        let confirmed = context.terminal().confirm(
            "Are you sure you want to unlink the Tracel Console project from this repository?",
            "yes",
            false,
        )?;
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
