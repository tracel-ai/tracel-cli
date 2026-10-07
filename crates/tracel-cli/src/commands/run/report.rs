use std::path::Path;

use tracel_job::{PROTOCOL, ReportedExperiment, RunReport, RunStatus};

use crate::error::{CliError, ErrorKind};

/// The report at `path`, `None` when the program wrote none, or why the file is not a report
/// of the protocol this CLI reads.
pub fn read_report(path: &Path) -> Result<Option<RunReport>, String> {
    match RunReport::read(path) {
        Ok(report) if report.protocol == PROTOCOL => Ok(Some(report)),
        Ok(report) => Err(format!(
            "protocol {} is not supported, expected {PROTOCOL}",
            report.protocol
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// The exit code of a program that stopped because it was asked to.
const STOPPED: i32 = 130;

/// The experiment's name: its number, or its offline run directory.
fn name(experiment: &ReportedExperiment) -> Option<String> {
    match (experiment.num, &experiment.dir) {
        (_, Some(dir)) => Some(format!("offline run {}", dir.display())),
        (Some(num), None) => Some(format!("experiment {num}")),
        (None, None) => None,
    }
}

/// Where `experiment` is recorded.
fn location(experiment: &ReportedExperiment) -> Option<String> {
    let name = name(experiment)?;
    Some(match (&experiment.dir, &experiment.url) {
        (None, Some(url)) => format!("{name}, {url}"),
        _ => name,
    })
}

/// The line announcing where `experiment` is recorded.
pub fn announcement(experiment: &ReportedExperiment) -> Option<String> {
    match (experiment.num, &experiment.url, &experiment.dir) {
        (_, _, Some(dir)) => Some(format!("Recording offline in {}", dir.display())),
        (Some(num), Some(url), None) => Some(format!("Recording experiment {num}: {url}")),
        (Some(num), None, None) => Some(format!("Recording experiment {num}")),
        (None, _, None) => None,
    }
}

/// How the program ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgramExit {
    /// The exit code, or `None` when a signal ended the program.
    pub code: Option<i32>,
    /// The signal that ended the program.
    pub signal: Option<i32>,
    /// Whether the launcher asked the program to stop.
    pub stop_requested: bool,
}

impl ProgramExit {
    /// Whether the program was stopped: the launcher asked it to stop, or it exited with the
    /// code of a program that was asked to.
    fn stopped(self) -> bool {
        self.stop_requested || self.code == Some(STOPPED)
    }
}

/// How a job run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ending {
    Completed,
    Failed {
        reason: String,
    },
    /// The program was stopped, with how the job ended when the report says.
    Stopped {
        outcome: Option<JobOutcome>,
    },
    /// The program rejected the job or its input (exit code 2).
    Rejected,
}

/// How a job ended, by its run report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobOutcome {
    Completed,
    Failed { reason: String },
}

impl JobOutcome {
    /// The outcome `report` gives, once the job has ended.
    fn of(report: &RunReport) -> Option<Self> {
        match report.status {
            RunStatus::Running => None,
            RunStatus::Completed => Some(Self::Completed),
            RunStatus::Failed => Some(Self::Failed {
                reason: report
                    .error
                    .clone()
                    .unwrap_or_else(|| "the job reported no error".to_string()),
            }),
        }
    }
}

impl From<JobOutcome> for Ending {
    fn from(outcome: JobOutcome) -> Self {
        match outcome {
            JobOutcome::Completed => Self::Completed,
            JobOutcome::Failed { reason } => Self::Failed { reason },
        }
    }
}

/// How the run ended: stopped when the launcher asked the program to stop or it exited 130,
/// otherwise from the report when it has a final status, otherwise from the exit code (0
/// completed, 2 rejected, any other failed).
pub fn ending(report: Option<&RunReport>, exit: ProgramExit) -> Ending {
    let outcome = report.and_then(JobOutcome::of);
    if exit.stopped() {
        return Ending::Stopped { outcome };
    }
    if let Some(outcome) = outcome {
        return outcome.into();
    }
    match (exit.code, exit.signal) {
        (Some(0), _) => Ending::Completed,
        (Some(2), _) => Ending::Rejected,
        (Some(code), _) => Ending::Failed {
            reason: format!("the program exited with code {code}"),
        },
        (None, Some(signal)) => Ending::Failed {
            reason: format!("the program was ended by signal {signal}"),
        },
        (None, None) => Ending::Failed {
            reason: "the program ended without an exit code".to_string(),
        },
    }
}

/// The error a run that did not complete fails with: `JOB_FAILED` when the job failed or was
/// stopped, naming the experiment it recorded when the report gives one, and `USAGE` when the
/// program rejected it.
pub fn ending_error(job: &str, ending: &Ending, report: Option<&RunReport>) -> Option<CliError> {
    let experiment = report.and_then(|report| report.experiment.as_ref());
    let location = experiment
        .and_then(location)
        .map(|location| format!(" ({location})"))
        .unwrap_or_default();
    let message = match ending {
        Ending::Completed => return None,
        Ending::Rejected => {
            return Some(
                CliError::new(
                    ErrorKind::Usage,
                    format!("The program rejected job '{job}' or its input (exit code 2)."),
                )
                .with_hint("Its error is above; list the jobs with `tracel run --list`."),
            );
        }
        Ending::Failed { reason } => {
            format!("Job '{job}' ended with status failed{location}: {reason}")
        }
        Ending::Stopped { outcome: None } => format!("Job '{job}' was stopped{location}."),
        Ending::Stopped {
            outcome: Some(outcome),
        } => {
            let recorded = experiment
                .and_then(name)
                .map(|name| format!("; {name}"))
                .unwrap_or_default();
            match outcome {
                JobOutcome::Completed => {
                    format!("Job '{job}' was stopped (it completed{recorded}).")
                }
                JobOutcome::Failed { reason } => {
                    format!("Job '{job}' was stopped (it failed{recorded}): {reason}")
                }
            }
        }
    };
    let error = CliError::new(ErrorKind::JobFailed, message);
    let console_experiment = experiment
        .filter(|experiment| experiment.dir.is_none())
        .and_then(|experiment| experiment.num);
    Some(match console_experiment {
        Some(num) => error.with_hint(format!(
            "Read its logs with `tracel experiments logs {num}`."
        )),
        None => error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STARTED_AT: &str = "2026-10-06T12:00:00Z";

    fn console_experiment() -> ReportedExperiment {
        ReportedExperiment {
            num: Some(42),
            url: Some("https://console.tracel.ai/users/alice/projects/demo/experiments/42".into()),
            dir: None,
        }
    }

    fn offline_run() -> ReportedExperiment {
        ReportedExperiment {
            num: Some(3),
            url: None,
            dir: Some("runs/mnist/3".into()),
        }
    }

    /// A report as the SDK writes it, for a job that recorded `experiment`, if any.
    fn report(status: RunStatus, experiment: Option<ReportedExperiment>) -> RunReport {
        RunReport {
            status,
            finished_at: (status != RunStatus::Running).then(|| "2026-10-06T12:05:00Z".into()),
            error: (status == RunStatus::Failed).then(|| "loss is NaN".into()),
            experiment,
            ..RunReport::new("mnist", STARTED_AT)
        }
    }

    /// What a job may have recorded: a Console experiment, an offline run, or no experiment.
    fn experiments() -> [Option<ReportedExperiment>; 3] {
        [Some(console_experiment()), Some(offline_run()), None]
    }

    /// The reports of a job that recorded each of [`experiments`].
    fn reports(status: RunStatus) -> [RunReport; 3] {
        experiments().map(|experiment| report(status, experiment))
    }

    fn exited(code: i32) -> ProgramExit {
        ProgramExit {
            code: Some(code),
            signal: None,
            stop_requested: false,
        }
    }

    #[test]
    fn announcements_name_the_experiment_or_the_offline_run() {
        assert_eq!(
            announcement(&console_experiment()).unwrap(),
            "Recording experiment 42: https://console.tracel.ai/users/alice/projects/demo/experiments/42"
        );
        assert_eq!(
            announcement(&offline_run()).unwrap(),
            "Recording offline in runs/mnist/3"
        );
        assert_eq!(
            location(&offline_run()).unwrap(),
            "offline run runs/mnist/3"
        );
        let without_page = ReportedExperiment {
            url: None,
            ..console_experiment()
        };
        assert_eq!(
            announcement(&without_page).unwrap(),
            "Recording experiment 42"
        );
        let unnamed = ReportedExperiment {
            num: None,
            url: None,
            dir: None,
        };
        assert_eq!(announcement(&unnamed), None);
        assert_eq!(location(&unnamed), None);
    }

    #[test]
    fn only_reports_of_this_protocol_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        assert_eq!(read_report(&path), Ok(None));

        for running in reports(RunStatus::Running) {
            running.write(&path).unwrap();
            assert_eq!(read_report(&path), Ok(Some(running)));
        }

        RunReport {
            protocol: 2,
            ..report(RunStatus::Running, None)
        }
        .write(&path)
        .unwrap();
        assert_eq!(
            read_report(&path),
            Err("protocol 2 is not supported, expected 1".to_string())
        );
        for contents in [
            r#"{"protocol": 1, "job": "mnist", "status": "paused", "started_at": "now"}"#,
            r#"{"protocol": 1, "job": "mnist", "status": "running"}"#,
            r#"{"protocol": 1, "job": "mnist", "status": "running"#,
            "",
        ] {
            std::fs::write(&path, contents).unwrap();
            assert!(read_report(&path).is_err(), "{contents}");
        }
    }

    #[test]
    fn a_report_that_leaves_out_the_experiment_has_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        std::fs::write(
            &path,
            r#"{"protocol": 1, "job": "wordtok", "status": "completed", "started_at": "now"}"#,
        )
        .unwrap();

        let report = read_report(&path).unwrap().unwrap();

        assert_eq!(report.experiment, None);
        assert_eq!(report.status, RunStatus::Completed);
    }

    fn stopped(code: Option<i32>, signal: Option<i32>) -> ProgramExit {
        ProgramExit {
            code,
            signal,
            stop_requested: true,
        }
    }

    fn failed(reason: &str) -> JobOutcome {
        JobOutcome::Failed {
            reason: reason.to_string(),
        }
    }

    #[test]
    fn a_final_report_status_decides_the_ending_of_a_job_not_stopped() {
        for code in [0, 1, 2, 101] {
            for completed in reports(RunStatus::Completed) {
                assert_eq!(ending(Some(&completed), exited(code)), Ending::Completed);
            }
            for failed in reports(RunStatus::Failed) {
                assert_eq!(
                    ending(Some(&failed), exited(code)),
                    Ending::Failed {
                        reason: "loss is NaN".to_string()
                    }
                );
            }
        }
    }

    #[test]
    fn without_a_final_report_the_exit_code_decides() {
        let running = reports(RunStatus::Running).map(Some);
        for report in running.iter().chain([&None]) {
            let report = report.as_ref();
            assert_eq!(ending(report, exited(0)), Ending::Completed);
            assert_eq!(ending(report, exited(2)), Ending::Rejected);
            for code in [1, 101] {
                assert_eq!(
                    ending(report, exited(code)),
                    Ending::Failed {
                        reason: format!("the program exited with code {code}")
                    }
                );
            }
            let killed = ProgramExit {
                code: None,
                signal: Some(9),
                stop_requested: false,
            };
            assert_eq!(
                ending(report, killed),
                Ending::Failed {
                    reason: "the program was ended by signal 9".to_string()
                }
            );
        }
    }

    #[test]
    fn a_job_asked_to_stop_is_stopped_with_how_it_ended() {
        for exit in [
            stopped(Some(130), None),
            stopped(Some(0), None),
            stopped(Some(1), None),
            stopped(None, Some(9)),
        ] {
            for completed in reports(RunStatus::Completed) {
                assert_eq!(
                    ending(Some(&completed), exit),
                    Ending::Stopped {
                        outcome: Some(JobOutcome::Completed)
                    }
                );
            }
            for failed_report in reports(RunStatus::Failed) {
                assert_eq!(
                    ending(Some(&failed_report), exit),
                    Ending::Stopped {
                        outcome: Some(failed("loss is NaN"))
                    }
                );
            }
        }
    }

    #[test]
    fn a_job_asked_to_stop_without_a_final_report_is_stopped() {
        let running = reports(RunStatus::Running).map(Some);
        for report in running.iter().chain([&None]) {
            for exit in [stopped(Some(130), None), stopped(None, Some(9))] {
                assert_eq!(
                    ending(report.as_ref(), exit),
                    Ending::Stopped { outcome: None }
                );
            }
        }
    }

    #[test]
    fn a_program_exiting_130_unasked_is_stopped() {
        assert_eq!(ending(None, exited(130)), Ending::Stopped { outcome: None });
        for experiment in experiments() {
            let ended = |status| ending(Some(&report(status, experiment.clone())), exited(130));
            assert_eq!(ended(RunStatus::Running), Ending::Stopped { outcome: None });
            assert_eq!(
                ended(RunStatus::Completed),
                Ending::Stopped {
                    outcome: Some(JobOutcome::Completed)
                }
            );
            assert_eq!(
                ended(RunStatus::Failed),
                Ending::Stopped {
                    outcome: Some(failed("loss is NaN"))
                }
            );
        }
    }

    /// The error `ending` fails with, its exit code, and its hint.
    fn failure(ending: &Ending, report: Option<&RunReport>) -> (String, i32, Option<String>) {
        let error = anyhow::Error::from(ending_error("mnist", ending, report).unwrap());
        let error_report = crate::error::ErrorReport::new(&error);
        (
            error.to_string(),
            error_report.exit_code,
            error_report.hint.map(str::to_string),
        )
    }

    /// The failure of a job that ended with `report`, and the program's `exit`.
    fn failure_of(report: &RunReport, exit: ProgramExit) -> (String, i32, Option<String>) {
        failure(&ending(Some(report), exit), Some(report))
    }

    fn logs_hint() -> Option<String> {
        Some("Read its logs with `tracel experiments logs 42`.".to_string())
    }

    #[test]
    fn failures_are_job_failed_and_point_at_the_experiment_when_there_is_one() {
        assert!(ending_error("mnist", &Ending::Completed, None).is_none());

        let [console, offline, without] = reports(RunStatus::Failed);
        assert_eq!(
            failure_of(&console, exited(1)),
            (
                "Job 'mnist' ended with status failed (experiment 42, https://console.tracel.ai/users/alice/projects/demo/experiments/42): loss is NaN".to_string(),
                9,
                logs_hint()
            )
        );
        assert_eq!(
            failure_of(&offline, exited(1)),
            (
                "Job 'mnist' ended with status failed (offline run runs/mnist/3): loss is NaN"
                    .to_string(),
                9,
                None
            )
        );
        assert_eq!(
            failure_of(&without, exited(1)),
            (
                "Job 'mnist' ended with status failed: loss is NaN".to_string(),
                9,
                None
            )
        );

        let error = ending_error(
            "mnist",
            &Ending::Failed {
                reason: "the program exited with code 1".to_string(),
            },
            None,
        )
        .unwrap();
        assert_eq!(error.kind, ErrorKind::JobFailed);
        assert_eq!(
            error.to_string(),
            "Job 'mnist' ended with status failed: the program exited with code 1"
        );

        let error = ending_error("mnist", &Ending::Rejected, None).unwrap();
        assert_eq!(error.kind, ErrorKind::Usage);
    }

    #[test]
    fn a_stopped_job_is_job_failed_and_says_how_it_ended() {
        let asked = stopped(Some(130), None);
        let [console, offline, without] = reports(RunStatus::Completed);
        assert_eq!(
            failure_of(&console, asked),
            (
                "Job 'mnist' was stopped (it completed; experiment 42).".to_string(),
                9,
                logs_hint()
            )
        );
        assert_eq!(
            failure_of(&offline, asked),
            (
                "Job 'mnist' was stopped (it completed; offline run runs/mnist/3).".to_string(),
                9,
                None
            )
        );
        assert_eq!(
            failure_of(&without, asked),
            (
                "Job 'mnist' was stopped (it completed).".to_string(),
                9,
                None
            )
        );

        let [console, offline, without] = reports(RunStatus::Failed);
        assert_eq!(
            failure_of(&console, asked),
            (
                "Job 'mnist' was stopped (it failed; experiment 42): loss is NaN".to_string(),
                9,
                logs_hint()
            )
        );
        assert_eq!(
            failure_of(&offline, asked),
            (
                "Job 'mnist' was stopped (it failed; offline run runs/mnist/3): loss is NaN"
                    .to_string(),
                9,
                None
            )
        );
        assert_eq!(
            failure_of(&without, asked),
            (
                "Job 'mnist' was stopped (it failed): loss is NaN".to_string(),
                9,
                None
            )
        );
    }

    #[test]
    fn a_job_stopped_before_it_ended_says_where_its_experiment_is_recorded() {
        let [console, offline, without] = reports(RunStatus::Running);
        assert_eq!(
            failure_of(&console, exited(130)),
            (
                "Job 'mnist' was stopped (experiment 42, https://console.tracel.ai/users/alice/projects/demo/experiments/42).".to_string(),
                9,
                logs_hint()
            )
        );
        assert_eq!(
            failure_of(&offline, exited(130)),
            (
                "Job 'mnist' was stopped (offline run runs/mnist/3).".to_string(),
                9,
                None
            )
        );
        assert_eq!(
            failure_of(&without, exited(130)),
            ("Job 'mnist' was stopped.".to_string(), 9, None)
        );
        assert_eq!(
            failure(&ending(None, stopped(None, Some(9))), None),
            ("Job 'mnist' was stopped.".to_string(), 9, None)
        );
    }

    #[test]
    fn an_unnamed_experiment_is_left_out() {
        let unnamed = ReportedExperiment {
            num: None,
            url: None,
            dir: None,
        };
        let completed = report(RunStatus::Completed, Some(unnamed));

        assert_eq!(
            failure_of(&completed, exited(130)),
            (
                "Job 'mnist' was stopped (it completed).".to_string(),
                9,
                None
            )
        );
    }
}
