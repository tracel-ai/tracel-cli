use directories::ProjectDirs;

use std::{fs, io, path::PathBuf};

pub use tracel_client::console::Env as Environment;

pub trait ToFileSuffix {
    fn file_suffix(&self) -> Option<String>;
}

impl ToFileSuffix for Environment {
    fn file_suffix(&self) -> Option<String> {
        match self {
            Environment::Production => None,
            Environment::Staging(version) => Some(format!("staging{}", version)),
            Environment::Development => Some("dev".to_string()),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("Missing configuration directory")]
    MissingDirectory,
}

pub struct AppConfig {
    base_dir: PathBuf,
    environment: Environment,
}

impl AppConfig {
    pub fn new(environment: Environment) -> Result<Self, ConfigError> {
        let proj_dirs = ProjectDirs::from("", "", "tracel").ok_or(ConfigError::MissingDirectory)?;

        let config_dir = proj_dirs.config_dir().to_path_buf();

        Ok(Self {
            base_dir: config_dir,
            environment,
        })
    }

    fn credentials_path(&self) -> PathBuf {
        let filename = self
            .environment
            .file_suffix()
            .map_or("credentials.json".to_string(), |suffix| {
                format!("credentials-{}.json", suffix)
            });
        self.base_dir.join(filename)
    }

    /// Deletes the API key earlier versions of the CLI stored, if any.
    pub fn delete_legacy_credentials(&self) -> Result<(), ConfigError> {
        match fs::remove_file(self.credentials_path()) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

/// Render the value the SDK expects in the `TRACEL_ENV` environment variable.
///
/// The SDK parses this string with an explicit match (see `discover_env` in the
/// `tracel-core` cloud backend). This need to match.
pub fn tracel_env_value(env: &Environment) -> String {
    match env {
        Environment::Production => "Production".to_string(),
        Environment::Development => "Development".to_string(),
        Environment::Staging(version) => format!("Staging({version})"),
    }
}
