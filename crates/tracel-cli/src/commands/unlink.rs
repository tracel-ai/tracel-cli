use clap::Args;
use serde::Serialize;

use crate::{
    context::CliContext,
    helpers::require_linked_project,
    tools::project_context::ProjectContext,
    ui::{Declined, Outcome, Render, Unanswered},
};

#[derive(Args, Debug)]
pub struct UnlinkArgs {
    /// Unlink without asking
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
    let linked = project.get_project();
    let name = format!("{}/{}", linked.owner, linked.name);

    if !args.yes {
        let confirmed = context.terminal().confirm(
            &format!("Unlink {name}?"),
            false,
            Unanswered::new(
                format!("Unlinking {name} needs confirmation."),
                "Pass --yes to unlink without asking.",
            ),
        )?;
        if !confirmed {
            return Err(Declined::new("Nothing was unlinked.").into());
        }
    }

    ProjectContext::unlink(&project.get_manifest_path())?;
    context.terminal().outro(&format!("Unlinked {name}."));

    Ok(Unlinked {
        unlinked: true,
        namespace: linked.owner.clone(),
        name: linked.name.clone(),
    }
    .into())
}
