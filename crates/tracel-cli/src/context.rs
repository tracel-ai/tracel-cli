use std::sync::Arc;

use crate::tools::tracel_config::TracelProject;
use crate::ui::{Output, Terminal};
use tracel_client::ClientError;
use tracel_client::console::auth::DeviceAuthClient;
use tracel_client::console::{AppSession, Client, Env, FileSessionStore, TracelCredentials};
use url::Url;

const CLIENT_ID: &str = "tracel-cli";

#[derive(thiserror::Error, Debug)]
pub enum ClientCreationError {
    #[error("No credentials found")]
    NoCredentials,
    #[error("Invalid credentials")]
    InvalidCredentials,
    #[error("The API key in TRACEL_API_KEY was refused: {0}")]
    ApiKeyRefused(ClientError),
    #[error(transparent)]
    SessionStore(ClientError),
    #[error("Server connection error: {0}")]
    ServerConnectionError(String),
}

/// What a command runs with: the user's two channels, and the environment and project
/// it targets.
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

    pub fn app_session(&self) -> Result<AppSession, ClientError> {
        let store = FileSessionStore::for_server(&self.environment.get_url())?;
        Ok(AppSession::new(Arc::new(store), self.device_auth()))
    }

    /// `TRACEL_API_KEY` if set, else the login stored for this environment's server.
    fn credentials(&self) -> Result<Option<TracelCredentials>, ClientError> {
        if let Ok(credentials) = TracelCredentials::from_env() {
            return Ok(Some(credentials));
        }

        let app_session = self.app_session()?;
        Ok(app_session
            .stored()?
            .map(|_| TracelCredentials::app_session(app_session)))
    }

    pub fn create_client(&self) -> Result<Client, ClientCreationError> {
        let credentials = self
            .credentials()
            .map_err(ClientCreationError::SessionStore)?
            .ok_or(ClientCreationError::NoCredentials)?;
        let uses_api_key = matches!(credentials, TracelCredentials::ApiKey(_));

        Client::connect(self.environment(), &credentials).map_err(|e| match e {
            ClientError::SessionStore(_) => ClientCreationError::SessionStore(e),
            ClientError::Unauthenticated | ClientError::CredentialNotAllowed if uses_api_key => {
                ClientCreationError::ApiKeyRefused(e)
            }
            ClientError::Unauthenticated | ClientError::AppSessionEnded => {
                ClientCreationError::InvalidCredentials
            }
            _ => ClientCreationError::ServerConnectionError(e.to_string()),
        })
    }

    /// The web Console of the environment.
    pub fn get_frontend_endpoint(&self) -> Url {
        web_console(&self.environment)
    }

    pub fn terminal(&self) -> &Terminal {
        &self.terminal
    }

    pub fn environment(&self) -> Env {
        self.environment.clone()
    }

    /// Stdout, for commands that stream their output while they run.
    pub fn output(&self) -> &Output {
        &self.output
    }

    pub fn project(&self) -> Option<&TracelProject> {
        self.project.as_ref()
    }

    pub fn environment_name(&self) -> String {
        environment_name(&self.environment)
    }

    pub fn get_api_endpoint(&self) -> String {
        self.environment.get_url().to_string()
    }
}

/// The web Console of an environment: production and staging serve it beside their
/// API, and a development server serves it on port 9000.
fn web_console(environment: &Env) -> Url {
    let url = match environment {
        Env::Production => "https://console.tracel.ai/".to_string(),
        Env::Staging(version) => format!("https://s{version}-console.tracel.ai/"),
        Env::Development => "http://localhost:9000/".to_string(),
    };
    Url::parse(&url).expect("a web Console URL is valid")
}

/// The name of an environment, as `TRACEL_ENV` and JSON data write it.
pub fn environment_name(environment: &Env) -> String {
    match environment {
        Env::Production => "production".to_string(),
        Env::Development => "development".to_string(),
        Env::Staging(version) => format!("staging-{version}"),
    }
}
