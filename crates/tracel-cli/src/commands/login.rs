mod pending;

use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use clap::Args;
use serde_json::{Value, json};
use tracel_client::console::auth::{DeviceAuthClient, DeviceFlowError, DevicePollOutcome};
use tracel_client::console::{Client, Env, TracelCredentials};
use url::Url;

use self::pending::{PendingLogin, PendingLoginStore};
use crate::context::{CliContext, ClientCreationError};
use crate::error::{CliError, ErrorKind, classify};
use crate::output::OutputMode;

#[derive(Args, Debug)]
pub struct LoginArgs {
    /// Start a login and save it for later completion.
    #[arg(long, conflicts_with = "complete")]
    pub no_wait: bool,
    /// Finish the pending login.
    #[arg(long)]
    pub complete: bool,
    /// Wait at most this many seconds, keeping the pending login on timeout.
    #[arg(long, requires = "complete", value_name = "SECONDS")]
    pub timeout: Option<u64>,
}

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
                            return Err(CliError::new(
                                ErrorKind::NotAuthenticated,
                                "Maximum login attempts exceeded",
                            )
                            .into());
                        }
                        let env_msg = environment_suffix(&context.environment());
                        if !context.terminal().is_interactive() {
                            return Err(CliError::new(
                                ErrorKind::NotAuthenticated,
                                format!(
                                    "Not logged in{}. Run 'tracel login' or set TRACEL_API_KEY.",
                                    env_msg
                                ),
                            )
                            .with_hint(
                                "Run 'tracel login', or 'tracel login --no-wait' then 'tracel login --complete'.",
                            )
                            .into());
                        }
                        context.terminal().print_err(&format!(
                            "Not logged in{}. Log in below, or press Ctrl+C to exit.",
                            env_msg
                        ));

                        log_in(context)?;
                    }
                    ClientCreationError::ServerConnectionError(msg) => {
                        if attempts > MAX_RETRIES {
                            return Err(CliError::new(
                                ErrorKind::Unavailable,
                                format!("Server connection failed after maximum retries: {}", msg),
                            )
                            .into());
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
    let instructions = format!(
        "Open {} and check that it shows the code {}.",
        terminal.format_url(&Url::parse(&authorization.verification_uri_complete)?),
        console::style(&authorization.user_code).bold()
    );
    if context.output() == OutputMode::Json {
        eprintln!("{}", console::strip_ansi_codes(&instructions));
    } else {
        terminal.print(&instructions);
    }

    let spinner = terminal.spinner();
    spinner.start("Waiting for approval... Press Ctrl+C to cancel.");
    let issued = device_auth
        .wait_for_approval(&authorization)
        .inspect_err(|_| spinner.error("Login failed."))?;
    spinner.stop("Login approved.");

    context.app_session()?.sign_in(issued)?;

    Ok(())
}

fn start_pending_login(context: &CliContext) -> anyhow::Result<Value> {
    let authorization = context.device_auth().start()?;
    let pending = PendingLogin::new(authorization, SystemTime::now())?;
    let store = PendingLoginStore::for_server(&context.environment().get_url())?;
    store.save(&pending)?;

    let terminal = context.terminal();
    terminal.print(&format!(
        "Open {} and check that it shows the code {}.",
        terminal.format_url(&Url::parse(
            &pending.authorization.verification_uri_complete
        )?),
        console::style(&pending.authorization.user_code).bold()
    ));
    terminal.print("Then run 'tracel login --complete'.");

    Ok(json!({
        "status": "pending",
        "verification_uri_complete": pending.authorization.verification_uri_complete,
        "user_code": pending.authorization.user_code,
        "expires_at": DateTime::<Utc>::from(pending.expires_at).to_rfc3339(),
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PollDeadline {
    Expiry,
    Timeout,
}

impl PollDeadline {
    fn error(self) -> CliError {
        match self {
            Self::Expiry => CliError::new(
                ErrorKind::NotAuthenticated,
                "The pending login expired before it was approved.",
            )
            .with_hint("Start again with 'tracel login --no-wait'."),
            Self::Timeout => CliError::new(ErrorKind::Timeout, "The login is still pending.")
                .with_hint("Run 'tracel login --complete' again."),
        }
    }
}

fn poll_window(
    expires_at: SystemTime,
    now: SystemTime,
    timeout: Option<Duration>,
    elapsed: Duration,
) -> (Duration, PollDeadline) {
    let remaining = expires_at.duration_since(now).unwrap_or_default();
    if let Some(timeout) = timeout {
        let timeout_remaining = timeout.saturating_sub(elapsed);
        if timeout_remaining < remaining {
            return (timeout_remaining, PollDeadline::Timeout);
        }
    }
    (remaining, PollDeadline::Expiry)
}

fn poll_backoff(interval: Duration) -> Duration {
    interval.saturating_add(Duration::from_secs(5))
}

fn poll_with_deadline(
    device_auth: &DeviceAuthClient,
    device_code: &str,
    remaining: Duration,
    deadline: PollDeadline,
) -> anyhow::Result<DevicePollOutcome> {
    let device_auth = device_auth.clone();
    let device_code = device_code.to_string();
    let (sender, receiver) = mpsc::channel();
    // The client's blocking HTTP timeout can exceed the remaining login time.
    std::thread::spawn(move || {
        let _ = sender.send(device_auth.poll(&device_code));
    });
    match receiver.recv_timeout(remaining) {
        Ok(Ok(outcome)) => Ok(outcome),
        Err(RecvTimeoutError::Timeout) => Err(deadline.error().into()),
        Ok(Err(DeviceFlowError::Client(error))) => {
            Err(anyhow::Error::new(error).context("Failed to check the pending login"))
        }
        Ok(Err(error)) => Err(
            CliError::new(ErrorKind::NotAuthenticated, error.to_string())
                .with_hint("Start again with 'tracel login --no-wait'.")
                .into(),
        ),
        Err(RecvTimeoutError::Disconnected) => Err(anyhow::anyhow!(
            "The pending login check stopped unexpectedly"
        )),
    }
}

fn complete_pending_login(context: &CliContext, timeout: Option<u64>) -> anyhow::Result<()> {
    let started = Instant::now();
    let store = PendingLoginStore::for_server(&context.environment().get_url())?;
    let mut pending = store.load()?.ok_or_else(|| {
        CliError::new(ErrorKind::NotFound, "No pending login found.")
            .with_hint("Run 'tracel login --no-wait' first.")
    })?;
    let timeout = timeout.map(Duration::from_secs);
    let expires_at = pending.expires_at;
    let window = || poll_window(expires_at, SystemTime::now(), timeout, started.elapsed());

    let result = (|| {
        let (remaining, deadline) = window();
        if remaining.is_zero() {
            return Err(deadline.error().into());
        }
        let device_auth = context.device_auth();
        let mut interval = pending.authorization.interval().max(Duration::from_secs(1));
        loop {
            let (remaining, deadline) = window();
            if remaining.is_zero() {
                return Err(deadline.error().into());
            }
            std::thread::sleep(interval.min(remaining));
            let (remaining, deadline) = window();
            if remaining.is_zero() {
                return Err(deadline.error().into());
            }

            let outcome = poll_with_deadline(
                &device_auth,
                &pending.authorization.device_code,
                remaining,
                deadline,
            )?;
            let (remaining, deadline) = window();
            if remaining.is_zero() {
                return Err(deadline.error().into());
            }
            match outcome {
                DevicePollOutcome::Pending => {}
                DevicePollOutcome::SlowDown => {
                    interval = poll_backoff(interval);
                    pending.authorization.interval =
                        interval.as_secs().try_into().unwrap_or(i64::MAX);
                    store.save(&pending)?;
                }
                DevicePollOutcome::Approved(issued) => {
                    context.app_session()?.sign_in(issued)?;
                    return Ok(());
                }
            }
        }
    })();

    // Keep the pending login for timeouts and outages, so the command can run again.
    let finished = match &result {
        Ok(()) => true,
        Err(error) => classify(error) == ErrorKind::NotAuthenticated,
    };
    if finished {
        store.clear()?;
    }
    result
}

pub fn handle_command(args: LoginArgs, context: CliContext) -> anyhow::Result<Value> {
    context.terminal().command_title("Login");

    if args.no_wait {
        return start_pending_login(&context);
    }
    if args.complete {
        complete_pending_login(&context, args.timeout)?;
    } else {
        log_in(&context)?;
    }

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

    Ok(json!({
        "username": client.user().username,
        "environment": context.environment_name(),
    }))
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;

    #[test]
    fn polling_stops_at_the_earlier_deadline() {
        let now = UNIX_EPOCH + Duration::from_secs(1_900_000_000);
        let expires_at = now + Duration::from_secs(60);
        for (timeout, elapsed, remaining, deadline) in [
            (None, 0, 60, PollDeadline::Expiry),
            (Some(90), 0, 60, PollDeadline::Expiry),
            (Some(60), 0, 60, PollDeadline::Expiry),
            (Some(30), 10, 20, PollDeadline::Timeout),
            (Some(30), 30, 0, PollDeadline::Timeout),
            (Some(30), 31, 0, PollDeadline::Timeout),
            (Some(0), 0, 0, PollDeadline::Timeout),
        ] {
            assert_eq!(
                poll_window(
                    expires_at,
                    now,
                    timeout.map(Duration::from_secs),
                    Duration::from_secs(elapsed)
                ),
                (Duration::from_secs(remaining), deadline)
            );
        }
        for timeout in [None, Some(Duration::ZERO), Some(Duration::MAX)] {
            for current in [expires_at, expires_at + Duration::from_secs(1)] {
                assert_eq!(
                    poll_window(expires_at, current, timeout, Duration::ZERO),
                    (Duration::ZERO, PollDeadline::Expiry)
                );
            }
        }
    }

    #[test]
    fn slow_down_adds_five_seconds_without_overflow() {
        let interval = Duration::from_secs(7);
        assert_eq!(poll_backoff(interval), Duration::from_secs(12));
        assert_eq!(
            poll_backoff(poll_backoff(interval)),
            Duration::from_secs(17)
        );
        assert_eq!(poll_backoff(Duration::MAX), Duration::MAX);
    }
}
