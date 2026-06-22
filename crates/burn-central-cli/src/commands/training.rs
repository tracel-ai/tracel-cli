use anyhow::Context;
use burn_central_workspace::tools::cargo;
use clap::Parser;

use crate::context::CliContext;

#[derive(Parser, Debug, Default)]
pub struct TrainingArgs {
    /// Arguments forwarded to `cargo run` (everything after `--`).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    forwarded: Vec<String>,
}

pub(crate) fn handle_command(args: TrainingArgs, _context: CliContext) -> anyhow::Result<()> {
    run_cargo(&args.forwarded)
}

/// Run `cargo run` in the current directory, forwarding `forwarded` after `--`.
///
/// stdin/stdout/stderr are inherited so the run is interactive, and the child's
/// exit code is mirrored. `burn train -- yo` is therefore equivalent to
/// `cargo run -- yo`.
pub(crate) fn run_cargo(forwarded: &[String]) -> anyhow::Result<()> {
    let mut cmd = cargo::command(); // honors $CARGO
    cmd.arg("run");
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
