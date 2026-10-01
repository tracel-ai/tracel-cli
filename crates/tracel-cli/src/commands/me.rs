use crate::app_config::Environment;
use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;

pub fn handle_command(context: CliContext) -> anyhow::Result<()> {
    context.terminal().command_title("User Information");

    let client = get_client_and_login_if_needed(&context)?;

    let user = match client.get_current_user() {
        Ok(user) => user,
        Err(e) => {
            context
                .terminal()
                .cancel_finalize(&format!("Failed to retrieve user information: {}", e));
            anyhow::bail!(e);
        }
    };

    context
        .terminal()
        .print(&format!("Username: {}", user.username));
    if let Some(email) = &user.email {
        context.terminal().print(&format!("Email: {}", email));
    }
    context
        .terminal()
        .print(&format!("Namespace: {}", user.namespace));

    let env_name = match context.environment() {
        Environment::Development => &format!("Development ({})", context.get_api_endpoint()),
        Environment::Staging(_) => &format!("Staging ({})", context.get_api_endpoint()),
        Environment::Production => &format!("Production ({})", context.get_api_endpoint()),
    };

    context
        .terminal()
        .print(&format!("Environment: {}", env_name));
    context
        .terminal()
        .finalize("User information retrieved successfully.");

    Ok(())
}
