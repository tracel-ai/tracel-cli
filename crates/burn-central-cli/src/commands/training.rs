use anyhow::Context;
use burn_central_workspace::{ProjectContext, tools::cargo};
use clap::Parser;

use crate::app_config::tracel_env_value;
use crate::context::CliContext;
use crate::helpers::find_manifest;

#[derive(Parser, Debug, Default)]
pub struct TrainingArgs {
    /// Arguments forwarded to `cargo run` (everything after `--`).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    forwarded: Vec<String>,
}

pub(crate) fn handle_command(args: TrainingArgs, context: CliContext) -> anyhow::Result<()> {
    run_cargo(&context, &args.forwarded)
}

/// Run `cargo run` in the current directory, forwarding `forwarded` after `--`.
///
/// stdin/stdout/stderr are inherited so the run is interactive, and the child's
/// exit code is mirrored. `burn train -- yo` is therefore equivalent to
/// `cargo run -- yo`.
///
/// The CLI exports the Burn Central environment (and, when the project is
/// linked, its namespace/project) to the child process so the SDK running
/// inside the training binary targets the same backend and project as the CLI.
pub(crate) fn run_cargo(context: &CliContext, forwarded: &[String]) -> anyhow::Result<()> {
    let mut cmd = cargo::command(); // honors $CARGO
    cmd.arg("run");

    // The environment is a CLI-runtime concept (selected via --dev/--staging),
    // so it is always exported and is not stored in tracel.toml.
    cmd.env("TRACEL_ENV", tracel_env_value(&context.environment()));

    // Namespace/project come from the linked tracel.toml. The SDK reads these
    // env vars before falling back to the file, keeping the CLI authoritative.
    // `burn train` does not require linkage, so skip silently if unavailable.
    if let Ok(manifest) = find_manifest() {
        if let Ok(project) = ProjectContext::load(&manifest) {
            let bc_project = project.get_project();
            cmd.env("TRACEL_NAMESPACE", &bc_project.owner);
            cmd.env("TRACEL_PROJECT", &bc_project.name);
        }
    }

    if !forwarded.is_empty() {
        cmd.arg("--");
        cmd.args(forwarded);
    }

    let status = cmd.status().context("Failed to run `cargo run`")?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}
