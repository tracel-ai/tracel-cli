use std::io::IsTerminal;

use clap::{CommandFactory, Parser, Subcommand};
use tracel_client::console::Env;

use crate::commands;
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind, ErrorReport};
use crate::helpers::project::parse_project;
use crate::tools::tracel_config::TracelProject;
use crate::ui::{self, Format, FormatArg, Outcome, Output, StdoutClosed, Terminal};

#[derive(Parser, Debug)]
#[clap(name = "tracel", author, version, about, long_about = None)]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Output format (default: auto)
    #[arg(short, long, global = true, value_enum, conflicts_with = "json")]
    pub output: Option<FormatArg>,

    /// Print machine-readable JSON
    #[arg(long, global = true, conflicts_with = "output")]
    pub json: bool,

    /// Never prompt; fail when input is needed
    #[arg(long, global = true)]
    pub no_input: bool,

    /// Tracel Console project to use
    #[arg(long, global = true, value_name = "NAMESPACE/NAME", value_parser = parse_project)]
    pub project: Option<TracelProject>,

    /// Run as if started in this directory
    #[arg(short = 'C', global = true, value_name = "DIR")]
    pub working_directory: Option<std::path::PathBuf>,

    /// Use development environment (localhost:9001) with separate dev credentials
    #[arg(long, global = true, action = clap::ArgAction::SetTrue, hide = true, conflicts_with = "staging")]
    pub dev: bool,

    /// Use staging environment (specify version: 1, 2, etc.)
    #[arg(
        long,
        global = true,
        value_name = "VERSION",
        hide = true,
        conflicts_with = "dev"
    )]
    pub staging: Option<u8>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run a job of your project locally or with --remote, or the project via `cargo run` (forwards args after `--`).
    Run(commands::run::RunArgs),

    /// Package your project for running on a remote machine.
    Package(commands::package::PackageArgs),
    /// Log in to the Tracel server.
    Login(commands::login::LoginArgs),
    /// Log out of the Tracel server.
    Logout,
    /// Show which credential commands use, or print an access token for scripts.
    Auth(commands::auth::AuthArgs),
    /// Browse and compare project experiments, metrics, and logs.
    #[command(visible_alias = "exp")]
    Experiments(commands::experiments::ExperimentsArgs),
    /// Browse, follow and cancel jobs in the selected project.
    Jobs(commands::jobs::JobsArgs),
    /// Initialize a new project or reinitialize an existing one.
    Init(commands::init::InitArgs),
    /// Unlink the Tracel Console project from this repository.
    Unlink(commands::unlink::UnlinkArgs),
    /// Display current user information.
    Me,
    /// Display the current project, or list projects.
    Project(commands::project::ProjectArgs),
    /// Browse and manage models in the model registry.
    Models(commands::models::ModelsArgs),
    /// Browse and download experiment artifacts.
    Artifacts(commands::artifacts::ArtifactsArgs),
    /// Browse datasets in the selected project.
    Datasets(commands::datasets::DatasetsArgs),
}

impl Commands {
    /// What `auto` output means: human on a terminal and JSON otherwise, except where
    /// stdout carries plain text: the bare token for `$(tracel auth token)`, or the
    /// output of the program `run` hands over to without a job, `--list` or `--remote`.
    fn auto_format(&self, stdout_is_terminal: bool) -> Format {
        match self {
            Self::Run(commands::run::RunArgs {
                job: None,
                list: false,
                remote: None,
                ..
            })
            | Self::Auth(commands::auth::AuthArgs {
                command: commands::auth::AuthCommands::Token,
            }) => Format::Human,
            _ => Format::for_stdout(stdout_is_terminal),
        }
    }
}

pub fn cli_main() {
    let args = match CliArgs::try_parse() {
        Ok(args) => args,
        Err(error) if !error.use_stderr() => error.exit(),
        Err(error) => {
            let format = usage_format(std::env::args_os().skip(1));
            if format == Format::Human {
                error.exit();
            }
            let message = error.to_string();
            let message = message
                .trim()
                .strip_prefix("error: ")
                .unwrap_or(message.trim());
            fail_early(&CliError::new(ErrorKind::Usage, message).into(), format);
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
                    "Cannot change directory to '{}': {error}",
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
    let (output, terminal) = ui::channels(format, args.no_input);
    let environment_value =
        std::env::var_os("TRACEL_ENV").map(|value| value.to_string_lossy().into_owned());
    let environment = resolve_environment(args.dev, args.staging, environment_value.as_deref())
        .unwrap_or_else(|error| fail(&error.into(), &output, &terminal));

    if args.dev {
        terminal
            .print_warning("Running in development mode - using local server and dev credentials");
    }

    let context = CliContext::new(terminal.clone(), output.clone(), environment, args.project);
    if let Err(error) = handle_command(command, context).and_then(|outcome| output.finish(outcome))
    {
        fail(&error, &output, &terminal);
    }
}

fn resolve_environment(
    dev: bool,
    version: Option<u8>,
    value: Option<&str>,
) -> Result<Env, CliError> {
    if dev {
        return Ok(Env::Development);
    }
    if let Some(version) = version {
        return Ok(Env::Staging(version));
    }
    match value {
        None | Some("Production" | "production") => Ok(Env::Production),
        Some("Development" | "development") => Ok(Env::Development),
        Some(value) => value
            .strip_prefix("Staging(")
            .and_then(|number| number.strip_suffix(')'))
            .or_else(|| value.strip_prefix("staging-"))
            .and_then(|number| number.parse::<u8>().ok())
            .map(Env::Staging)
            .ok_or_else(|| {
                CliError::new(
                    ErrorKind::Usage,
                    format!("Invalid TRACEL_ENV value '{value}'."),
                )
            }),
    }
}

/// Reports a failed command on both channels and exits with its code. A reader that
/// closed stdout got all it wanted, so that ends the command quietly.
fn fail(error: &anyhow::Error, output: &Output, terminal: &Terminal) -> ! {
    if error.chain().any(|cause| cause.is::<StdoutClosed>()) {
        std::process::exit(0);
    }
    let report = ErrorReport::new(error);
    output.error(&report);
    terminal.error(&report);
    std::process::exit(report.exit_code)
}

/// Fails before the command's channels exist.
fn fail_early(error: &anyhow::Error, format: Format) -> ! {
    let (output, terminal) = ui::channels(format, true);
    fail(error, &output, &terminal)
}

/// The format for an error in arguments that did not parse.
fn usage_format(arguments: impl IntoIterator<Item = std::ffi::OsString>) -> Format {
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
    let auto = Format::for_stdout(std::io::stdout().is_terminal());
    Format::resolve(
        flag,
        json,
        std::env::var("TRACEL_OUTPUT").ok().as_deref(),
        auto,
    )
    .unwrap_or(auto)
}

fn handle_command(command: Commands, context: CliContext) -> anyhow::Result<Outcome> {
    match command {
        Commands::Run(run_args) => commands::run::handle_command(run_args, context),
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
    use super::*;

    #[test]
    fn command_definitions_are_valid() {
        CliArgs::command().debug_assert();
        assert!(
            CliArgs::try_parse_from(["tracel", "model", "upload", "weights", "-d", "."]).is_err()
        );
    }

    #[test]
    fn run_forwards_only_arguments_after_double_dash() {
        let forwarded = |arguments: &[&str]| {
            let Some(Commands::Run(run)) = CliArgs::try_parse_from(arguments).unwrap().command
            else {
                panic!("Expected run command");
            };
            run.forwarded
        };
        assert!(forwarded(&["tracel", "run"]).is_empty());
        assert_eq!(
            forwarded(&["tracel", "run", "--", "train", "--epochs", "10"]),
            ["train", "--epochs", "10"]
        );
        assert_eq!(
            forwarded(&["tracel", "run", "--json", "--", "--json"]),
            ["--json"]
        );
        for arguments in [
            &["tracel", "run", "--epochs"][..],
            &["tracel", "run", "train", "--epochs"],
            &["tracel", "run", "train", "evaluate"],
        ] {
            let error = CliArgs::try_parse_from(arguments).unwrap_err();
            assert_eq!(error.exit_code(), 2);
        }
    }

    #[test]
    fn project_and_directory_flags_are_global() {
        for arguments in [
            ["tracel", "--project", "alice/demo", "-C", "/tmp", "project"],
            ["tracel", "project", "--project", "alice/demo", "-C", "/tmp"],
        ] {
            let args = CliArgs::try_parse_from(arguments).unwrap();
            let project = args.project.unwrap();
            assert_eq!(project.owner, "alice");
            assert_eq!(project.name, "demo");
            assert_eq!(
                args.working_directory.unwrap(),
                std::path::Path::new("/tmp")
            );
        }
        let args = CliArgs::try_parse_from([
            "tracel",
            "models",
            "push",
            "weights",
            "-d",
            ".",
            "--project",
            "alice/demo",
        ])
        .unwrap();
        assert!(args.working_directory.is_none());
        assert_eq!(args.project.unwrap().name, "demo");
        let error = CliArgs::try_parse_from(["tracel", "--project", "bad", "project"]).unwrap_err();
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn working_directory_is_separate_from_upload_directory() {
        let args = CliArgs::try_parse_from([
            "tracel",
            "models",
            "push",
            "weights",
            "-d",
            "./weights",
            "-C",
            "/tmp",
        ])
        .unwrap();
        assert_eq!(
            args.working_directory.unwrap(),
            std::path::Path::new("/tmp")
        );
        let Some(Commands::Models(model)) = args.command else {
            panic!("Expected models push command");
        };
        let commands::models::ModelsCommands::Push(upload) = model.command else {
            panic!("Expected models push command");
        };
        assert_eq!(upload.directory, std::path::Path::new("./weights"));
    }

    #[test]
    fn models_push_rejects_removed_flags() {
        for flag in ["--namespace", "-n", "-p"] {
            let error = CliArgs::try_parse_from([
                "tracel", "models", "push", "weights", "-d", ".", flag, "alice",
            ])
            .unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
        }
    }

    #[test]
    fn environment_values_match_sdk_and_lowercase_forms() {
        for value in [None, Some("Production"), Some("production")] {
            assert!(matches!(
                resolve_environment(false, None, value),
                Ok(Env::Production)
            ));
        }
        for value in ["Development", "development"] {
            assert!(matches!(
                resolve_environment(false, None, Some(value)),
                Ok(Env::Development)
            ));
        }
        for version in [0, 1, 255] {
            for value in [format!("Staging({version})"), format!("staging-{version}")] {
                assert!(matches!(
                    resolve_environment(false, None, Some(&value)),
                    Ok(Env::Staging(number)) if number == version
                ));
            }
        }
        for value in [
            "bogus",
            "",
            "Staging()",
            "Staging(1",
            "staging-",
            "staging--1",
            "Staging(256)",
            "staging-256",
            "Production ",
        ] {
            let error = resolve_environment(false, None, Some(value)).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Usage);
            assert!(error.to_string().contains("TRACEL_ENV"));
        }
    }

    #[test]
    fn environment_flags_override_variable() {
        for value in [None, Some("Production"), Some("Development"), Some("bogus")] {
            assert!(matches!(
                resolve_environment(true, None, value),
                Ok(Env::Development)
            ));
            assert!(matches!(
                resolve_environment(false, Some(2), value),
                Ok(Env::Staging(2))
            ));
        }
        assert_eq!(
            CliArgs::try_parse_from(["tracel", "--dev", "--staging", "1", "me"])
                .unwrap_err()
                .kind(),
            clap::error::ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn output_flags_are_global_and_conflict() {
        for arguments in [
            vec!["tracel", "me", "--json"],
            vec!["tracel", "--json", "me"],
        ] {
            assert!(CliArgs::try_parse_from(arguments).unwrap().json);
        }
        assert_eq!(
            CliArgs::try_parse_from(["tracel", "auth", "status", "-o", "human"])
                .unwrap()
                .output,
            Some(FormatArg::Human)
        );
        let error = CliArgs::try_parse_from(["tracel", "me", "--json", "-o", "json"]).unwrap_err();
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn auto_output_is_text_where_another_program_reads_stdout() {
        for arguments in [
            vec!["tracel", "auth", "token"],
            vec!["tracel", "run", "--", "--epochs", "1"],
        ] {
            let command = CliArgs::try_parse_from(arguments).unwrap().command.unwrap();
            for stdout_is_terminal in [false, true] {
                assert_eq!(command.auto_format(stdout_is_terminal), Format::Human);
            }
        }
        for arguments in [
            vec!["tracel", "auth", "status"],
            vec!["tracel", "me"],
            vec!["tracel", "run", "--remote", "gpu", "--", "--epochs", "1"],
            vec!["tracel", "run", "--list"],
            vec!["tracel", "run", "train", "--set", "epochs=1"],
        ] {
            let command = CliArgs::try_parse_from(arguments).unwrap().command.unwrap();
            assert_eq!(command.auto_format(true), Format::Human);
            assert_eq!(command.auto_format(false), Format::Json);
        }
    }

    #[test]
    fn no_input_is_global() {
        for arguments in [
            ["tracel", "--no-input", "unlink"],
            ["tracel", "unlink", "--no-input"],
        ] {
            assert!(CliArgs::try_parse_from(arguments).unwrap().no_input);
        }
    }
}
