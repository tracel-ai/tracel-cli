use std::io::{self, Write};
use std::time::SystemTime;

use clap::{Args, Subcommand};
use serde::Serialize;
use tracel_client::ClientError;
use tracel_client::console::TracelCredentials;

use crate::commands::login::environment_suffix;
use crate::context::{CliContext, ClientCreationError};
use crate::error::{CliError, ErrorKind};
use crate::output::{Details, Outcome, Render, Timestamp};

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

pub fn handle_command(args: AuthArgs, context: CliContext) -> anyhow::Result<Outcome> {
    match args.command {
        AuthCommands::Token => Ok(access_token(&context)?.into()),
        AuthCommands::Status => Ok(status(&context)?.into()),
    }
}

/// Printed bare for `$(tracel auth token)`.
#[derive(Serialize)]
struct AccessToken {
    access_token: String,
}

impl Render for AccessToken {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        writeln!(out, "{}", self.access_token)
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Credential {
    ApiKey,
    Login,
    None,
}

#[derive(Serialize)]
struct AuthStatus {
    credential: Credential,
    login_ends_at: Option<Timestamp>,
    user: User,
    environment: String,
}

#[derive(Serialize)]
struct User {
    username: String,
    namespace: String,
}

impl Render for AuthStatus {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        let credential = match self.credential {
            Credential::ApiKey => "TRACEL_API_KEY",
            Credential::Login => "login",
            Credential::None => "none",
        };
        let now = SystemTime::now();
        let login = match self.login_ends_at {
            Some(Timestamp(end)) if now >= end => "ended".to_string(),
            Some(Timestamp(end)) => format!("ends in {}", time_left(end, now)),
            None => "none".to_string(),
        };
        Details::new()
            .field("Credential", credential)
            .field("Login", login)
            .field("User", &self.user.username)
            .field("Namespace", &self.user.namespace)
            .field("Environment", &self.environment)
            .write(out)
    }
}

fn access_token(context: &CliContext) -> anyhow::Result<AccessToken> {
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

    Ok(AccessToken {
        access_token: access_token.as_str().to_string(),
    })
}

fn status(context: &CliContext) -> anyhow::Result<AuthStatus> {
    let client = context.create_client();
    let login = context.app_session()?.stored()?;

    let credential = match (TracelCredentials::from_env().is_ok(), &login) {
        (true, _) => Credential::ApiKey,
        (false, Some(_)) => Credential::Login,
        (false, None) => Credential::None,
    };

    match client {
        Ok(client) => Ok(AuthStatus {
            credential,
            login_ends_at: login.map(|login| Timestamp(login.refresh_token_expires_at)),
            user: User {
                username: client.user().username.clone(),
                namespace: client.user().namespace.clone(),
            },
            environment: context.environment_name(),
        }),
        Err(ClientCreationError::NoCredentials | ClientCreationError::InvalidCredentials) => {
            Err(CliError::new(
                ErrorKind::NotAuthenticated,
                format!(
                    "Not logged in{}. Run 'tracel login' or set TRACEL_API_KEY.",
                    environment_suffix(&context.environment())
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
