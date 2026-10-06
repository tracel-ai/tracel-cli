use std::time::SystemTime;

use chrono::{DateTime, Utc};
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tracel_client::ClientError;
use tracel_client::console::TracelCredentials;

use crate::commands::login::environment_suffix;
use crate::context::{CliContext, ClientCreationError};
use crate::error::{CliError, ErrorKind};
use crate::output::OutputMode;

#[derive(Args, Debug)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: AuthCommands,
}

#[derive(Subcommand, Debug)]
pub enum AuthCommands {
    /// Print an access token of your login, renewed if needed, for scripts.
    /// TRACEL_API_KEY is ignored.
    Token,
    /// Show which credential commands use, the user they act as, and when your login ends.
    Status,
}

pub fn handle_command(args: AuthArgs, context: CliContext) -> anyhow::Result<Value> {
    match args.command {
        AuthCommands::Token => print_token(&context),
        AuthCommands::Status => show_status(&context),
    }
}

fn print_token(context: &CliContext) -> anyhow::Result<Value> {
    let access_token = context
        .app_session()?
        .access_token()
        .map_err(|e| -> anyhow::Error {
            match e {
                ClientError::AppSessionEnded => CliError::new(
                    ErrorKind::NotAuthenticated,
                    format!(
                        "Not logged in{}. Run 'tracel login' first.",
                        environment_suffix(&context.environment())
                    ),
                )
                .into(),
                e => e.into(),
            }
        })?;

    if context.output() == OutputMode::Human {
        println!("{}", access_token.as_str());
    }

    Ok(json!({"access_token": access_token.as_str()}))
}

fn show_status(context: &CliContext) -> anyhow::Result<Value> {
    context.terminal().command_title("Authentication Status");

    let client = context.create_client();
    let login = context.app_session()?.stored()?;

    let credential = match (TracelCredentials::from_env().is_ok(), &login) {
        (true, _) => "TRACEL_API_KEY",
        (false, Some(_)) => "login",
        (false, None) => "none",
    };
    context
        .terminal()
        .print(&format!("Credential: {}", credential));

    let now = SystemTime::now();
    let login_status = match &login {
        Some(login) if login.has_ended_at(now) => "ended".to_string(),
        Some(login) => format!("ends in {}", time_left(login.refresh_token_expires_at, now)),
        None => "none".to_string(),
    };
    context
        .terminal()
        .print(&format!("Login: {}", login_status));

    let env_msg = environment_suffix(&context.environment());
    match client {
        Ok(client) => {
            context.terminal().finalize(&format!(
                "Commands act as {}{}.",
                client.user().username,
                env_msg
            ));
            Ok(json!({
                "credential": if credential == "TRACEL_API_KEY" { "api_key" } else { credential },
                "login_ends_at": login.as_ref().map(|login| DateTime::<Utc>::from(login.refresh_token_expires_at).to_rfc3339()),
                "user": {"username": client.user().username, "namespace": client.user().namespace},
                "environment": context.environment_name(),
            }))
        }
        Err(ClientCreationError::NoCredentials | ClientCreationError::InvalidCredentials) => {
            Err(CliError::new(
                ErrorKind::NotAuthenticated,
                format!(
                    "Not logged in{}. Run 'tracel login' or set TRACEL_API_KEY.",
                    env_msg
                ),
            )
            .into())
        }
        Err(e) => Err(e.into()),
    }
}

fn time_left(until: SystemTime, now: SystemTime) -> String {
    let minutes = until.duration_since(now).unwrap_or_default().as_secs() / 60;
    let (days, hours, minutes) = (minutes / (24 * 60), minutes / 60 % 24, minutes % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}
