use serde::Serialize;

use crate::context::CliContext;
use crate::ui::{Outcome, Render};

#[derive(Serialize)]
struct LoggedOut {
    logged_out: bool,
}

impl Render for LoggedOut {}

pub fn handle_command(context: CliContext) -> anyhow::Result<Outcome> {
    let app_session = context.app_session()?;
    let was_logged_in = app_session.stored()?.is_some();
    app_session.sign_out()?;

    context.terminal().outro(if was_logged_in {
        "Logged out."
    } else {
        "Already logged out."
    });

    Ok(LoggedOut {
        logged_out: was_logged_in,
    }
    .into())
}
