use std::io;

use anyhow::Context;
use serde::Serialize;

use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::ui::{Details, Human, Outcome, Render};

#[derive(Serialize)]
struct CurrentUser {
    username: String,
    email: Option<String>,
    namespace: String,
    environment: String,
    api_url: String,
}

impl Render for CurrentUser {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        Details::new()
            .optional("Email", self.email.as_ref())
            .field("Namespace", &self.namespace)
            .field("API URL", &self.api_url)
            .write(out)
    }

    fn summary(&self) -> Option<String> {
        Some(format!("Logged in as {}.", self.username))
    }
}

pub fn handle_command(context: CliContext) -> anyhow::Result<Outcome> {
    let client = get_client_and_login_if_needed(&context)?;
    let user = client.get_current_user().context("Can't read your user")?;

    Ok(CurrentUser {
        username: user.username,
        email: user.email,
        namespace: user.namespace,
        environment: context.environment_name(),
        api_url: context.api_url().to_string(),
    }
    .into())
}
