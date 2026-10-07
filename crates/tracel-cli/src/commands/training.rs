use anyhow::Context;
use clap::Parser;

use tracel_client::console::Env;

use crate::{context::CliContext, output::Outcome, tools::cargo};

#[derive(Parser, Debug, Default)]
pub struct TrainingArgs {
    /// Arguments forwarded to `cargo run` (everything after `--`).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    forwarded: Vec<String>,
}

pub fn handle_command(args: TrainingArgs, context: CliContext) -> anyhow::Result<Outcome> {
    run_cargo(&args.forwarded, context)?;
    Ok(Outcome::streamed())
}

/// Run `cargo run` in the current directory, forwarding `forwarded` after `--`.
///
/// `tracel train -- entrypoint` is equivalent to `cargo run -- entrypoint`: stdio is
/// inherited and the program's exit code is the command's.
pub fn run_cargo(forwarded: &[String], context: CliContext) -> anyhow::Result<()> {
    let mut cmd = cargo::command();
    cmd.arg("run");

    cmd.env("TRACEL_ENV", tracel_env_value(&context.environment()));
    if let Some(project) = context.project() {
        cmd.env("TRACEL_NAMESPACE", &project.owner);
        cmd.env("TRACEL_PROJECT", &project.name);
    }

    if !forwarded.is_empty() {
        cmd.arg("--");
        cmd.args(forwarded);
    }

    hand_over(cmd)
}

/// Replace this process with `cmd`. `cargo run` does the same with the program it
/// builds, so a signal sent to `tracel` reaches the program instead of orphaning it.
#[cfg(unix)]
fn hand_over(mut cmd: std::process::Command) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;

    Err(cmd.exec()).context("Failed to run `cargo run`")
}

/// Windows cannot replace a process, so wait for the child and mirror its exit code.
#[cfg(not(unix))]
fn hand_over(mut cmd: std::process::Command) -> anyhow::Result<()> {
    let status = cmd.status().context("Failed to run `cargo run`")?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}

/// The value the SDK expects in the `TRACEL_ENV` environment variable.
///
/// The SDK parses this string with an explicit match (see `discover_env` in the
/// `tracel-core` cloud backend). This needs to match.
fn tracel_env_value(env: &Env) -> String {
    match env {
        Env::Production => "Production".to_string(),
        Env::Development => "Development".to_string(),
        Env::Staging(version) => format!("Staging({version})"),
    }
}
