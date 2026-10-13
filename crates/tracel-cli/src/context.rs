use std::sync::Arc;

use tracel_client::ClientError;
use tracel_client::console::auth::DeviceAuthClient;
use tracel_client::console::{AppSession, Client, Env, FileSessionStore, TracelCredentials};
use url::Url;

use crate::environment;
use crate::tools::tracel_config::TracelProject;
use crate::ui::{Output, Terminal};

const CLIENT_ID: &str = "tracel-cli";

/// Why no client could be made.
#[derive(thiserror::Error, Debug)]
pub enum ClientCreationError {
    #[error("Not logged in.")]
    NoCredentials,
    #[error("Your login has ended.")]
    InvalidCredentials,
    #[error("TRACEL_API_KEY was rejected: it's invalid, expired or revoked.")]
    ApiKeyRefused,
    #[error(transparent)]
    SessionStore(ClientError),
    #[error("Can't reach the Tracel Console at {0}.")]
    ServerConnectionError(Url),
}

/// What a command runs with: stdout and stderr, and the environment and project it
/// targets.
pub struct CliContext {
    terminal: Terminal,
    output: Output,
    environment: Env,
    project: Option<TracelProject>,
}

impl CliContext {
    pub fn new(
        terminal: Terminal,
        output: Output,
        environment: Env,
        project: Option<TracelProject>,
    ) -> Self {
        Self {
            terminal,
            output,
            environment,
            project,
        }
    }

    pub fn device_auth(&self) -> DeviceAuthClient {
        DeviceAuthClient::new(self.environment(), CLIENT_ID)
    }

    /// Open the login stored for this environment's Tracel Console.
    pub fn app_session(&self) -> Result<AppSession, ClientError> {
        let store = FileSessionStore::for_server(&self.environment.get_url())?;
        Ok(AppSession::new(Arc::new(store), self.device_auth()))
    }

    /// Find the credential commands use: `TRACEL_API_KEY` when it is set, else the
    /// stored login.
    pub fn credentials(&self) -> Result<Option<TracelCredentials>, ClientError> {
        first_credentials(TracelCredentials::from_env().ok(), || {
            let app_session = self.app_session()?;
            Ok(app_session
                .stored()?
                .map(|_| TracelCredentials::app_session(app_session)))
        })
    }

    /// Connect to the Tracel Console with the credential commands use.
    pub fn create_client(&self) -> Result<Client, ClientCreationError> {
        let credentials = self
            .credentials()
            .map_err(ClientCreationError::SessionStore)?
            .ok_or(ClientCreationError::NoCredentials)?;
        let uses_api_key = matches!(credentials, TracelCredentials::ApiKey(_));

        Client::connect(self.environment(), &credentials).map_err(|e| match e {
            ClientError::SessionStore(_) => ClientCreationError::SessionStore(e),
            ClientError::Unauthenticated | ClientError::CredentialNotAllowed if uses_api_key => {
                ClientCreationError::ApiKeyRefused
            }
            ClientError::Unauthenticated | ClientError::AppSessionEnded => {
                ClientCreationError::InvalidCredentials
            }
            _ => ClientCreationError::ServerConnectionError(self.environment.get_url()),
        })
    }

    pub fn web_console(&self) -> Url {
        environment::web_console(&self.environment)
    }

    pub fn terminal(&self) -> &Terminal {
        &self.terminal
    }

    /// Return stdout, for commands that stream their output while they run.
    pub fn output(&self) -> &Output {
        &self.output
    }

    pub fn environment(&self) -> Env {
        self.environment.clone()
    }

    pub fn environment_name(&self) -> String {
        environment::name(&self.environment)
    }

    /// Return the project `--project` names, if any.
    pub fn project(&self) -> Option<&TracelProject> {
        self.project.as_ref()
    }

    pub fn api_url(&self) -> Url {
        self.environment.get_url()
    }
}

/// Prefer `api_key`, and read the stored login only without one.
fn first_credentials(
    api_key: Option<TracelCredentials>,
    stored_login: impl FnOnce() -> Result<Option<TracelCredentials>, ClientError>,
) -> Result<Option<TracelCredentials>, ClientError> {
    match api_key {
        Some(api_key) => Ok(Some(api_key)),
        None => stored_login(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracel_api_key_comes_before_a_stored_login() {
        let api_key = TracelCredentials::ApiKey("key".into());

        let chosen = first_credentials(Some(api_key), || panic!("the stored login is read"));

        assert!(matches!(chosen, Ok(Some(TracelCredentials::ApiKey(key))) if key == "key"));
    }
}
