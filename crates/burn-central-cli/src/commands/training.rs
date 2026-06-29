use anyhow::Context;
use burn_central_workspace::tools::cargo;
use clap::Parser;

use crate::{app_config::tracel_env_value, context::CliContext};

#[derive(Parser, Debug, Default)]
pub struct TrainingArgs {
    /// Arguments forwarded to `cargo run` (everything after `--`).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    forwarded: Vec<String>,
}

pub(crate) fn handle_command(args: TrainingArgs, context: CliContext) -> anyhow::Result<()> {
    run_cargo(&args.forwarded, context)
}

/// Run `cargo run` in the current directory, forwarding `forwarded` after `--`.
///
/// stdin/stdout/stderr are inherited so the run is interactive, and the child's
/// exit code is mirrored. `burn train -- entrypoint` is therefore equivalent to
/// `cargo run -- entrypoint`.
pub(crate) fn run_cargo(forwarded: &[String], context: CliContext) -> anyhow::Result<()> {
    let mut cmd = cargo::command();
    cmd.arg("run");

    cmd.env("TRACEL_ENV", tracel_env_value(&context.environment()));

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
