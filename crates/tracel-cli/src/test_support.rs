//! Helpers shared by the tests of several modules.

use std::io;

use crate::cli::{CliArgs, Commands};
use crate::error::{ErrorKind, ErrorReport, classify};
use crate::ui::{self, Format, Human, Layout, Place, Render, Terminal};

/// Parse `tracel` with `args`, as the shell would pass them.
pub fn parse(args: &[&str]) -> Result<CliArgs, clap::Error> {
    CliArgs::try_parse_args(std::iter::once("tracel").chain(args.iter().copied()))
}

/// The command `args` name, which must parse.
pub fn command(args: &[&str]) -> Commands {
    parse(args)
        .unwrap_or_else(|error| panic!("{args:?} don't parse: {error}"))
        .command
        .unwrap_or_else(|| panic!("{args:?} name no command"))
}

/// What a failure reports: its kind, its exit code, and whether it has a hint.
pub fn report(error: &anyhow::Error) -> (ErrorKind, i32, bool) {
    let report = ErrorReport::new(error);
    (classify(error), report.exit_code, report.hint.is_some())
}

/// The plain text of a result, without its summary.
pub fn plain(result: &impl Render) -> String {
    let mut text = Vec::new();
    result
        .render(&mut Human::plain(&mut text, None))
        .expect("writing to memory succeeds");
    String::from_utf8(text).expect("results are UTF-8")
}

/// A terminal that never draws or asks: JSON output, and `--no-input`.
pub fn quiet_terminal() -> Terminal {
    ui::channels(Format::Json, true, Layout::Bare, Place::default()).1
}

/// A writer whose reader has gone away.
pub struct ClosedPipe;

impl io::Write for ClosedPipe {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
