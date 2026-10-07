use anyhow::Context;
use clap::Args;

use crate::{context::CliContext, environment, tools::cargo, ui::Outcome};

#[derive(Args, Debug)]
pub struct RunArgs {
    /// Arguments for the program, after --
    #[arg(last = true, value_name = "ARGS")]
    pub forwarded: Vec<String>,
}

/// Run `cargo run` in this directory with the arguments after `--`. The program gets
/// tracel's stdio, and its exit code is tracel's.
pub fn handle_command(args: RunArgs, context: CliContext) -> anyhow::Result<Outcome> {
    let mut cmd = cargo::command();
    cmd.arg("run");

    cmd.env("TRACEL_ENV", environment::sdk_value(&context.environment()));
    if let Some(project) = context.project() {
        cmd.env("TRACEL_NAMESPACE", &project.namespace);
        cmd.env("TRACEL_PROJECT", &project.name);
    }

    if !args.forwarded.is_empty() {
        cmd.arg("--");
        cmd.args(&args.forwarded);
    }

    hand_over(cmd)?;
    Ok(Outcome::streamed())
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
