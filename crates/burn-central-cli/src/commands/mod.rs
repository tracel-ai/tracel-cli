use crate::context::CliContext;

pub mod clean;
pub mod init;
pub mod login;
pub mod me;
pub mod package;
pub mod project;
pub mod training;
pub mod unlink;

/// `burn` with no subcommand runs the project via `cargo run`, mirroring
/// `burn train` with no forwarded arguments.
pub fn default_command(_context: CliContext) -> anyhow::Result<()> {
    training::run_cargo(&[])
}
