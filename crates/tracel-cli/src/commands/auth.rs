use std::io::{self, Write};
use std::time::SystemTime;

use clap::{Args, Subcommand};
use serde::Serialize;
use tracel_client::ClientError;
use tracel_client::console::TracelCredentials;

use crate::commands::login::login_needed;
use crate::context::{CliContext, ClientCreationError};
use crate::error::{CliError, ErrorKind};
use crate::ui::{Details, Human, Layout, Outcome, Render, Timestamp};

#[derive(Args, Debug)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: AuthCommands,
}

#[derive(Subcommand, Debug)]
pub enum AuthCommands {
    /// Print an access token of your login for scripts
    Token,
    /// Show the credential in use and when your login ends
    Status,
}

impl AuthArgs {
    /// `token` draws nothing: its bare value is read by other programs.
    pub fn layout(&self) -> Layout {
        match self.command {
            AuthCommands::Token => Layout::Bare,
            AuthCommands::Status => Layout::Read,
        }
    }
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
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        writeln!(out, "{}", self.access_token)
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Credential {
    ApiKey,
    Login,
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
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        let credential = match self.credential {
            Credential::ApiKey => "TRACEL_API_KEY",
            Credential::Login => "login",
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
            .field("Namespace", &self.user.namespace)
            .write(out)
    }

    fn summary(&self) -> Option<String> {
        Some(format!("Logged in as {}.", self.user.username))
    }
}

fn access_token(context: &CliContext) -> anyhow::Result<AccessToken> {
    let access_token = context
        .app_session()?
        .access_token()
        .map_err(|e| -> anyhow::Error {
            match e {
                ClientError::AppSessionEnded => {
                    CliError::new(ErrorKind::NotAuthenticated, "Not logged in.")
                        .with_hint("Run `tracel login`.")
                        .into()
                }
                e => e.into(),
            }
        })?;

    Ok(AccessToken {
        access_token: access_token.as_str().to_string(),
    })
}

fn status(context: &CliContext) -> anyhow::Result<AuthStatus> {
    let client = context.create_client().map_err(|error| match error {
        ClientCreationError::NoCredentials | ClientCreationError::InvalidCredentials => {
            login_needed(&error).into()
        }
        error => anyhow::Error::from(error),
    })?;
    let credential = match context.credentials()? {
        Some(TracelCredentials::ApiKey(_)) => Credential::ApiKey,
        _ => Credential::Login,
    };
    let login = context.app_session()?.stored()?;

    Ok(AuthStatus {
        credential,
        login_ends_at: login.map(|login| Timestamp(login.refresh_token_expires_at)),
        user: User {
            username: client.user().username.clone(),
            namespace: client.user().namespace.clone(),
        },
        environment: context.environment_name(),
    })
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    #[test]
    fn the_time_left_on_a_login_reads_in_its_two_largest_units() {
        let now = UNIX_EPOCH + Duration::from_secs(1_900_000_000);
        for (seconds, expected) in [
            (0, "0m"),
            (59, "0m"),
            (45 * 60, "45m"),
            (3 * 3600 + 5 * 60, "3h 5m"),
            (6 * 86400 + 23 * 3600 + 59 * 60, "6d 23h"),
        ] {
            let until = now + Duration::from_secs(seconds);
            assert_eq!(time_left(until, now), expected, "{seconds}");
        }
    }
}
