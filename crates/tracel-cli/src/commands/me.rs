use std::io;

use anyhow::Context;
use serde::Serialize;

use crate::commands::login::{environment_suffix, get_client_and_login_if_needed};
use crate::context::CliContext;
use crate::ui::{Details, Human, Outcome, Render};

#[derive(Serialize)]
struct CurrentUser {
    username: String,
    email: Option<String>,
    namespace: String,
    environment: String,
    api_url: String,
    /// " (development environment)" and the like, empty for production.
    #[serde(skip)]
    environment_suffix: String,
}

impl Render for CurrentUser {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        Details::new()
            .field("Username", &self.username)
            .optional("Email", self.email.as_ref())
            .field("Namespace", &self.namespace)
            .field("Environment", &self.environment)
            .field("API URL", &self.api_url)
            .write(out)
    }

    fn summary(&self) -> Option<String> {
        Some(format!(
            "Signed in as {}{}.",
            self.username, self.environment_suffix
        ))
    }
}

pub fn handle_command(context: CliContext) -> anyhow::Result<Outcome> {
    let client = get_client_and_login_if_needed(&context)?;
    let user = client
        .get_current_user()
        .context("Failed to retrieve user information")?;

    Ok(CurrentUser {
        username: user.username,
        email: user.email,
        namespace: user.namespace,
        environment: context.environment_name(),
        api_url: context.get_api_endpoint(),
        environment_suffix: environment_suffix(&context.environment()),
    }
    .into())
}
