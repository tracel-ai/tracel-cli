use anyhow::Context;
use clap::Parser;

use tracel_client::console::Env;

use crate::{context::CliContext, tools::cargo};

#[derive(Parser, Debug, Default)]
pub struct TrainingArgs {
    /// Arguments forwarded to `cargo run` (everything after `--`).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    forwarded: Vec<String>,
}

pub fn handle_command(args: TrainingArgs, context: CliContext) -> anyhow::Result<()> {
    run_cargo(&args.forwarded, context)
}

/// Run `cargo run` in the current directory, forwarding `forwarded` after `--`.
///
/// stdin/stdout/stderr are inherited so the run is interactive, and the child's
/// exit code is mirrored. `tracel train -- entrypoint` is therefore equivalent to
/// `cargo run -- entrypoint`.
pub fn run_cargo(forwarded: &[String], context: CliContext) -> anyhow::Result<()> {
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
