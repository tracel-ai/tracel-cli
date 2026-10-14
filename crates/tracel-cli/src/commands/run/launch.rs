use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use serde::Serialize;
use tracel_job::{ExitCode, TRACEL_DESCRIBE};

use crate::error::{CliError, ErrorKind};

/// How long a program has to stop after it is asked to, before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(30);
/// How often the program and the signals are checked.
const TICK: Duration = Duration::from_millis(100);

/// Where a job run records its experiment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Console,
    Offline,
}

impl Target {
    /// The value of `TRACEL_TARGET`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Console => "console",
            Self::Offline => "offline",
        }
    }
}

/// Where to record a run: the Console when a project is linked and a credential is
/// available, unless `offline` is asked for. Also returns why the run is offline when it was
/// not asked to be.
pub fn select_target(
    offline: bool,
    linked: bool,
    logged_in: bool,
) -> (Target, Option<&'static str>) {
    if offline {
        return (Target::Offline, None);
    }
    if !linked {
        return (
            Target::Offline,
            Some(
                "Running offline: no Tracel Console project is linked (run `tracel init` or pass --project).",
            ),
        );
    }
    if !logged_in {
        return (
            Target::Offline,
            Some("Running offline: not logged in (run `tracel login` or set TRACEL_API_KEY)."),
        );
    }
    (Target::Console, None)
}

/// A job to run with the program built from the workspace.
pub struct Launch<'a> {
    pub program: &'a Path,
    pub job: &'a str,
    pub input: &'a str,
    pub env: Vec<(&'static str, String)>,
    /// Where the program writes its stdout.
    pub stdout: Stdio,
}

/// How the program ended, and whether `tracel` stopped it.
#[derive(Clone, Copy, Debug)]
pub struct Exit {
    pub status: ExitStatus,
    pub stopped: bool,
}

/// Something that happened while the program ran.
pub enum Event {
    /// A signal was forwarded to the program for the first time.
    Stopping,
    /// The program was still running 30 seconds after the first signal, and was killed.
    Killed,
}

/// Run `<program> <job> <input>` and wait for it, calling `on_event` as it runs.
///
/// SIGINT, SIGTERM and SIGHUP are forwarded to the program as SIGTERM, and the program is
/// killed when it is still running 30 seconds after the first. The program runs in its own
/// process group with stdin closed, so a Ctrl-C in the terminal reaches it only as that
/// SIGTERM. On Windows, the console delivers Ctrl-C to the program itself, and the program
/// is killed 30 seconds later.
pub fn launch(launch: Launch, mut on_event: impl FnMut(Event)) -> anyhow::Result<Exit> {
    let signals = Arc::new(AtomicUsize::new(0));
    {
        let signals = signals.clone();
        ctrlc::set_handler(move || {
            signals.fetch_add(1, Ordering::SeqCst);
        })
        .context("Can't handle termination signals")?;
    }

    let mut command = Command::new(launch.program);
    command
        .arg(launch.job)
        .arg(launch.input)
        .env_remove(TRACEL_DESCRIBE)
        .envs(launch.env)
        .stdin(Stdio::null())
        .stdout(launch.stdout);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("Can't run {}", launch.program.display()))?;

    let mut forwarded = 0;
    let mut stop_requested_at = None;
    let mut killed = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let received = signals.load(Ordering::SeqCst);
        if received > forwarded {
            forwarded = received;
            request_stop(&child);
            if stop_requested_at.is_none() {
                stop_requested_at = Some(Instant::now());
                on_event(Event::Stopping);
            }
        }
        if let Some(requested_at) = stop_requested_at
            && !killed
            && requested_at.elapsed() >= STOP_GRACE
        {
            let _ = child.kill();
            killed = true;
            on_event(Event::Killed);
        }
        std::thread::sleep(TICK);
    };

    Ok(Exit {
        status,
        stopped: stop_requested_at.is_some(),
    })
}

#[cfg(unix)]
fn request_stop(child: &Child) {
    let Ok(pid) = libc::pid_t::try_from(child.id()) else {
        return;
    };
    // SAFETY: `kill` has no memory effects; `pid` is a child not yet waited for.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
}

/// The console already delivered the Ctrl-C to the program.
#[cfg(not(unix))]
fn request_stop(_child: &Child) {}

/// Build the error a run of `job` that ended with `exit` fails with, unless the job
/// completed: `JOB_FAILED` when the job failed or was stopped, and `USAGE` when the
/// program rejected it.
pub fn exit_error(job: &str, exit: &Exit) -> Option<CliError> {
    if exit.stopped {
        return Some(CliError::new(
            ErrorKind::JobFailed,
            format!("Stopped job '{job}'."),
        ));
    }
    let code = exit.status.code();
    let how = match (code.and_then(ExitCode::from_code), code) {
        (Some(ExitCode::Completed), _) => return None,
        (Some(ExitCode::Rejected), _) => {
            return Some(CliError::new(
                ErrorKind::Usage,
                format!("The program rejected job '{job}' or its input (exit code 2)."),
            ));
        }
        (_, Some(code)) => format!("exit code {code}"),
        (_, None) => match signal(exit.status) {
            Some(signal) => format!("ended by signal {signal}"),
            None => "ended without an exit code".to_string(),
        },
    };
    Some(CliError::new(
        ErrorKind::JobFailed,
        format!("Job '{job}' failed ({how})."),
    ))
}

fn signal(status: ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_linked_project_and_a_credential_select_the_console() {
        assert_eq!(select_target(false, true, true), (Target::Console, None));
    }

    #[test]
    fn offline_is_forced_or_explained() {
        for (linked, logged_in) in [(false, false), (false, true), (true, false), (true, true)] {
            assert_eq!(
                select_target(true, linked, logged_in),
                (Target::Offline, None)
            );
        }
        for logged_in in [false, true] {
            let (target, note) = select_target(false, false, logged_in);
            assert_eq!(target, Target::Offline);
            assert!(note.unwrap().contains("tracel init"));
        }
        let (target, note) = select_target(false, true, false);
        assert_eq!(target, Target::Offline);
        assert!(note.unwrap().contains("tracel login"));
    }

    #[test]
    fn targets_are_named_as_the_sdk_reads_them() {
        for (target, name) in [(Target::Console, "console"), (Target::Offline, "offline")] {
            assert_eq!(target.as_str(), name);
            assert_eq!(serde_json::to_value(target).unwrap(), name);
        }
    }

    #[cfg(unix)]
    #[test]
    fn how_the_program_ended_decides_how_the_run_fails() {
        for (script, stopped, expected) in [
            ("exit 0", false, None),
            (
                "exit 1",
                false,
                Some((ErrorKind::JobFailed, "Job 'mnist' failed (exit code 1).")),
            ),
            (
                "exit 2",
                false,
                Some((
                    ErrorKind::Usage,
                    "The program rejected job 'mnist' or its input (exit code 2).",
                )),
            ),
            (
                "exit 3",
                false,
                Some((ErrorKind::JobFailed, "Job 'mnist' failed (exit code 3).")),
            ),
            (
                "kill -9 $$",
                false,
                Some((
                    ErrorKind::JobFailed,
                    "Job 'mnist' failed (ended by signal 9).",
                )),
            ),
            (
                "exit 0",
                true,
                Some((ErrorKind::JobFailed, "Stopped job 'mnist'.")),
            ),
        ] {
            let status = Command::new("sh").args(["-c", script]).status().unwrap();
            let error = exit_error("mnist", &Exit { status, stopped });
            assert_eq!(
                error.map(|error| (error.kind, error.to_string())),
                expected.map(|(kind, message)| (kind, message.to_string())),
                "{script} {stopped}"
            );
        }
    }
}
