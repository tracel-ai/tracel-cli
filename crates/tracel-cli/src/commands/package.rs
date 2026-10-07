mod code_version;

use clap::{Args, ValueEnum};
use serde::Serialize;

pub use code_version::build_package;

use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::{require_workspace_project, validate_project_exists_on_server};
use crate::tools::target;
use crate::ui::{Outcome, Terminal};

#[derive(Args, Clone, Debug)]
pub struct PackageArgs {
    /// Package a compiled binary or source (required without prompts)
    #[arg(long, value_enum)]
    pub mode: Option<Mode>,
    /// Rust target triple to build (repeatable; binary mode only)
    #[arg(long = "target", value_name = "TRIPLE")]
    pub targets: Vec<String>,
    /// Name of the binary to upload when several are built
    #[arg(long, value_name = "NAME")]
    pub bin: Option<String>,
    /// Install missing Rust targets without asking
    #[arg(long)]
    pub install_targets: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Binary,
    Source,
}

impl PackageArgs {
    /// Reject flags that cannot work before doing anything: a missing `--mode` without
    /// prompts, an unknown `--target`, or `--target` with `--mode source`.
    pub fn check(&self, terminal: &Terminal) -> anyhow::Result<()> {
        if self.mode.is_none() && !terminal.is_interactive() {
            return Err(CliError::new(
                ErrorKind::Usage,
                "Missing --mode. Valid values: binary, source.",
            )
            .with_hint("Pass --mode binary or --mode source.")
            .into());
        }
        if let Some(mode) = self.mode {
            check_targets_allowed(mode, &self.targets)?;
        }
        for triple in &self.targets {
            target::parse_target(triple)?;
        }
        Ok(())
    }
}

pub fn handle_command(args: PackageArgs, context: CliContext) -> anyhow::Result<Outcome> {
    args.check(context.terminal())?;
    context.terminal().command_title("Package project");

    // 1. Require a workspace and a project that exists on the server.
    let project = require_workspace_project(&context)?;
    let client = get_client_and_login_if_needed(&context)?;
    validate_project_exists_on_server(&project, &client)?;

    // 2. Build, then upload.
    let version = build_package(&context, &project, &args)?.publish(&context, &client, &project)?;
    context.terminal().finalize(if version.uploaded {
        "Project packaged successfully."
    } else {
        "Nothing to upload."
    });
    Ok(version.into())
}

fn check_targets_allowed(mode: Mode, targets: &[String]) -> Result<(), CliError> {
    if mode == Mode::Source && !targets.is_empty() {
        return Err(CliError::new(
            ErrorKind::Usage,
            "--target is only valid with --mode binary.",
        ));
    }
    Ok(())
}
