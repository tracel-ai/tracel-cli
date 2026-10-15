//! Jobs on compute providers: the command a queued job runs the program with, and
//! reading a job until it finishes.

use std::borrow::Cow;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tracel_client::ClientError;
use tracel_client::console::Client;
use tracel_client::console::job::request::JobLogCursor;
use tracel_client::console::job::response::{JobLogLine, JobLogResponse, JobResponse};

use super::{Resource, map_resource_error};
use crate::error::{CliError, ErrorKind};
use crate::tools::tracel_config::TracelProject;
use crate::ui::{Human, Output, Render};

/// How often a followed job is read again once its logs are caught up.
const FOLLOW_INTERVAL: Duration = Duration::from_secs(2);

/// How many of its last lines following a running job starts with, unless told, as
/// `tail -f` does.
const LINES_ON_ATTACH: u32 = 10;

/// How many lines one read of a job's log asks for when paging through it.
const LOG_PAGE_LINES: u32 = 1_000;

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

/// An event of following a job's logs. As JSON, a line gains `"type": "log"`.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LogEvent<'a> {
    Log(&'a JobLogLine),
    End { status: &'a str },
}

impl Render for LogEvent<'_> {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        match self {
            Self::Log(line) => writeln!(out, "{}", line.message),
            Self::End { .. } => Ok(()),
        }
    }
}

/// Where reading a job's log starts: with this many of its last lines, or with its first.
#[derive(Clone, Copy)]
enum LogStart {
    Last(u32),
    First,
}

/// Where a reader of a job's log reads next: at its start until a line is read, then after
/// the newest line.
struct LogFollower {
    newest: Option<u64>,
    start: LogStart,
}

impl LogFollower {
    fn new(start: LogStart) -> Self {
        Self {
            newest: None,
            start,
        }
    }

    /// A job numbers its lines from 0, so its first page is the lines before
    /// [`LOG_PAGE_LINES`].
    fn read_at(&self) -> (JobLogCursor, Option<u32>) {
        match (self.newest, self.start) {
            (Some(seq), _) => (JobLogCursor::After(seq), Some(LOG_PAGE_LINES)),
            (None, LogStart::Last(lines)) => (JobLogCursor::Last, Some(lines)),
            (None, LogStart::First) => (
                JobLogCursor::Before(u64::from(LOG_PAGE_LINES)),
                Some(LOG_PAGE_LINES),
            ),
        }
    }

    /// Take `page`, read at [`read_at`](Self::read_at), and return whether newer lines may
    /// already be waiting. A log still empty is read again from its first line.
    fn take(&mut self, page: &JobLogResponse) -> bool {
        let more = match (self.newest, self.start) {
            (Some(_), _) => page.has_more,
            (None, LogStart::First) => !page.lines.is_empty(),
            (None, LogStart::Last(_)) => false,
        };
        self.start = LogStart::First;
        if let Some(line) = page.lines.last() {
            self.newest = Some(line.seq);
        }
        more
    }
}

/// A job's log being shown, read a page at a time.
struct JobLog<'a> {
    client: &'a Client,
    output: &'a Output,
    project: &'a TracelProject,
    num: i32,
    follower: LogFollower,
}

impl<'a> JobLog<'a> {
    fn new(
        client: &'a Client,
        output: &'a Output,
        project: &'a TracelProject,
        num: i32,
        start: LogStart,
    ) -> Self {
        Self {
            client,
            output,
            project,
            num,
            follower: LogFollower::new(start),
        }
    }

    fn read_new_lines(&mut self) -> anyhow::Result<()> {
        loop {
            let (cursor, limit) = self.follower.read_at();
            let page = self
                .client
                .get_job_logs(
                    &self.project.namespace,
                    &self.project.name,
                    self.num,
                    cursor,
                    limit,
                )
                .map_err(|error| missing_job(error, self.project, self.num))?;
            let more = self.follower.take(&page);
            self.show(&page.lines)?;
            if !more {
                return Ok(());
            }
        }
    }

    fn show(&self, lines: &[JobLogLine]) -> anyhow::Result<()> {
        for line in lines {
            self.output.event(&LogEvent::Log(line))?;
        }
        Ok(())
    }
}

/// Write job `num`'s log up to its newest line: its last `tail` lines, or every line.
pub fn show_logs(
    client: &Client,
    output: &Output,
    project: &TracelProject,
    num: i32,
    tail: Option<u32>,
) -> anyhow::Result<()> {
    let start = tail.map_or(LogStart::First, LogStart::Last);
    JobLog::new(client, output, project, num, start).read_new_lines()
}

/// Write job `num`'s log lines from `start` until the job has finished, and return it as
/// last read. The job is read before its logs, so the lines read after a final status are
/// complete.
fn follow(
    client: &Client,
    output: &Output,
    project: &TracelProject,
    num: i32,
    start: LogStart,
) -> anyhow::Result<JobResponse> {
    let mut log = JobLog::new(client, output, project, num, start);
    let job = poll_job(client, project, num, FOLLOW_INTERVAL, None, |_| {
        log.read_new_lines()
    })?;
    output.event(&LogEvent::End {
        status: &job.status,
    })?;
    Ok(job)
}

/// Write job `num`'s last `tail` lines, 10 unless told, then its new ones until it has
/// finished, and return it as last read.
pub fn follow_logs(
    client: &Client,
    output: &Output,
    project: &TracelProject,
    num: i32,
    tail: Option<u32>,
) -> anyhow::Result<JobResponse> {
    let start = LogStart::Last(tail.unwrap_or(LINES_ON_ATTACH));
    follow(client, output, project, num, start)
}

/// Follow job `num`, queued just before, with every line it writes until it finishes, and
/// fail unless it completed.
pub fn follow_job(
    client: &Client,
    output: &Output,
    project: &TracelProject,
    num: i32,
) -> anyhow::Result<JobResponse> {
    let job = follow(client, output, project, num, LogStart::First)?;
    require_completed(&job)?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde_json::json;

    use tracel_client::console::job::response::JobLogStream;

    use super::*;

    fn command(arguments: &[&str]) -> String {
        let arguments: Vec<String> = arguments
            .iter()
            .map(|argument| argument.to_string())
            .collect();
        job_command(&arguments)
    }

    fn line(seq: u64) -> JobLogLine {
        JobLogLine {
            seq,
            stream: JobLogStream::Stdout,
            emitted_at: "2026-10-15T12:00:00.000Z".into(),
            message: format!("epoch {seq}"),
        }
    }

    fn page(seqs: std::ops::Range<u64>, has_more: bool) -> JobLogResponse {
        JobLogResponse {
            lines: seqs.map(line).collect(),
            has_more,
        }
    }

    #[test]
    fn given_plain_arguments_when_writing_a_job_command_then_they_are_joined_unchanged() {
        let written = command(&["train", "--epochs", "10", "--lr=0.01", "$HOME", "*.rs"]);

        assert_eq!(written, "train --epochs 10 --lr=0.01 $HOME *.rs");
    }

    #[rstest]
    #[case::empty("", "''")]
    #[case::space("my run", "'my run'")]
    #[case::leading_hash("#tag", "'#tag'")]
    #[case::double_quote(r#"say "hi""#, r#"'say "hi"'"#)]
    #[case::backslash(r"C:\data", r"'C:\data'")]
    #[case::single_quote("it's", r"'it'\''s'")]
    fn given_an_argument_the_split_would_change_when_writing_a_job_command_then_it_is_single_quoted(
        #[case] argument: &str,
        #[case] expected: &str,
    ) {
        let written = command(&[argument]);

        assert_eq!(written, expected);
    }

    #[test]
    fn given_a_job_command_when_reading_it_back_then_it_gives_the_same_arguments() {
        let arguments = [
            "--name",
            "my run",
            "",
            "it's",
            "#tag",
            r#"{"label":"a \"b\""}"#,
        ];
        let written = command(&arguments);

        let read = job_arguments(&written).unwrap();

        assert_eq!(read, arguments);
    }

    #[test]
    fn given_a_job_and_its_input_queued_by_tracel_run_when_reading_the_command_then_both_are_found()
    {
        let input = json!({"epochs": 2, "optimizer": {"lr": 0.01}});
        let queued = command(&["train", &input.to_string()]);

        let invocation = job_invocation(&queued);

        assert_eq!(invocation, Some(("train".to_string(), input)));
    }

    #[rstest]
    #[case::flags("train --epochs 10")]
    #[case::input_not_an_object("train '[1]'")]
    #[case::flag_for_a_job("--epochs '{}'")]
    #[case::extra_argument("train '{}' extra")]
    fn given_another_command_when_reading_it_as_a_job_and_input_then_there_is_none(
        #[case] queued: &str,
    ) {
        let invocation = job_invocation(queued);

        assert_eq!(invocation, None);
    }

    #[test]
    fn given_no_line_read_yet_when_following_then_it_reads_the_last_lines_it_attaches_with() {
        let follower = LogFollower::new(LogStart::Last(LINES_ON_ATTACH));

        assert_eq!(
            follower.read_at(),
            (JobLogCursor::Last, Some(LINES_ON_ATTACH))
        );
    }

    #[test]
    fn given_lines_read_on_attach_when_following_then_it_reads_a_page_after_the_newest() {
        let mut follower = LogFollower::new(LogStart::Last(LINES_ON_ATTACH));

        let more = follower.take(&page(15..25, true));

        assert!(!more);
        assert_eq!(
            follower.read_at(),
            (JobLogCursor::After(24), Some(LOG_PAGE_LINES))
        );
    }

    #[test]
    fn given_a_follower_that_attached_to_an_empty_log_when_reading_again_then_it_reads_from_the_first_line()
     {
        let mut follower = LogFollower::new(LogStart::Last(LINES_ON_ATTACH));

        follower.take(&page(0..0, false));

        assert_eq!(
            follower.read_at(),
            (
                JobLogCursor::Before(u64::from(LOG_PAGE_LINES)),
                Some(LOG_PAGE_LINES)
            )
        );
    }

    #[test]
    fn given_no_line_read_yet_when_reading_from_the_first_line_then_it_reads_the_lines_before_the_first_page_ends()
     {
        let follower = LogFollower::new(LogStart::First);

        assert_eq!(
            follower.read_at(),
            (
                JobLogCursor::Before(u64::from(LOG_PAGE_LINES)),
                Some(LOG_PAGE_LINES)
            )
        );
    }

    #[test]
    fn given_a_first_page_with_lines_when_reading_from_the_first_line_then_it_reads_on_after_the_newest()
     {
        let mut follower = LogFollower::new(LogStart::First);

        let more = follower.take(&page(0..1000, false));

        assert!(more);
        assert_eq!(
            follower.read_at(),
            (JobLogCursor::After(999), Some(LOG_PAGE_LINES))
        );
    }

    #[test]
    fn given_an_empty_log_when_reading_from_the_first_line_then_it_stops() {
        let mut follower = LogFollower::new(LogStart::First);

        let more = follower.take(&page(0..0, false));

        assert!(!more);
    }

    #[test]
    fn given_more_lines_after_the_newest_when_following_then_it_reads_on_at_once() {
        let mut follower = LogFollower::new(LogStart::Last(LINES_ON_ATTACH));
        follower.take(&page(0..3, false));

        let more = follower.take(&page(3..1003, true));

        assert!(more);
        assert_eq!(
            follower.read_at(),
            (JobLogCursor::After(1002), Some(LOG_PAGE_LINES))
        );
    }
}
