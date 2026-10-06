use tracel_client::console::{Client, Env, TracelCredentials};
use url::Url;

use crate::context::{CliContext, ClientCreationError};

pub fn get_client_and_login_if_needed(context: &CliContext) -> anyhow::Result<Client> {
    const MAX_RETRIES: u32 = 3;
    let mut attempts = 0;

    loop {
        match context.create_client() {
            Ok(client) => {
                if attempts > 0 {
                    context.terminal().print_success("Successfully logged in!");
                }
                return Ok(client);
            }
            Err(err) => {
                attempts += 1;
                match err {
                    ClientCreationError::InvalidCredentials
                    | ClientCreationError::NoCredentials => {
                        if attempts > MAX_RETRIES {
                            return Err(anyhow::anyhow!("Maximum login attempts exceeded"));
                        }
                        let env_msg = environment_suffix(&context.environment());
                        if !context.terminal().is_interactive() {
                            anyhow::bail!(
                                "Not logged in{}. Run 'tracel login' or set TRACEL_API_KEY.",
                                env_msg
                            );
                        }
                        context.terminal().print_err(&format!(
                            "Not logged in{}. Log in below, or press Ctrl+C to exit.",
                            env_msg
                        ));

                        log_in(context)?;
                    }
                    ClientCreationError::ServerConnectionError(msg) => {
                        if attempts > MAX_RETRIES {
                            return Err(anyhow::anyhow!(
                                "Server connection failed after maximum retries: {}",
                                msg
                            ));
                        }
                        context.terminal().print_err(&format!(
                            "Failed to connect to the server: {msg}. Retrying..."
                        ));
                    }
                    err => return Err(err.into()),
                }
            }
        }
    }
}

pub fn environment_suffix(environment: &Env) -> String {
    match environment {
        Env::Development => " (development environment)".to_string(),
        Env::Staging(version) => format!(" (staging environment v{})", version),
        Env::Production => String::new(),
    }
}

fn log_in(context: &CliContext) -> anyhow::Result<()> {
    let terminal = context.terminal();
    let device_auth = context.device_auth();

    let authorization = device_auth.start()?;
    terminal.print(&format!(
        "Open {} and check that it shows the code {}.",
        terminal.format_url(&Url::parse(&authorization.verification_uri_complete)?),
        console::style(&authorization.user_code).bold()
    ));

    let spinner = terminal.spinner();
    spinner.start("Waiting for approval... Press Ctrl+C to cancel.");
    let issued = device_auth
        .wait_for_approval(&authorization)
        .inspect_err(|_| spinner.error("Login failed."))?;
    spinner.stop("Login approved.");

    context.app_session()?.sign_in(issued)?;

    Ok(())
}

pub fn handle_command(context: CliContext) -> anyhow::Result<()> {
    context.terminal().command_title("Login");

    log_in(&context)?;

    let credentials = TracelCredentials::app_session(context.app_session()?);
    let client = Client::connect(context.environment(), &credentials)?;

    if TracelCredentials::from_env().is_ok() {
        context.terminal().print_warning(
            "TRACEL_API_KEY is set, so other commands use it instead of this login.",
        );
    }
    context.terminal().finalize(&format!(
        "Logged in as {}{}.",
        client.user().username,
        environment_suffix(&context.environment())
    ));

    Ok(())
}
