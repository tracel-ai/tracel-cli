mod remote;

use anyhow::Context;
use clap::{ArgGroup, Args};

use tracel_client::console::Env;

use crate::commands::package::PackageArgs;
use crate::{context::CliContext, tools::cargo, ui::Outcome};

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("packaging")
        .args(["mode", "targets", "bin", "install_targets"])
        .multiple(true)
        .requires("remote")
))]
pub struct RunArgs {
    /// Queue a job on this compute provider group instead of running locally
    #[arg(long, value_name = "GROUP")]
    pub remote: Option<String>,
    #[command(flatten)]
    pub package: PackageArgs,
    /// Queue the job without asking for confirmation
    #[arg(long, short = 'y', requires = "remote")]
    pub yes: bool,
    /// Package and show the job without uploading or queuing anything
    #[arg(long, requires = "remote", conflicts_with_all = ["follow", "yes"])]
    pub dry_run: bool,
    /// Follow the job's logs until it finishes; exit 9 when it fails or is cancelled
    #[arg(long, requires = "remote")]
    pub follow: bool,
    /// Arguments passed to the program
    #[arg(last = true, value_name = "ARGS")]
    pub forwarded: Vec<String>,
}

pub fn handle_command(args: RunArgs, context: CliContext) -> anyhow::Result<Outcome> {
    match &args.remote {
        Some(group) => remote::handle_command(group, &args, &context),
        None => {
            run_locally(&args.forwarded, &context)?;
            Ok(Outcome::streamed())
        }
    }
}

/// Run `cargo run` in the current directory, passing the arguments given after `--`.
///
/// `tracel run -- entrypoint` is equivalent to `cargo run -- entrypoint`: stdio is
/// inherited and the program's exit code is the command's.
fn run_locally(forwarded: &[String], context: &CliContext) -> anyhow::Result<()> {
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

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;
    use clap::{CommandFactory, Parser};

    use super::*;
    use crate::cli::{CliArgs, Commands};
    use crate::commands::package::Mode;

    fn parse(arguments: &[&str]) -> Result<RunArgs, clap::Error> {
        let Some(Commands::Run(args)) = CliArgs::try_parse_from(arguments)?.command else {
            panic!("Expected run command");
        };
        Ok(args)
    }

    #[test]
    fn without_remote_the_run_is_local() {
        CliArgs::command().debug_assert();
        let args = parse(&["tracel", "run", "--", "--remote", "gpu", "--yes"]).unwrap();
        assert!(args.remote.is_none());
        assert_eq!(args.forwarded, ["--remote", "gpu", "--yes"]);
    }

    #[test]
    fn remote_accepts_packaging_flags_and_forwarded_arguments() {
        let args = parse(&[
            "tracel",
            "run",
            "--remote",
            "gpu",
            "--mode",
            "binary",
            "--target",
            "x86_64-unknown-linux-gnu",
            "--target",
            "aarch64-unknown-linux-gnu",
            "--bin",
            "trainer",
            "--install-targets",
            "-y",
            "--follow",
            "--",
            "train",
            "--epochs",
            "10",
        ])
        .unwrap();
        assert_eq!(args.remote.as_deref(), Some("gpu"));
        assert_eq!(args.package.mode, Some(Mode::Binary));
        assert_eq!(
            args.package.targets,
            ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]
        );
        assert_eq!(args.package.bin.as_deref(), Some("trainer"));
        assert!(args.package.install_targets);
        assert!(args.yes);
        assert!(!args.dry_run);
        assert!(args.follow);
        assert_eq!(args.forwarded, ["train", "--epochs", "10"]);

        let args = parse(&["tracel", "run", "--remote", "gpu", "--dry-run"]).unwrap();
        assert!(args.dry_run);
        assert!(!args.follow);
        assert!(args.package.mode.is_none());
        assert!(args.forwarded.is_empty());
    }

    #[test]
    fn remote_flags_require_remote() {
        for flags in [
            &["--mode", "source"][..],
            &["--target", "x86_64-unknown-linux-gnu"],
            &["--bin", "trainer"],
            &["--install-targets"],
            &["--yes"],
            &["-y"],
            &["--dry-run"],
            &["--follow"],
        ] {
            let arguments = [&["tracel", "run"][..], flags].concat();
            let error = parse(&arguments).unwrap_err();
            assert_eq!(
                error.kind(),
                ErrorKind::MissingRequiredArgument,
                "{flags:?}"
            );
            assert_eq!(error.exit_code(), 2);
        }
        let error = parse(&["tracel", "run", "--remote"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn dry_run_conflicts_with_follow_and_yes() {
        for flag in ["--follow", "--yes", "-y"] {
            let error =
                parse(&["tracel", "run", "--remote", "gpu", "--dry-run", flag]).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::ArgumentConflict, "{flag}");
        }
        assert!(parse(&["tracel", "run", "--remote", "gpu", "--yes", "--follow"]).is_ok());
    }
}
