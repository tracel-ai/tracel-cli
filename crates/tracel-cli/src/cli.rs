use std::io::IsTerminal;

use clap::{CommandFactory, Parser, Subcommand};
use serde_json::{Value, json};
use tracel_client::console::Env;

use crate::commands;
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind, ErrorReport};
use crate::helpers::project::parse_project;
use crate::output::{self, OutputMode};
use crate::tools::terminal::Terminal;
use crate::tools::tracel_config::TracelProject;

#[derive(Parser, Debug)]
#[clap(name = "tracel", author, version, about, long_about = None)]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Output format (default: auto)
    #[arg(short, long, global = true, value_enum, conflicts_with = "json")]
    pub output: Option<OutputMode>,

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
    /// Run your project locally via `cargo run` (forwards args after `--`).
    Train(commands::training::TrainingArgs),

    /// Package your project for running on a remote machine.
    Package(commands::package::PackageArgs),
    /// Log in to the Tracel server.
    Login(commands::login::LoginArgs),
    /// Log out of the Tracel server.
    Logout,
    /// Show which credential commands use, or print an access token for scripts.
    Auth(commands::auth::AuthArgs),
    /// Initialize a new project or reinitialize an existing one.
    Init(commands::init::InitArgs),
    /// Unlink the Tracel Console project from this repository.
    Unlink(commands::unlink::UnlinkArgs),
    /// Display current user information.
    Me,
    /// Display current project information.
    Project,
    /// Upload local files as a model version in the model registry.
    Model(commands::model::ModelArgs),
}

pub fn cli_main() {
    let args = match CliArgs::try_parse() {
        Ok(args) => args,
        Err(error) if !error.use_stderr() => error.exit(),
        Err(error) => {
            let mode = usage_output_mode(std::env::args_os().skip(1));
            if mode == OutputMode::Json {
                let message = error.to_string();
                let message = message
                    .trim()
                    .strip_prefix("error: ")
                    .unwrap_or(message.trim());
                let error = anyhow::Error::new(CliError::new(ErrorKind::Usage, message));
                report_error(&error, mode, &Terminal::new(mode));
            } else {
                error.exit();
            }
            return;
        }
    };

    if let Some(directory) = &args.working_directory {
        if let Err(error) = std::env::set_current_dir(directory) {
            let mode = usage_output_mode(std::env::args_os().skip(1));
            let error = CliError::new(
                ErrorKind::Usage,
                format!(
                    "Cannot change directory to '{}': {error}",
                    directory.display()
                ),
            );
            report_error(&error.into(), mode, &Terminal::new(mode));
            return;
        }
    }

    let Some(command) = args.command else {
        // A reader that closes early (`tracel | head`) is not an error.
        let _ = CliArgs::command().print_help();
        return;
    };

    let mode = match OutputMode::resolve(
        args.output,
        args.json,
        std::env::var("TRACEL_OUTPUT").ok().as_deref(),
        std::io::stdout().is_terminal(),
    ) {
        Ok(mode) => mode,
        Err(error) => {
            let mode = OutputMode::resolve(None, false, None, std::io::stdout().is_terminal())
                .expect("Auto output is valid");
            report_error(&error.into(), mode, &Terminal::new(mode));
            return;
        }
    };
    // `auth token` prints the bare token for `$(tracel auth token)` unless JSON is asked for.
    let json_requested = args.json
        || args.output == Some(OutputMode::Json)
        || std::env::var("TRACEL_OUTPUT").as_deref() == Ok("json");
    let prints_raw_token = matches!(
        &command,
        Commands::Auth(commands::auth::AuthArgs {
            command: commands::auth::AuthCommands::Token
        })
    ) && !json_requested;
    let mode = if matches!(command, Commands::Train(_)) || prints_raw_token {
        OutputMode::Human
    } else {
        mode
    };

    let terminal = Terminal::new(mode).with_no_input(args.no_input);
    let environment_value =
        std::env::var_os("TRACEL_ENV").map(|value| value.to_string_lossy().into_owned());
    let environment =
        match resolve_environment(args.dev, args.staging, environment_value.as_deref()) {
            Ok(environment) => environment,
            Err(error) => {
                report_error(&error.into(), mode, &terminal);
                return;
            }
        };

    if args.dev {
        terminal
            .print_warning("Running in development mode - using local server and dev credentials");
    }

    let context = CliContext::new(terminal.clone(), environment, mode, args.project);

    let result = handle_command(command, context).and_then(|data| {
        if mode == OutputMode::Json {
            output::write_success(&data)?;
        }
        Ok(())
    });
    if let Err(error) = result {
        report_error(&error, mode, &terminal);
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

fn report_error(error: &anyhow::Error, mode: OutputMode, terminal: &Terminal) {
    let report = ErrorReport::new(error);
    if mode == OutputMode::Json {
        let _ = output::write_failure(&report);
        eprintln!(
            "error: {}",
            console::strip_ansi_codes(&report.message)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        );
    } else {
        terminal.cancel_finalize(&format!("{error:#}"));
        if let Some(hint) = report.hint {
            terminal.print(hint);
        }
    }
    std::process::exit(report.exit_code);
}

fn usage_output_mode(arguments: impl IntoIterator<Item = std::ffi::OsString>) -> OutputMode {
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
                    "human" => OutputMode::Human,
                    "json" => OutputMode::Json,
                    _ => OutputMode::Auto,
                });
            }
        }
    }
    OutputMode::resolve(
        flag,
        json,
        std::env::var("TRACEL_OUTPUT").ok().as_deref(),
        std::io::stdout().is_terminal(),
    )
    .unwrap_or_else(|_| {
        if std::io::stdout().is_terminal() {
            OutputMode::Human
        } else {
            OutputMode::Json
        }
    })
}

fn handle_command(command: Commands, context: CliContext) -> anyhow::Result<Value> {
    match command {
        Commands::Train(run_args) => {
            commands::training::handle_command(run_args, context).map(|()| json!({}))
        }
        Commands::Package(package_args) => commands::package::handle_command(package_args, context),
        Commands::Login(login_args) => commands::login::handle_command(login_args, context),
        Commands::Logout => commands::logout::handle_command(context),
        Commands::Auth(auth_args) => commands::auth::handle_command(auth_args, context),
        Commands::Init(init_args) => commands::init::handle_command(init_args, context),
        Commands::Unlink(unlink_args) => commands::unlink::handle_command(unlink_args, context),
        Commands::Me => commands::me::handle_command(context),
        Commands::Project => commands::project::handle_command(context),
        Commands::Model(model_args) => commands::model::handle_command(model_args, context),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "model",
            "upload",
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
            "model",
            "upload",
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
        let Some(Commands::Model(model)) = args.command else {
            panic!("Expected model upload command");
        };
        let commands::model::ModelCommands::Upload(upload) = model.command;
        assert_eq!(upload.directory, std::path::Path::new("./weights"));
    }

    #[test]
    fn model_upload_rejects_removed_flags() {
        for flag in ["--namespace", "-n", "-p"] {
            let error = CliArgs::try_parse_from([
                "tracel", "model", "upload", "weights", "-d", ".", flag, "alice",
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
            Some(OutputMode::Human)
        );
        let error = CliArgs::try_parse_from(["tracel", "me", "--json", "-o", "json"]).unwrap_err();
        assert_eq!(error.exit_code(), 2);
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
