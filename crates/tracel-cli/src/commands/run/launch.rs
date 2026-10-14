use std::io;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Context;
use serde::Serialize;
use tracel_job::{ExitCode, TRACEL_DESCRIBE};

use crate::error::{CliError, ErrorKind};

/// How often a running program and the signals are checked.
const TICK: Duration = Duration::from_millis(50);

/// Where a job run records its experiments.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Console,
    Offline,
}

impl Target {
    /// Name the target as `TRACEL_TARGET` does.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Console => "console",
            Self::Offline => "offline",
        }
    }
}

/// Choose where to record a run: the Tracel Console when a project is linked and a
/// credential is available, unless `offline` is asked for. Also says why the run is
/// offline when that wasn't asked for.
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
            Some("Recording offline: no project is linked. Link one with `tracel init`."),
        );
    }
    if !logged_in {
        return (
            Target::Offline,
            Some(
                "Recording offline: you aren't logged in. Log in with `tracel login`, or set TRACEL_API_KEY.",
            ),
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

/// Run `<program> <job> <input>` and wait for it.
///
/// The program runs in a process group of its own with stdin closed, so a Ctrl-C in the
/// terminal reaches only `tracel`. The first SIGINT, SIGTERM or SIGHUP `tracel` gets kills
/// the program and what it started at once. On Windows, the console also delivers Ctrl-C
/// to the program.
pub fn launch(launch: Launch) -> anyhow::Result<Exit> {
    let signalled = Arc::new(AtomicBool::new(false));
    {
        let signalled = Arc::clone(&signalled);
        ctrlc::set_handler(move || signalled.store(true, Ordering::SeqCst))
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
    let mut child = own_process_group(&mut command)
        .spawn()
        .with_context(|| format!("Can't run {}", launch.program.display()))?;

    if let Some(status) = wait_until(&mut child, || signalled.load(Ordering::SeqCst))? {
        return Ok(Exit {
            status,
            stopped: false,
        });
    }
    kill_job(&mut child).context("Can't stop the job")?;
    Ok(Exit {
        status: child.wait()?,
        stopped: true,
    })
}

/// Wait for `child` to exit, checking every tick until `give_up` says to stop waiting.
/// Returns its status, or `None` when waiting was given up.
pub fn wait_until(
    child: &mut Child,
    mut give_up: impl FnMut() -> bool,
) -> io::Result<Option<ExitStatus>> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if give_up() {
            return Ok(None);
        }
        std::thread::sleep(TICK);
    }
}

/// Make the program `command` runs the leader of a process group of its own, which
/// [`kill_job`] kills. On Windows, it is left as is.
fn own_process_group(command: &mut Command) -> &mut Command {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

/// Kill the program `child` runs and the processes it started: on Unix with SIGKILL to
/// the process group it leads, or to the program alone when that fails; on Windows the
/// program.
fn kill_job(child: &mut Child) -> io::Result<()> {
    #[cfg(unix)]
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: `killpg` has no memory effects; `pid` leads the group of a child not yet
        // waited for.
        if unsafe { libc::killpg(pid, libc::SIGKILL) } == 0 {
            return Ok(());
        }
    }
    child.kill()
}

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

    #[cfg(unix)]
    #[test]
    fn killing_a_job_kills_the_processes_it_started() {
        use std::io::{BufRead, BufReader, Read};
        use std::sync::mpsc;

        let mut command = Command::new("sh");
        command
            .args(["-c", "(echo ready; sleep 30; echo survived) & wait"])
            .stdout(Stdio::piped());
        let mut job = own_process_group(&mut command).spawn().unwrap();
        let mut stdout = BufReader::new(job.stdout.take().unwrap());
        let mut ready = String::new();
        stdout.read_line(&mut ready).unwrap();

        kill_job(&mut job).unwrap();
        let (sender, closed) = mpsc::channel();
        std::thread::spawn(move || {
            let mut rest = String::new();
            let _ = stdout.read_to_string(&mut rest);
            let _ = sender.send(rest);
        });

        assert_eq!(ready, "ready\n");
        assert_eq!(
            closed.recv_timeout(Duration::from_secs(10)).as_deref(),
            Ok(""),
            "a process the job started outlived it"
        );
        assert!(!job.wait().unwrap().success());
    }
}
