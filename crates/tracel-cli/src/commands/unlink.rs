use anyhow::Context;
use clap::Args;
use serde::Serialize;

use crate::{
    context::CliContext,
    helpers::require_linked_project,
    output::{Outcome, Render},
    tools::project_context::ProjectContext,
};

#[derive(Args, Debug)]
pub struct UnlinkArgs {
    /// Unlink without asking for confirmation
    #[arg(long, short = 'y')]
    pub yes: bool,
}

/// `namespace` and `name` are the project that was unlinked, absent when cancelled.
#[derive(Serialize)]
struct Unlinked {
    unlinked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

impl Render for Unlinked {}

pub fn handle_command(args: UnlinkArgs, context: CliContext) -> anyhow::Result<Outcome> {
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
            return Ok(Unlinked {
                unlinked: false,
                namespace: None,
                name: None,
            }
            .into());
        }
    }

    ProjectContext::unlink(&project.get_manifest_path()).context("Failed to unlink project")?;
    context.terminal().finalize("Project unlinked successfully");

    let linked = project.get_project();
    Ok(Unlinked {
        unlinked: true,
        namespace: Some(linked.owner.clone()),
        name: Some(linked.name.clone()),
    }
    .into())
}
