use anyhow::Context;
use clap::Args;

use crate::{
    context::CliContext, helpers::require_linked_project, tools::project_context::ProjectContext,
};

#[derive(Args, Debug)]
pub struct UnlinkArgs {
    /// Unlink without asking for confirmation
    #[arg(long, short = 'y')]
    pub yes: bool,
}

pub fn handle_command(args: UnlinkArgs, context: CliContext) -> anyhow::Result<()> {
    let project = require_linked_project()?;

    context.terminal().command_title("Unlink");

    if !args.yes {
        if !context.terminal().is_interactive() {
            anyhow::bail!("Unlinking needs confirmation: pass --yes to unlink without a prompt.");
        }

        let confirmed = context.terminal().confirm(
            "Are you sure you want to unlink the Tracel Console project from this repository?",
        )?;
        if !confirmed {
            context.terminal().cancel_finalize("Cancelled");
            return Ok(());
        }
    }

    ProjectContext::unlink(&project.get_manifest_path()).context("Failed to unlink project")?;
    context.terminal().finalize("Project unlinked successfully");

    Ok(())
}
