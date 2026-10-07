use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{CliError, ErrorKind};

/// The runner protocol version this CLI reads.
const PROTOCOL: u32 = 1;

/// The report a program writes to the path in `TRACEL_REPORT_FILE` when its experiment is
/// created, and again when the experiment ends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunReport {
    pub protocol: u32,
    pub job: String,
    pub experiment: ReportedExperiment,
    pub status: RunStatus,
    pub started_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Where a run records its experiment: its number and Console page, or the directory of an
/// offline run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedExperiment {
    #[serde(default)]
    pub num: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl RunStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// The report in `contents`, or why it is not a report of this protocol version.
pub fn parse_report(contents: &str) -> Result<RunReport, String> {
    let report = serde_json::from_str::<RunReport>(contents).map_err(|error| error.to_string())?;
    if report.protocol != PROTOCOL {
        return Err(format!(
            "protocol {} is not supported, expected {PROTOCOL}",
            report.protocol
        ));
    }
    Ok(report)
}

/// The report at `path`, `None` when the program wrote none, or why the file is not a report.
pub fn read_report(path: &Path) -> Result<Option<RunReport>, String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => parse_report(&contents).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

impl RunReport {
    /// Where the run is recorded, once the experiment is created.
    pub fn location(&self) -> Option<String> {
        let experiment = &self.experiment;
        match (experiment.num, &experiment.url, &experiment.dir) {
            (_, _, Some(dir)) => Some(format!("offline run {dir}")),
            (Some(num), Some(url), None) => Some(format!("experiment {num}, {url}")),
            (Some(num), None, None) => Some(format!("experiment {num}")),
            (None, _, None) => None,
        }
    }

    /// The line announcing where the run is recorded.
    pub fn announcement(&self) -> Option<String> {
        let experiment = &self.experiment;
        match (experiment.num, &experiment.url, &experiment.dir) {
            (_, _, Some(dir)) => Some(format!("Recording offline in {dir}")),
            (Some(num), Some(url), None) => Some(format!("Recording experiment {num}: {url}")),
            (Some(num), None, None) => Some(format!("Recording experiment {num}")),
            (None, _, None) => None,
        }
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
        .and_then(RunReport::location)
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
            format!(
                "Job '{job}' ended with status {}{location}: {reason}",
                RunStatus::Failed.as_str()
            ),
        ),
        Ending::Cancelled => CliError::new(
            ErrorKind::JobFailed,
            format!(
                "Job '{job}' ended with status {}{location}.",
                RunStatus::Cancelled.as_str()
            ),
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

    /// A report as the SDK writes it, for a Console experiment or an offline run.
    fn report(status: &str, console: bool) -> RunReport {
        let experiment = if console {
            r#"{"num": 42, "url": "https://console.tracel.ai/users/alice/projects/demo/experiments/42"}"#
        } else {
            r#"{"num": 3, "url": null, "dir": "runs/mnist/3"}"#
        };
        let finished_at = if status == "running" {
            "null"
        } else {
            r#""2026-10-06T12:05:00Z""#
        };
        let error = if status == "failed" {
            r#""loss is NaN""#
        } else {
            "null"
        };
        parse_report(&format!(
            r#"{{"protocol": 1, "job": "mnist", "experiment": {experiment}, "status": "{status}",
                "started_at": "2026-10-06T12:00:00Z", "finished_at": {finished_at}, "error": {error}}}"#
        ))
        .unwrap()
    }

    fn exited(code: i32) -> ProgramExit {
        ProgramExit {
            code: Some(code),
            signal: None,
            stop_requested: false,
        }
    }

    #[test]
    fn reports_parse_with_optional_fields() {
        let failed = report("failed", true);
        assert_eq!(failed.job, "mnist");
        assert_eq!(failed.status, RunStatus::Failed);
        assert_eq!(failed.experiment.num, Some(42));
        assert_eq!(failed.error.as_deref(), Some("loss is NaN"));
        assert_eq!(
            failed.announcement().unwrap(),
            "Recording experiment 42: https://console.tracel.ai/users/alice/projects/demo/experiments/42"
        );

        let offline = report("running", false);
        assert_eq!(offline.experiment.dir.as_deref(), Some("runs/mnist/3"));
        assert_eq!(offline.finished_at, None);
        assert_eq!(
            offline.announcement().unwrap(),
            "Recording offline in runs/mnist/3"
        );
        assert_eq!(offline.location().unwrap(), "offline run runs/mnist/3");
        assert_eq!(
            serde_json::to_value(&offline).unwrap()["experiment"]["dir"],
            "runs/mnist/3"
        );

        let without_page = parse_report(
            r#"{"protocol": 1, "job": "mnist", "experiment": {"num": 7, "url": null},
                "status": "running", "started_at": "2026-10-06T12:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(
            without_page.announcement().unwrap(),
            "Recording experiment 7"
        );
        assert!(
            serde_json::to_value(&without_page).unwrap()["experiment"]
                .get("dir")
                .is_none()
        );
    }

    #[test]
    fn other_protocols_and_partial_files_are_not_reports() {
        for contents in [
            r#"{"protocol": 2, "job": "mnist", "experiment": {}, "status": "running", "started_at": "now"}"#,
            r#"{"protocol": 1, "job": "mnist", "experiment": {}, "status": "paused", "started_at": "now"}"#,
            r#"{"protocol": 1, "job": "mnist", "status": "running", "started_at": "now"}"#,
            r#"{"protocol": 1, "job": "mnist", "status": "running"#,
            "",
        ] {
            assert!(parse_report(contents).is_err(), "{contents}");
        }
        assert_eq!(read_report(Path::new("/nonexistent/report.json")), Ok(None));
    }

    #[test]
    fn a_final_report_status_decides_the_ending() {
        for code in [0, 1, 2, 130] {
            assert_eq!(
                ending(Some(&report("completed", true)), exited(code)),
                Ending::Completed
            );
            assert_eq!(
                ending(Some(&report("failed", true)), exited(code)),
                Ending::Failed {
                    reason: "loss is NaN".to_string()
                }
            );
            assert_eq!(
                ending(Some(&report("cancelled", true)), exited(code)),
                Ending::Cancelled
            );
        }
    }

    #[test]
    fn without_a_final_report_the_exit_code_decides() {
        for report in [None, Some(report("running", true))] {
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

        let failed = report("failed", true);
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

        let cancelled = report("cancelled", false);
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
