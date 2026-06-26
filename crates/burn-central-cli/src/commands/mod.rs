use crate::commands::init::prompt_init;
use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::helpers::{is_burn_central_project_linked, require_cargo_workspace};

pub mod clean;
pub mod init;
pub mod login;
pub mod me;
pub mod package;
pub mod project;
pub mod training;
pub mod unlink;

/// `burn` with no subcommand runs the project via `cargo run` (like `burn train`
/// with no forwarded arguments), but first ensures the repository is linked to a
/// Burn Central project, prompting for initialization if it is not.
pub fn default_command(mut context: CliContext) -> anyhow::Result<()> {
    let client = get_client_and_login_if_needed(&mut context)?;

    // Check if we have a linked Burn Central project
    if !is_burn_central_project_linked() {
        // Make sure we're at least in a Rust project before initializing
        let _crate_info = require_cargo_workspace(&context)?;
        context
            .terminal()
            .print("No Burn Central project linked, prompting for initialization.");
        prompt_init(&context, &client)?;
        return Ok(());
    }

    training::run_cargo(&context, &[])
}
