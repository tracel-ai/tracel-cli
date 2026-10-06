use crate::commands::login::environment_suffix;
use crate::context::CliContext;

pub fn handle_command(context: CliContext) -> anyhow::Result<()> {
    context.terminal().command_title("Logout");

    let app_session = context.app_session()?;
    let was_logged_in = app_session.stored()?.is_some();
    app_session.sign_out()?;

    let env_msg = environment_suffix(&context.environment());
    if was_logged_in {
        context
            .terminal()
            .finalize(&format!("Logged out{}.", env_msg));
    } else {
        context
            .terminal()
            .finalize(&format!("Nobody was logged in{}.", env_msg));
    }

    Ok(())
}
