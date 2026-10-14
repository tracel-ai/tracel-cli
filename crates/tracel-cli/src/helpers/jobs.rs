//! Jobs on compute providers: the command a queued job runs the program with, and
//! reading a job until it finishes.

use std::borrow::Cow;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tracel_client::ClientError;
use tracel_client::console::Client;
use tracel_client::console::job::response::{JobLogResponse, JobResponse};

use super::{Resource, map_resource_error};
use crate::error::{CliError, ErrorKind};
use crate::tools::tracel_config::TracelProject;
use crate::ui::{Human, Output, Render};

/// How often a followed job is read again once its logs are caught up.
const FOLLOW_INTERVAL: Duration = Duration::from_secs(2);

/// Write the job command that gives the program `arguments` unchanged.
///
/// The compute provider splits the command into words with shell quoting rules (quotes,
/// backslashes, and `#` comments, without expansions), so every argument the split would
/// change is single-quoted.
pub fn job_command(arguments: &[String]) -> String {
    arguments
        .iter()
        .map(|argument| quote(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote(argument: &str) -> Cow<'_, str> {
    let unchanged = !argument.is_empty()
        && !argument.starts_with('#')
        && !argument.contains([' ', '\t', '\n', '\'', '"', '\\']);
    if unchanged {
        return Cow::Borrowed(argument);
    }
    Cow::Owned(format!("'{}'", argument.replace('\'', r"'\''")))
}

/// Read the job and input of a command `tracel run <job> --remote` queued, as
/// [`job_command`] writes it; `None` for any other command.
pub fn job_invocation(command: &str) -> Option<(String, Value)> {
    let [job, input] = <[String; 2]>::try_from(job_arguments(command)?).ok()?;
    let input: Value = serde_json::from_str(&input).ok()?;
    let named = !job.is_empty() && !job.starts_with('-');
    (named && input.is_object()).then_some((job, input))
}

/// Split a command [`job_command`] wrote into its arguments; `None` when it quotes in a
/// way [`job_command`] doesn't.
fn job_arguments(command: &str) -> Option<Vec<String>> {
    let mut arguments = Vec::new();
    let mut argument = None::<String>;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' => arguments.extend(argument.take()),
            '\'' => {
                let argument = argument.get_or_insert_default();
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => argument.push(c),
                    }
                }
            }
            '\\' => argument.get_or_insert_default().push(chars.next()?),
            '"' | '\t' | '\n' => return None,
            c => argument.get_or_insert_default().push(c),
        }
    }
    arguments.extend(argument);
    Some(arguments)
}

/// How a job ended, from its status, or `None` while the status can still change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalStatus {
    Completed,
    Failed,
    Cancelled,
}

pub fn final_status(status: &str) -> Option<FinalStatus> {
    match status {
        "completed" => Some(FinalStatus::Completed),
        "failed" => Some(FinalStatus::Failed),
        "cancelled" => Some(FinalStatus::Cancelled),
        _ => None,
    }
}

/// Fail with `JOB_FAILED` unless `job`, whose status is final, completed.
pub fn require_completed(job: &JobResponse) -> Result<(), CliError> {
    let num = job.job_num;
    let message = match final_status(&job.status) {
        Some(FinalStatus::Completed) | None => return Ok(()),
        Some(FinalStatus::Failed) => format!("Job {num} failed."),
        Some(FinalStatus::Cancelled) => format!("Job {num} was cancelled."),
    };
    Err(CliError::new(ErrorKind::JobFailed, message))
}

/// Turn the Tracel Console's "not found" for job `num` into an error that names it.
pub fn missing_job(error: ClientError, project: &TracelProject, num: i32) -> anyhow::Error {
    map_resource_error(error, &project.namespace, &project.name, Resource::Job(num))
}

pub fn read_job(client: &Client, project: &TracelProject, num: i32) -> anyhow::Result<JobResponse> {
    client
        .get_job(&project.namespace, &project.name, num)
        .map_err(|error| missing_job(error, project, num))
}

/// Read job `num` every `interval` until its status is final, or `deadline` passes, and
/// return it as last read. `each` gets every read.
pub fn poll_job(
    client: &Client,
    project: &TracelProject,
    num: i32,
    interval: Duration,
    deadline: Option<Instant>,
    mut each: impl FnMut(&JobResponse) -> anyhow::Result<()>,
) -> anyhow::Result<JobResponse> {
    loop {
        let job = read_job(client, project, num)?;
        each(&job)?;
        if final_status(&job.status).is_some() {
            return Ok(job);
        }
        let pause = match deadline {
            Some(deadline) => match deadline.saturating_duration_since(Instant::now()) {
                left if left.is_zero() => return Ok(job),
                left => interval.min(left),
            },
            None => interval,
        };
        std::thread::sleep(pause);
    }
}

/// An event of following a job's logs. As JSON, a page of logs gains `"type": "log"`.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LogEvent<'a> {
    Log(&'a JobLogResponse),
    End {
        status: &'a str,
        /// Whether the logs so far end inside a line, which the text then ends.
        #[serde(skip)]
        mid_line: bool,
    },
}

impl Render for LogEvent<'_> {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        match self {
            Self::Log(page) => out.write_all(page.logs.as_bytes()),
            Self::End { mid_line: true, .. } => writeln!(out),
            Self::End { .. } => Ok(()),
        }
    }
}

/// Find where the next read of the logs starts after `page`, read from `start`, and
/// whether more is already there to read.
fn next_read(start: u64, page: &JobLogResponse) -> anyhow::Result<(u64, bool)> {
    let next = page.end.max(start);
    if page.has_more && next == start {
        anyhow::bail!("The Tracel Console has more logs to send, but not from where.");
    }
    Ok((next, page.has_more))
}

/// Write job `num`'s logs from byte `start` until the job has finished, and return it as
/// last read. The job is read before its logs, so the logs read after a final status are
/// complete.
pub fn follow_logs(
    client: &Client,
    output: &Output,
    project: &TracelProject,
    num: i32,
    start: u64,
) -> anyhow::Result<JobResponse> {
    let mut start = start;
    let mut mid_line = false;
    let job = poll_job(client, project, num, FOLLOW_INTERVAL, None, |_| {
        loop {
            let page = client
                .get_job_logs(&project.namespace, &project.name, num, Some(start))
                .map_err(|error| missing_job(error, project, num))?;
            if !page.logs.is_empty() {
                output.event(&LogEvent::Log(&page))?;
                mid_line = !page.logs.ends_with('\n');
            }
            let more;
            (start, more) = next_read(start, &page)?;
            if !more {
                return Ok(());
            }
        }
    })?;
    output.event(&LogEvent::End {
        status: &job.status,
        mid_line,
    })?;
    Ok(job)
}

/// Follow job `num`'s logs from the start until it finishes, and fail unless it
/// completed.
pub fn follow_job(
    client: &Client,
    output: &Output,
    project: &TracelProject,
    num: i32,
) -> anyhow::Result<JobResponse> {
    let job = follow_logs(client, output, project, num, 0)?;
    require_completed(&job)?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::plain;

    fn command(arguments: &[&str]) -> String {
        job_command(
            &arguments
                .iter()
                .map(|argument| argument.to_string())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn arguments_the_split_would_change_are_single_quoted() {
        for (arguments, expected) in [
            (&[][..], ""),
            (
                &["train", "--epochs", "10", "--lr=0.01"],
                "train --epochs 10 --lr=0.01",
            ),
            (
                &["$HOME", "*.rs", "~/data", "a#b", "x|y;z", "--name=é"],
                "$HOME *.rs ~/data a#b x|y;z --name=é",
            ),
            (&[""], "''"),
            (&["my run"], "'my run'"),
            (&["tab\there"], "'tab\there'"),
            (&["two\nlines"], "'two\nlines'"),
            (&["#tag"], "'#tag'"),
            (&[r#"say "hi""#], r#"'say "hi"'"#),
            (&[r"C:\data"], r"'C:\data'"),
            (&["it's"], r"'it'\''s'"),
            (&["'"], r"''\'''"),
        ] {
            assert_eq!(command(arguments), expected, "{arguments:?}");
        }
    }

    #[test]
    fn a_job_command_reads_back_as_its_arguments() {
        for arguments in [
            &[][..],
            &["train", "--epochs", "10"],
            &[
                "--name", "my run", "", "--tag", "it's", "'", "#tag", r"C:\data",
            ],
            &["train", r#"{"label":"it's \"ok\"","lr":0.1}"#],
        ] {
            assert_eq!(
                job_arguments(&command(arguments)).unwrap(),
                arguments,
                "{arguments:?}"
            );
        }
        assert_eq!(job_arguments(r#"say "hi""#), None);
        assert_eq!(job_arguments("'unterminated"), None);
    }

    #[test]
    fn only_a_job_and_an_input_object_are_an_invocation() {
        let input = json!({"epochs": 2, "optimizer": {"lr": 0.01}});
        let queued = command(&["train", &input.to_string()]);
        assert_eq!(job_invocation(&queued), Some(("train".to_string(), input)));
        for other in [
            "train --epochs 10",
            "train",
            "train '[1]'",
            "--epochs '{}'",
            "train '{}' extra",
            "'' '{}'",
            "",
        ] {
            assert_eq!(job_invocation(other), None, "{other:?}");
        }
    }

    fn page(end: u64, has_more: bool) -> JobLogResponse {
        JobLogResponse {
            logs: "epoch 1\n".into(),
            start: 0,
            end,
            total_size: 4096,
            has_more,
        }
    }

    #[test]
    fn the_next_read_starts_where_a_page_ends() {
        for (start, page, expected) in [
            (0, page(8, false), Some((8, false))),
            (0, page(8, true), Some((8, true))),
            (16, page(8, false), Some((16, false))),
            (8, page(8, false), Some((8, false))),
            (8, page(8, true), None),
        ] {
            assert_eq!(next_read(start, &page).ok(), expected, "{start}");
        }
    }

    #[test]
    fn followed_logs_are_typed_events_that_end_their_line() {
        let page = JobLogResponse {
            logs: "epoch 1\nepo".into(),
            ..page(11, true)
        };
        assert_eq!(
            serde_json::to_value(LogEvent::Log(&page)).unwrap(),
            json!({
                "type": "log",
                "logs": "epoch 1\nepo",
                "start": 0,
                "end": 11,
                "total_size": 4096,
                "has_more": true,
            })
        );
        assert_eq!(plain(&LogEvent::Log(&page)), "epoch 1\nepo");
        for mid_line in [false, true] {
            let end = LogEvent::End {
                status: "completed",
                mid_line,
            };
            assert_eq!(
                serde_json::to_value(&end).unwrap(),
                json!({"type": "end", "status": "completed"})
            );
            assert_eq!(plain(&end), if mid_line { "\n" } else { "" });
        }
    }
}
