//! Helpers shared by the tests of several modules.

use serde_json::Value;
use tracel_job::JobDefinition;

use crate::cli::{CliArgs, Commands};
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

/// The plain text of a result, without its summary.
pub fn plain(result: &impl Render) -> String {
    let mut text = Vec::new();
    result
        .render(&mut Human::plain(&mut text, None))
        .expect("writing to memory succeeds");
    String::from_utf8(text).expect("results are UTF-8")
}

/// A job named `name`, with no description, and with an input schema and an example
/// input when given.
pub fn job(name: &str, input_schema: Option<Value>, input_example: Option<Value>) -> JobDefinition {
    JobDefinition {
        name: name.to_string(),
        description: None,
        input_schema,
        input_example,
    }
}

/// A terminal that never draws or asks: JSON output, and `--no-input`.
pub fn quiet_terminal() -> Terminal {
    ui::channels(Format::Json, true, Layout::Bare, Place::default()).1
}
