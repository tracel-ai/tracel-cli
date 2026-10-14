mod code_version;

use clap::{Args, ValueEnum};
use serde::Serialize;

pub use code_version::build_package;

use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::{require_workspace_project, validate_project_exists_on_server};
use crate::tools::target;
use crate::ui::{Outcome, Terminal, short_digest};

#[derive(Args, Clone, Debug)]
pub struct PackageArgs {
    /// Package a compiled binary or the source
    #[arg(long, value_enum)]
    pub mode: Option<Mode>,
    /// Build for this target triple; repeat for more
    #[arg(long = "target", value_name = "TRIPLE")]
    pub targets: Vec<String>,
    /// Binary to build, when there are several
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

const MODE_NEEDED: &str = "Packaging needs a mode.";
const MODE_HINT: &str = "Pass --mode binary or --mode source.";

impl PackageArgs {
    /// Reject flags that can't work before doing anything: a missing `--mode` without
    /// prompts, an unknown `--target`, or `--target` with `--mode source`.
    pub fn check(&self, terminal: &Terminal) -> anyhow::Result<()> {
        if self.mode.is_none() && !terminal.is_interactive() {
            return Err(CliError::new(ErrorKind::Usage, MODE_NEEDED)
                .with_hint(MODE_HINT)
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

    let (project, source) = require_workspace_project(&context)?;
    let client = get_client_and_login_if_needed(&context)?;
    validate_project_exists_on_server(project.get_project(), &source, &client)?;

    let version = build_package(&context, &project, &args)?.publish(&context, &client, &project)?;
    let digest = short_digest(&version.digest);
    context.terminal().outro(&if version.uploaded {
        format!("Uploaded code version {digest}.")
    } else {
        format!("Code version {digest} is already uploaded.")
    });
    Ok(Outcome::data(version))
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
