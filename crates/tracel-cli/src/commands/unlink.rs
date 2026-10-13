use anyhow::Context;
use clap::Args;
use serde::Serialize;

use crate::{
    context::CliContext,
    helpers::require_linked_project,
    tools::project_context::ProjectContext,
    ui::{Declined, Outcome, Render},
};

#[derive(Args, Debug)]
pub struct UnlinkArgs {
    /// Unlink without asking for confirmation
    #[arg(long, short = 'y')]
    pub yes: bool,
}

/// The project that was unlinked.
#[derive(Serialize)]
struct Unlinked {
    unlinked: bool,
    namespace: String,
    name: String,
}

impl Render for Unlinked {}

pub fn handle_command(args: UnlinkArgs, context: CliContext) -> anyhow::Result<Outcome> {
    let project = require_linked_project()?;

    if !args.yes {
        let confirmed = context.terminal().confirm(
            "Are you sure you want to unlink the Tracel Console project from this repository?",
            "yes",
            false,
        )?;
        if !confirmed {
            return Err(Declined::new("Nothing was unlinked.").into());
        }
    }

    ProjectContext::unlink(&project.get_manifest_path()).context("Failed to unlink project")?;
    context.terminal().outro("Project unlinked successfully");

    let linked = project.get_project();
    Ok(Unlinked {
        unlinked: true,
        namespace: linked.owner.clone(),
        name: linked.name.clone(),
    }
    .into())
}
