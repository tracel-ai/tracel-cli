mod pending;

use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

use clap::Args;
use serde::Serialize;
use tracel_client::console::auth::{DeviceAuthClient, DeviceFlowError, DevicePollOutcome};
use tracel_client::console::{Client, TracelCredentials};
use url::Url;

use self::pending::{PendingLogin, PendingLoginStore};
use crate::context::{CliContext, ClientCreationError};
use crate::error::{CliError, ErrorKind, classify};
use crate::ui::{Layout, Outcome, Terminal, Timestamp};

#[derive(Args, Debug)]
pub struct LoginArgs {
    /// Start a login and finish it later with --complete
    #[arg(long, conflicts_with = "complete")]
    pub no_wait: bool,
    /// Finish a login started with --no-wait
    #[arg(long)]
    pub complete: bool,
    /// Stop waiting after this many seconds
    #[arg(long, requires = "complete", value_name = "SECONDS")]
    pub timeout: Option<u64>,
}

impl LoginArgs {
    pub fn layout(&self) -> Layout {
        if self.no_wait {
            Layout::flow("Start a login to the Tracel Console")
        } else if self.complete {
            Layout::flow("Finish the pending login to the Tracel Console")
        } else {
            Layout::flow("Log in to the Tracel Console")
        }
    }
}

/// How long to wait before each new attempt to reach the Tracel Console.
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
];

/// Connect to the Tracel Console, logging in first when there is no login and the user
/// can answer.
pub fn get_client_and_login_if_needed(context: &CliContext) -> anyhow::Result<Client> {
    let mut delays = RETRY_DELAYS.into_iter();
    let mut logged_in = false;
    loop {
        match context.create_client() {
            Ok(client) => return Ok(client),
            Err(
                error @ (ClientCreationError::InvalidCredentials
                | ClientCreationError::NoCredentials),
            ) if !logged_in => {
                if !context.terminal().is_interactive() {
                    return Err(login_needed(&error).into());
                }
                context.terminal().warning(&error.to_string());
                log_in(context)?;
                logged_in = true;
            }
            Err(error @ ClientCreationError::ServerConnectionError(_)) => match delays.next() {
                Some(delay) => std::thread::sleep(delay),
                None => return Err(error.into()),
            },
            Err(error) => return Err(error.into()),
        }
    }
}

/// Build the error for a command that needs a login and has none.
pub fn login_needed(error: &ClientCreationError) -> CliError {
    CliError::new(ErrorKind::NotAuthenticated, error.to_string())
        .with_hint("Run `tracel login`, or set TRACEL_API_KEY.")
}

fn log_in(context: &CliContext) -> anyhow::Result<()> {
    let terminal = context.terminal();
    let device_auth = context.device_auth();

    let authorization = device_auth.start()?;
    let approval_page = Url::parse(&authorization.verification_uri_complete)?;
    terminal.instruct(&approval_instructions(
        terminal,
        &approval_page,
        &authorization.user_code,
    ));
    offer_browser(terminal, &approval_page)?;

    let spinner = terminal.spinner("Waiting for approval");
    let issued = device_auth.wait_for_approval(&authorization)?;
    spinner.clear();

    context.app_session()?.sign_in(issued)?;

    Ok(())
}

/// Ask the user to open `page` and check that it shows `code`.
fn approval_instructions(terminal: &Terminal, page: &Url, code: &str) -> String {
    format!(
        "Open {} and check that it shows the code {}.",
        terminal.format_url(page),
        console::style(code).for_stderr().bold()
    )
}

/// Offer to open the approval page in a browser, when there is one and the user can
/// answer.
fn offer_browser(terminal: &Terminal, approval_page: &Url) -> anyhow::Result<()> {
    if !webbrowser::Browser::is_available() || !terminal.offer("Open it in your browser?")? {
        return Ok(());
    }
    if let Err(error) = webbrowser::open(approval_page.as_str()) {
        terminal.warning(&format!(
            "Can't open a browser ({error}), so open the link yourself."
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct LoggedIn {
    username: String,
    environment: String,
}

/// Serialized with `"status": "pending"`.
#[derive(Serialize)]
#[serde(tag = "status", rename = "pending")]
struct LoginPending {
    verification_uri_complete: String,
    user_code: String,
    expires_at: Timestamp,
}

fn start_pending_login(context: &CliContext) -> anyhow::Result<LoginPending> {
    let authorization = context.device_auth().start()?;
    let pending = PendingLogin::new(authorization, SystemTime::now())?;
    let store = PendingLoginStore::for_server(&context.environment().get_url())?;
    store.save(&pending)?;

    let terminal = context.terminal();
    let approval_page = Url::parse(&pending.authorization.verification_uri_complete)?;
    terminal.info(&approval_instructions(
        terminal,
        &approval_page,
        &pending.authorization.user_code,
    ));
    terminal.outro("Once you approve it, run `tracel login --complete`.");

    Ok(LoginPending {
        verification_uri_complete: pending.authorization.verification_uri_complete,
        user_code: pending.authorization.user_code,
        expires_at: Timestamp(pending.expires_at),
    })
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
            .with_hint("Start a new one with `tracel login --no-wait`."),
            Self::Timeout => CliError::new(ErrorKind::Timeout, "The login is still pending.")
                .with_hint("Run `tracel login --complete` again."),
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
            Err(anyhow::Error::new(error).context("Can't check the pending login"))
        }
        Ok(Err(error)) => Err(
            CliError::new(ErrorKind::NotAuthenticated, error.to_string())
                .with_hint("Start a new one with `tracel login --no-wait`.")
                .into(),
        ),
        Err(RecvTimeoutError::Disconnected) => {
            Err(anyhow::anyhow!("The pending login check stopped."))
        }
    }
}

fn complete_pending_login(context: &CliContext, timeout: Option<u64>) -> anyhow::Result<()> {
    let started = Instant::now();
    let store = PendingLoginStore::for_server(&context.environment().get_url())?;
    let mut pending = store.load()?.ok_or_else(|| {
        CliError::new(ErrorKind::NotFound, "No pending login.")
            .with_hint("Start one with `tracel login --no-wait`.")
    })?;
    let timeout = timeout.map(Duration::from_secs);
    let expires_at = pending.expires_at;
    // The time left before the earlier deadline, or that deadline's error once it passed.
    let time_left = || match poll_window(expires_at, SystemTime::now(), timeout, started.elapsed())
    {
        (remaining, deadline) if remaining.is_zero() => Err(anyhow::Error::from(deadline.error())),
        window => Ok(window),
    };

    let result = (|| {
        let device_auth = context.device_auth();
        let mut interval = pending.authorization.interval().max(Duration::from_secs(1));
        let (mut remaining, _) = time_left()?;
        loop {
            std::thread::sleep(interval.min(remaining));
            let (left, deadline) = time_left()?;
            match poll_with_deadline(
                &device_auth,
                &pending.authorization.device_code,
                left,
                deadline,
            )? {
                DevicePollOutcome::Approved(issued) => {
                    context.app_session()?.sign_in(issued)?;
                    return Ok(());
                }
                DevicePollOutcome::SlowDown => {
                    interval = poll_backoff(interval);
                    pending.authorization.interval =
                        interval.as_secs().try_into().unwrap_or(i64::MAX);
                    store.save(&pending)?;
                }
                DevicePollOutcome::Pending => {}
            }
            remaining = time_left()?.0;
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

pub fn handle_command(args: LoginArgs, context: CliContext) -> anyhow::Result<Outcome> {
    if args.no_wait {
        return Ok(Outcome::data(start_pending_login(&context)?));
    }
    if args.complete {
        complete_pending_login(&context, args.timeout)?;
    } else {
        log_in(&context)?;
    }

    let credentials = TracelCredentials::app_session(context.app_session()?);
    let client = Client::connect(context.environment(), &credentials)?;

    if TracelCredentials::from_env().is_ok() {
        context
            .terminal()
            .warning("TRACEL_API_KEY is set, so other commands use it instead of this login.");
    }
    context
        .terminal()
        .outro(&format!("Logged in as {}.", client.user().username));

    Ok(Outcome::data(LoggedIn {
        username: client.user().username.clone(),
        environment: context.environment_name(),
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
    fn a_pending_login_says_so() {
        let pending = LoginPending {
            verification_uri_complete: "http://localhost:9001/verify?code=BCDF-GHJK".into(),
            user_code: "BCDF-GHJK".into(),
            expires_at: Timestamp(UNIX_EPOCH + Duration::from_secs(1_900_000_000)),
        };
        assert_eq!(
            serde_json::to_value(pending).unwrap(),
            serde_json::json!({
                "status": "pending",
                "verification_uri_complete": "http://localhost:9001/verify?code=BCDF-GHJK",
                "user_code": "BCDF-GHJK",
                "expires_at": "2030-03-17T17:46:40+00:00",
            })
        );
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
