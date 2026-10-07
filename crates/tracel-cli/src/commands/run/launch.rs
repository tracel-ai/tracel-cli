use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use serde::Serialize;

use super::report::{ProgramExit, RunReport, read_report};

/// How long a program has to stop after it is asked to, before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(30);
/// How often the report file is read while the program runs.
const REPORT_INTERVAL: Duration = Duration::from_millis(500);
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
    pub report_path: &'a Path,
}

/// A finished run: how the program ended, and its last report.
pub struct Finished {
    pub exit: ProgramExit,
    pub report: Option<RunReport>,
}

/// Something that happened while the program ran.
pub enum Event<'a> {
    /// The program wrote a new report.
    Report(&'a RunReport),
    /// A signal was forwarded to the program for the first time.
    Stopping,
    /// The program was still running 30 seconds after the first signal, and was killed.
    Killed,
    /// The report file the program left is not a report this CLI reads.
    UnreadableReport(&'a str),
}

/// Run `<program> <job> <input>` and wait for it, calling `on_event` as it runs.
///
/// SIGINT, SIGTERM and SIGHUP are forwarded to the program as SIGTERM, and the program is
/// killed when it is still running 30 seconds after the first. The program runs in its own
/// process group with stdin closed, so a Ctrl-C in the terminal reaches it only as that
/// SIGTERM. On Windows, the console delivers Ctrl-C to the program itself, and the program
/// is killed 30 seconds later.
pub fn launch(launch: Launch, mut on_event: impl FnMut(Event)) -> anyhow::Result<Finished> {
    let signals = Arc::new(AtomicUsize::new(0));
    {
        let signals = signals.clone();
        ctrlc::set_handler(move || {
            signals.fetch_add(1, Ordering::SeqCst);
        })
        .context("Failed to handle termination signals")?;
    }

    let mut command = Command::new(launch.program);
    command
        .arg(launch.job)
        .arg(launch.input)
        .env_remove("TRACEL_DESCRIBE")
        .env("TRACEL_REPORT_FILE", launch.report_path)
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
        .with_context(|| format!("Failed to run {}", launch.program.display()))?;

    let mut report = None;
    let mut forwarded = 0;
    let mut stop_requested_at = None;
    let mut killed = false;
    let mut report_read_at = Instant::now();
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
        if let Some(requested_at) = stop_requested_at {
            if !killed && requested_at.elapsed() >= STOP_GRACE {
                let _ = child.kill();
                killed = true;
                on_event(Event::Killed);
            }
        }
        if report_read_at.elapsed() >= REPORT_INTERVAL {
            report_read_at = Instant::now();
            update_report(launch.report_path, &mut report, &mut on_event, false);
        }
        std::thread::sleep(TICK);
    };
    update_report(launch.report_path, &mut report, &mut on_event, true);

    Ok(Finished {
        exit: program_exit(status, stop_requested_at.is_some()),
        report,
    })
}

/// Read the report, and pass it to `on_event` when it changed. A file that is not a report is
/// reported only on the `last` read, since the program may still replace it.
fn update_report(
    path: &Path,
    report: &mut Option<RunReport>,
    on_event: &mut impl FnMut(Event),
    last: bool,
) {
    let latest = match read_report(path) {
        Ok(Some(latest)) => latest,
        Ok(None) => return,
        Err(reason) => {
            if last {
                on_event(Event::UnreadableReport(&reason));
            }
            return;
        }
    };
    if report.as_ref() != Some(&latest) {
        on_event(Event::Report(&latest));
        *report = Some(latest);
    }
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

fn program_exit(status: ExitStatus, stop_requested: bool) -> ProgramExit {
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    #[cfg(not(unix))]
    let signal = None;
    ProgramExit {
        code: status.code(),
        signal,
        stop_requested,
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
}
