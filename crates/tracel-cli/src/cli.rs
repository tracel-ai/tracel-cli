use std::ffi::OsString;
use std::io::IsTerminal;

use clap::{CommandFactory, Parser, Subcommand};
use tracel_client::console::Env;

use crate::commands;
use crate::context::CliContext;
use crate::environment;
use crate::error::{CliError, ErrorKind, ErrorReport};
use crate::helpers::{parse_project, resolve_project};
use crate::tools::tracel_config::TracelProject;
use crate::ui::{
    self, Declined, Format, FormatArg, Layout, Outcome, Output, Place, StdoutClosed, Terminal,
};

#[derive(Parser, Debug)]
#[clap(
    name = "tracel",
    author,
    version,
    about = "Train, track and share Burn models with the Tracel Console",
    long_about = None
)]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Choose the output format
    #[arg(short, long, global = true, value_enum, conflicts_with = "json")]
    pub output: Option<FormatArg>,

    /// Print machine-readable JSON
    #[arg(long, global = true, conflicts_with = "output")]
    pub json: bool,

    /// Fail instead of asking for input
    #[arg(long, global = true)]
    pub no_input: bool,

    /// Use this Tracel Console project
    #[arg(long, global = true, value_name = "NAMESPACE/NAME", value_parser = parse_project)]
    pub project: Option<TracelProject>,

    /// Run as if started in this directory
    #[arg(short = 'C', global = true, value_name = "DIR")]
    pub working_directory: Option<std::path::PathBuf>,

    /// Use the development environment
    #[arg(long, global = true, action = clap::ArgAction::SetTrue, hide = true, conflicts_with = "staging")]
    pub dev: bool,

    /// Use this staging environment
    #[arg(
        long,
        global = true,
        value_name = "VERSION",
        hide = true,
        conflicts_with = "dev"
    )]
    pub staging: Option<u8>,
}

impl CliArgs {
    /// Parse the arguments `tracel` was started with.
    pub fn try_parse_args<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        Self::try_parse_from(args)
    }
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run the workspace locally with `cargo run`
    Train(commands::training::TrainingArgs),
    /// Upload a code version of the workspace
    Package(commands::package::PackageArgs),
    /// Log in to the Tracel Console
    Login(commands::login::LoginArgs),
    /// Log out of the Tracel Console
    Logout,
    /// Show your login, or print an access token
    Auth(commands::auth::AuthArgs),
    /// Browse experiments, their metrics and logs
    #[command(visible_alias = "exp")]
    Experiments(commands::experiments::ExperimentsArgs),
    /// Browse, follow and cancel jobs
    Jobs(commands::jobs::JobsArgs),
    /// Link the workspace to a Tracel Console project
    Init(commands::init::InitArgs),
    /// Unlink the workspace from its project
    Unlink(commands::unlink::UnlinkArgs),
    /// Show who you are logged in as
    Me,
    /// Show the current project, or list projects
    Project(commands::project::ProjectArgs),
    /// Browse and manage models
    Models(commands::models::ModelsArgs),
    /// Browse and download experiment artifacts
    Artifacts(commands::artifacts::ArtifactsArgs),
    /// Browse the project's datasets
    Datasets(commands::datasets::DatasetsArgs),
}

impl Commands {
    /// How the command draws on a terminal.
    fn layout(&self) -> Layout {
        match self {
            Self::Train(_) => Layout::Bare,
            Self::Auth(args) => args.layout(),
            Self::Package(_) => Layout::flow("Package a code version"),
            Self::Login(args) => args.layout(),
            Self::Logout => Layout::flow("Log out of the Tracel Console"),
            Self::Experiments(args) => args.layout(),
            Self::Jobs(args) => args.layout(),
            Self::Init(_) => Layout::flow("Link this workspace to a Tracel Console project"),
            Self::Unlink(_) => Layout::flow("Unlink this workspace from its project"),
            Self::Me | Self::Project(_) | Self::Datasets(_) => Layout::Read,
            Self::Models(args) => args.layout(),
            Self::Artifacts(args) => args.layout(),
        }
    }

    /// Whether the command works in a project, which its header then names.
    fn in_project(&self) -> bool {
        matches!(
            self,
            Self::Package(_)
                | Self::Experiments(_)
                | Self::Jobs(_)
                | Self::Models(_)
                | Self::Artifacts(_)
                | Self::Datasets(_)
        )
    }

    /// Choose what `auto` means: human on a terminal and JSON otherwise, except for
    /// `auth token` and `train`, whose stdout is plain text for another program.
    fn auto_format(&self, stdout_is_terminal: bool) -> Format {
        match self {
            Self::Train(_)
            | Self::Auth(commands::auth::AuthArgs {
                command: commands::auth::AuthCommands::Token,
            }) => Format::Human,
            _ => Format::for_stdout(stdout_is_terminal),
        }
    }
}

pub fn cli_main() {
    let args = match CliArgs::try_parse_args(std::env::args_os()) {
        Ok(args) => args,
        Err(error) if !error.use_stderr() => error.exit(),
        Err(error) => {
            let format = usage_format(
                std::env::args_os().skip(1),
                std::env::var("TRACEL_OUTPUT").ok().as_deref(),
                std::io::stdout().is_terminal(),
            );
            if format == Format::Human {
                error.exit();
            }
            fail_early(&usage_error(&error).into(), format);
        }
    };

    let stdout_is_terminal = std::io::stdout().is_terminal();
    let auto = match &args.command {
        Some(command) => command.auto_format(stdout_is_terminal),
        None => Format::for_stdout(stdout_is_terminal),
    };
    let format = Format::resolve(
        args.output,
        args.json,
        std::env::var("TRACEL_OUTPUT").ok().as_deref(),
        auto,
    );

    if let Some(directory) = &args.working_directory {
        if let Err(error) = std::env::set_current_dir(directory) {
            let error = CliError::new(
                ErrorKind::Usage,
                format!(
                    "Can't change to directory '{}': {error}",
                    directory.display()
                ),
            );
            fail_early(&error.into(), format.unwrap_or(auto));
        }
    }

    let Some(command) = args.command else {
        // A reader that closes early (`tracel | head`) is not an error.
        let _ = CliArgs::command().print_help();
        return;
    };

    let format = format.unwrap_or_else(|error| fail_early(&error.into(), auto));
    let environment_value =
        std::env::var_os("TRACEL_ENV").map(|value| value.to_string_lossy().into_owned());
    let environment = environment::resolve(args.dev, args.staging, environment_value.as_deref())
        .unwrap_or_else(|error| fail_early(&error.into(), format));
    let place = Place {
        project: command
            .in_project()
            .then(|| resolve_project(args.project.as_ref()).ok())
            .flatten()
            .map(|resolved| format!("{}/{}", resolved.project.namespace, resolved.project.name)),
        environment: match environment {
            Env::Production => None,
            _ => Some(environment::name(&environment)),
        },
    };
    let (output, terminal) = ui::channels(format, args.no_input, command.layout(), place);

    let context = CliContext::new(terminal.clone(), output.clone(), environment, args.project);
    if let Err(error) = handle_command(command, context).and_then(|outcome| output.finish(outcome))
    {
        fail(&error, &output, &terminal);
    }
}

/// How a command that stopped on an error ends.
enum Stop<'a> {
    /// Its reader closed stdout, having got all it wanted.
    Closed,
    /// The user answered a prompt no, and the command ends on what it didn't do.
    Declined(&'a str),
    Failed(ErrorReport<'a>),
}

impl<'a> Stop<'a> {
    fn of(error: &'a anyhow::Error) -> Self {
        if error.chain().any(|cause| cause.is::<StdoutClosed>()) {
            return Self::Closed;
        }
        match error
            .chain()
            .find_map(|cause| cause.downcast_ref::<Declined>())
        {
            Some(declined) => Self::Declined(declined.outcome()),
            None => Self::Failed(ErrorReport::new(error)),
        }
    }

    fn exit_code(&self) -> i32 {
        match self {
            Self::Closed | Self::Declined(_) => 0,
            Self::Failed(report) => report.exit_code,
        }
    }
}

/// End a command that stopped on `error`: report a failure on both channels, and exit
/// with its code.
fn fail(error: &anyhow::Error, output: &Output, terminal: &Terminal) -> ! {
    let stop = Stop::of(error);
    match &stop {
        Stop::Closed => {}
        Stop::Declined(outcome) => terminal.outro(outcome),
        Stop::Failed(report) => {
            output.error(report);
            terminal.error(report);
        }
    }
    std::process::exit(stop.exit_code())
}

/// Fail before the command's channels exist.
fn fail_early(error: &anyhow::Error, format: Format) -> ! {
    let (output, terminal) = ui::channels(format, true, Layout::Bare, Place::default());
    fail(error, &output, &terminal)
}

/// Turn arguments that did not parse into a usage error, without clap's "error: "
/// prefix.
fn usage_error(error: &clap::Error) -> CliError {
    let message = error.to_string();
    let message = message.trim();
    CliError::new(
        ErrorKind::Usage,
        message.strip_prefix("error: ").unwrap_or(message),
    )
}

/// Choose the format for an error in arguments that did not parse, from the output
/// flags among them and `environment`, the value of TRACEL_OUTPUT.
fn usage_format(
    arguments: impl IntoIterator<Item = OsString>,
    environment: Option<&str>,
    stdout_is_terminal: bool,
) -> Format {
    let mut arguments = arguments.into_iter();
    let mut flag = None;
    let mut json = false;
    while let Some(argument) = arguments.next() {
        let argument = argument.to_string_lossy();
        if argument == "--" {
            break;
        }
        if argument == "--json" {
            json = true;
        } else {
            let value = if argument == "--output" || argument == "-o" {
                arguments
                    .next()
                    .map(|value| value.to_string_lossy().into_owned())
            } else {
                argument
                    .strip_prefix("--output=")
                    .or_else(|| argument.strip_prefix("-o"))
                    .map(str::to_owned)
            };
            if let Some(value) = value {
                flag = Some(match value.as_str() {
                    "human" => FormatArg::Human,
                    "json" => FormatArg::Json,
                    _ => FormatArg::Auto,
                });
            }
        }
    }
    let auto = Format::for_stdout(stdout_is_terminal);
    Format::resolve(flag, json, environment, auto).unwrap_or(auto)
}

fn handle_command(command: Commands, context: CliContext) -> anyhow::Result<Outcome> {
    match command {
        Commands::Train(run_args) => commands::training::handle_command(run_args, context),
        Commands::Package(package_args) => commands::package::handle_command(package_args, context),
        Commands::Login(login_args) => commands::login::handle_command(login_args, context),
        Commands::Logout => commands::logout::handle_command(context),
        Commands::Auth(auth_args) => commands::auth::handle_command(auth_args, context),
        Commands::Experiments(experiments_args) => {
            commands::experiments::handle_command(experiments_args, context)
        }
        Commands::Jobs(jobs_args) => commands::jobs::handle_command(jobs_args, context),
        Commands::Init(init_args) => commands::init::handle_command(init_args, context),
        Commands::Unlink(unlink_args) => commands::unlink::handle_command(unlink_args, context),
        Commands::Me => commands::me::handle_command(context),
        Commands::Project(args) => commands::project::handle_command(args, context),
        Commands::Models(args) => commands::models::handle_command(args, context),
        Commands::Artifacts(args) => commands::artifacts::handle_command(args, context),
        Commands::Datasets(args) => commands::datasets::handle_command(args, context),
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::error::classify;
    use crate::test_support::{command, parse};
    use crate::ui::{Declined, StdoutClosed};

    fn arguments(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn given_the_command_definitions_when_clap_checks_them_then_they_are_valid() {
        CliArgs::command().debug_assert();
    }

    #[rstest]
    #[case::auth_token(&["auth", "token"])]
    #[case::train(&["train", "--", "--epochs", "1"])]
    fn given_a_command_whose_stdout_is_its_own_when_piped_then_its_output_stays_text(
        #[case] args: &[&str],
    ) {
        let format = command(args).auto_format(false);

        assert_eq!(format, Format::Human);
    }

    #[test]
    fn given_another_command_when_stdout_is_piped_then_its_output_is_json() {
        let format = command(&["me"]).auto_format(false);

        assert_eq!(format, Format::Json);
    }

    #[test]
    fn given_another_command_when_stdout_is_a_terminal_then_its_output_is_text() {
        let format = command(&["me"]).auto_format(true);

        assert_eq!(format, Format::Human);
    }

    #[rstest]
    #[case::json_flag(&["models", "get", "--json"])]
    #[case::output_flag(&["-o", "json", "models", "get"])]
    #[case::output_equals(&["--output=json", "models", "get"])]
    #[case::output_attached(&["-ojson", "models", "get"])]
    fn given_an_output_flag_among_arguments_that_do_not_parse_when_reporting_then_the_error_is_json(
        #[case] args: &[&str],
    ) {
        let format = usage_format(arguments(args), None, true);

        assert_eq!(format, Format::Json);
    }

    #[test]
    fn given_tracel_output_json_when_arguments_do_not_parse_then_the_error_is_json() {
        let format = usage_format(arguments(&["models", "get"]), Some("json"), true);

        assert_eq!(format, Format::Json);
    }

    #[test]
    fn given_an_output_flag_and_tracel_output_when_arguments_do_not_parse_then_the_flag_wins() {
        let format = usage_format(
            arguments(&["models", "get", "-o", "human"]),
            Some("json"),
            true,
        );

        assert_eq!(format, Format::Human);
    }

    #[test]
    fn given_json_after_a_double_dash_when_arguments_do_not_parse_then_it_is_not_an_output_flag() {
        let format = usage_format(arguments(&["train", "--", "--json"]), None, true);

        assert_eq!(format, Format::Human);
    }

    #[test]
    fn given_arguments_that_do_not_parse_when_reporting_them_then_it_is_a_usage_error_without_clap_prefix()
     {
        let parse_error = parse(&["models", "get", "--unknown"]).unwrap_err();

        let error = anyhow::Error::new(usage_error(&parse_error));

        assert_eq!(classify(&error), ErrorKind::Usage);
        assert!(!error.to_string().starts_with("error:"), "{error}");
    }

    #[test]
    fn given_a_reader_that_closed_stdout_when_the_command_stops_then_it_exits_with_0() {
        let error = anyhow::Error::new(StdoutClosed).context("Can't write the result");

        let stop = Stop::of(&error);

        assert!(matches!(stop, Stop::Closed));
        assert_eq!(stop.exit_code(), 0);
    }

    #[test]
    fn given_a_declined_prompt_when_the_command_stops_then_it_ends_on_its_outcome_with_0() {
        let error = anyhow::Error::new(Declined::new("Nothing was linked."))
            .context("Can't link the workspace");

        let stop = Stop::of(&error);

        assert!(matches!(stop, Stop::Declined("Nothing was linked.")));
        assert_eq!(stop.exit_code(), 0);
    }

    #[test]
    fn given_a_failure_when_the_command_stops_then_it_exits_with_the_failure_code() {
        let error = anyhow::Error::new(CliError::new(ErrorKind::NotFound, "No model 'w'."));

        let stop = Stop::of(&error);

        assert_eq!(stop.exit_code(), 5);
    }
}
