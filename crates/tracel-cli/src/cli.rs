use std::io::IsTerminal;

use clap::{CommandFactory, Parser, Subcommand};
use serde_json::{Value, json};
use tracel_client::console::Env;

use crate::commands;
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind, ErrorReport};
use crate::output::{self, OutputMode};
use crate::tools::terminal::Terminal;

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
    Login,
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

    let environment = if args.dev {
        Env::Development
    } else if let Some(version) = args.staging {
        Env::Staging(version)
    } else {
        Env::Production
    };

    let terminal = Terminal::new(mode).with_no_input(args.no_input);

    if args.dev {
        terminal
            .print_warning("Running in development mode - using local server and dev credentials");
    }

    let context = CliContext::new(terminal.clone(), environment, mode);

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
        Commands::Login => commands::login::handle_command(context),
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
