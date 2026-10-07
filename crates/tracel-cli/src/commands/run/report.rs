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

/// Where `experiment` is recorded, once it is created.
fn location(experiment: &ReportedExperiment) -> Option<String> {
    match (experiment.num, &experiment.url, &experiment.dir) {
        (_, _, Some(dir)) => Some(format!("offline run {}", dir.display())),
        (Some(num), Some(url), None) => Some(format!("experiment {num}, {url}")),
        (Some(num), None, None) => Some(format!("experiment {num}")),
        (None, _, None) => None,
    }
}

/// The line announcing where `experiment` is recorded, once it is created.
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

/// How a job run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ending {
    Completed,
    Failed {
        reason: String,
    },
    Cancelled,
    /// The program rejected the job or its input (exit code 2).
    Rejected,
}

/// How the run ended: from the report when it has a final status, otherwise from the exit
/// code (0 completed, 1 failed, 2 rejected, 130 cancelled).
pub fn ending(report: Option<&RunReport>, exit: ProgramExit) -> Ending {
    match report.map(|report| (report.status, &report.error)) {
        Some((RunStatus::Completed, _)) => return Ending::Completed,
        Some((RunStatus::Failed, error)) => {
            return Ending::Failed {
                reason: error
                    .clone()
                    .unwrap_or_else(|| "the job reported no error".to_string()),
            };
        }
        Some((RunStatus::Cancelled, _)) => return Ending::Cancelled,
        Some((RunStatus::Running, _)) | None => {}
    }
    match (exit.code, exit.signal) {
        (Some(0), _) => Ending::Completed,
        (Some(2), _) => Ending::Rejected,
        (Some(130), _) => Ending::Cancelled,
        (Some(code), _) => Ending::Failed {
            reason: format!("the program exited with code {code}"),
        },
        (None, _) if exit.stop_requested => Ending::Cancelled,
        (None, Some(signal)) => Ending::Failed {
            reason: format!("the program was ended by signal {signal}"),
        },
        (None, None) => Ending::Failed {
            reason: "the program ended without an exit code".to_string(),
        },
    }
}

/// The error a run that did not complete fails with: `JOB_FAILED` when the job failed or was
/// cancelled, `USAGE` when the program rejected it.
pub fn ending_error(job: &str, ending: &Ending, report: Option<&RunReport>) -> Option<CliError> {
    let location = report
        .and_then(|report| location(&report.experiment))
        .map(|location| format!(" ({location})"))
        .unwrap_or_default();
    let error = match ending {
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
        Ending::Failed { reason } => CliError::new(
            ErrorKind::JobFailed,
            format!("Job '{job}' ended with status failed{location}: {reason}"),
        ),
        Ending::Cancelled => CliError::new(
            ErrorKind::JobFailed,
            format!("Job '{job}' ended with status cancelled{location}."),
        ),
    };
    let console_experiment = report
        .filter(|report| report.experiment.dir.is_none())
        .and_then(|report| report.experiment.num);
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

    /// A report as the SDK writes it, for a Console experiment or an offline run.
    fn report(status: RunStatus, console: bool) -> RunReport {
        let experiment = if console {
            console_experiment()
        } else {
            offline_run()
        };
        RunReport {
            status,
            finished_at: (status != RunStatus::Running).then(|| "2026-10-06T12:05:00Z".into()),
            error: (status == RunStatus::Failed).then(|| "loss is NaN".into()),
            ..RunReport::new("mnist", experiment, STARTED_AT)
        }
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
        let not_created = ReportedExperiment {
            num: None,
            url: None,
            dir: None,
        };
        assert_eq!(announcement(&not_created), None);
        assert_eq!(location(&not_created), None);
    }

    #[test]
    fn only_reports_of_this_protocol_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        assert_eq!(read_report(&path), Ok(None));

        let running = report(RunStatus::Running, false);
        running.write(&path).unwrap();
        assert_eq!(read_report(&path), Ok(Some(running.clone())));

        RunReport {
            protocol: 2,
            ..running
        }
        .write(&path)
        .unwrap();
        assert_eq!(
            read_report(&path),
            Err("protocol 2 is not supported, expected 1".to_string())
        );
        for contents in [
            r#"{"protocol": 1, "job": "mnist", "experiment": {}, "status": "paused", "started_at": "now"}"#,
            r#"{"protocol": 1, "job": "mnist", "status": "running", "started_at": "now"}"#,
            r#"{"protocol": 1, "job": "mnist", "status": "running"#,
            "",
        ] {
            std::fs::write(&path, contents).unwrap();
            assert!(read_report(&path).is_err(), "{contents}");
        }
    }

    #[test]
    fn a_final_report_status_decides_the_ending() {
        for code in [0, 1, 2, 130] {
            assert_eq!(
                ending(Some(&report(RunStatus::Completed, true)), exited(code)),
                Ending::Completed
            );
            assert_eq!(
                ending(Some(&report(RunStatus::Failed, true)), exited(code)),
                Ending::Failed {
                    reason: "loss is NaN".to_string()
                }
            );
            assert_eq!(
                ending(Some(&report(RunStatus::Cancelled, true)), exited(code)),
                Ending::Cancelled
            );
        }
    }

    #[test]
    fn without_a_final_report_the_exit_code_decides() {
        for report in [None, Some(report(RunStatus::Running, true))] {
            let report = report.as_ref();
            assert_eq!(ending(report, exited(0)), Ending::Completed);
            assert_eq!(ending(report, exited(2)), Ending::Rejected);
            assert_eq!(ending(report, exited(130)), Ending::Cancelled);
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
            let stopped = ProgramExit {
                stop_requested: true,
                ..killed
            };
            assert_eq!(ending(report, stopped), Ending::Cancelled);
        }
    }

    #[test]
    fn failures_are_job_failed_and_point_at_the_experiment() {
        assert!(ending_error("mnist", &Ending::Completed, None).is_none());

        let failed = report(RunStatus::Failed, true);
        let error =
            ending_error("mnist", &ending(Some(&failed), exited(1)), Some(&failed)).unwrap();
        assert_eq!(error.kind, ErrorKind::JobFailed);
        assert_eq!(
            error.to_string(),
            "Job 'mnist' ended with status failed (experiment 42, https://console.tracel.ai/users/alice/projects/demo/experiments/42): loss is NaN"
        );
        let error = anyhow::Error::from(error);
        let error_report = crate::error::ErrorReport::new(&error);
        assert_eq!(
            error_report.hint,
            Some("Read its logs with `tracel experiments logs 42`.")
        );

        let cancelled = report(RunStatus::Cancelled, false);
        let error = ending_error("mnist", &Ending::Cancelled, Some(&cancelled)).unwrap();
        assert_eq!(error.kind, ErrorKind::JobFailed);
        assert_eq!(
            error.to_string(),
            "Job 'mnist' ended with status cancelled (offline run runs/mnist/3)."
        );
        assert_eq!(
            crate::error::ErrorReport::new(&anyhow::Error::from(error)).hint,
            None
        );

        let error = ending_error(
            "mnist",
            &Ending::Failed {
                reason: "the program exited with code 1".to_string(),
            },
            None,
        )
        .unwrap();
        assert_eq!(
            error.to_string(),
            "Job 'mnist' ended with status failed: the program exited with code 1"
        );

        let error = ending_error("mnist", &Ending::Rejected, None).unwrap();
        assert_eq!(error.kind, ErrorKind::Usage);
    }
}
