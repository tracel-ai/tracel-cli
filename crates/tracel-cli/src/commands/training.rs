use anyhow::Context;
use clap::Parser;

use crate::{context::CliContext, environment, tools::cargo, ui::Outcome};

#[derive(Parser, Debug, Default)]
pub struct TrainingArgs {
    /// Arguments for the program, after --
    #[arg(
        value_name = "ARGS",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    forwarded: Vec<String>,
}

pub fn handle_command(args: TrainingArgs, context: CliContext) -> anyhow::Result<Outcome> {
    run_cargo(&args.forwarded, context)?;
    Ok(Outcome::streamed())
}

/// Run `cargo run` in this directory with `forwarded` after `--`. The program gets
/// tracel's stdio, and its exit code is tracel's.
fn run_cargo(forwarded: &[String], context: CliContext) -> anyhow::Result<()> {
    let mut cmd = cargo::command();
    cmd.arg("run");

    cmd.env("TRACEL_ENV", environment::sdk_value(&context.environment()));
    if let Some(project) = context.project() {
        cmd.env("TRACEL_NAMESPACE", &project.namespace);
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

    Err(cmd.exec()).context("Can't run `cargo run`")
}

/// Windows cannot replace a process, so wait for the child and mirror its exit code.
#[cfg(not(unix))]
fn hand_over(mut cmd: std::process::Command) -> anyhow::Result<()> {
    let status = cmd.status().context("Can't run `cargo run`")?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}
