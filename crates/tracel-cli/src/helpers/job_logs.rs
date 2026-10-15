//! Following a job's logs until it finishes: live, as its compute provider streams them,
//! while it runs, and from the log stored once it has ended.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use serde::Serialize;
use tracel_client::WebSocketClient;
use tracel_client::console::Client;
use tracel_client::console::job::response::{JobLogResponse, JobLogStreamMessage, JobResponse};
use tracel_client::websocket::WebSocketError;

use super::jobs::{final_status, missing_job, read_job, require_completed};
use crate::context::CliContext;
use crate::tools::tracel_config::TracelProject;
use crate::ui::{Human, Output, Render};

/// How often a followed job is read again once its logs are caught up.
const FOLLOW_INTERVAL: Duration = Duration::from_secs(2);

/// How long to wait before reading a live log that had nothing new again.
const PAUSE_BETWEEN_EMPTY_LIVE_LOG_READS: Duration = Duration::from_millis(100);

/// An event of following a job's logs. As JSON, a page of logs gains `"type": "log"`.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LogEvent<'a> {
    Log(&'a JobLogResponse),
    LiveLog {
        logs: &'a str,
    },
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
            Self::LiveLog { logs } => out.write_all(logs.as_bytes()),
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

/// The log a compute provider uploads once the job has ended, written from a byte on.
struct StoredLogFollower {
    start: u64,
    has_written_logs: bool,
    ends_mid_line: bool,
}

impl StoredLogFollower {
    fn write_what_is_new(
        &mut self,
        client: &Client,
        output: &Output,
        project: &TracelProject,
        num: i32,
    ) -> anyhow::Result<()> {
        loop {
            let page = client
                .get_job_logs(&project.namespace, &project.name, num, Some(self.start))
                .map_err(|error| missing_job(error, project, num))?;
            if !page.logs.is_empty() {
                output.event(&LogEvent::Log(&page))?;
                self.has_written_logs = true;
                self.ends_mid_line = !page.logs.ends_with('\n');
            }
            let more;
            (self.start, more) = next_read(self.start, &page)?;
            if !more {
                return Ok(());
            }
        }
    }
}

enum LiveStream {
    NotOpened,
    Open(Box<WebSocketClient>),
    Unavailable,
}

/// The lines a running job's compute provider streams, written as they come.
struct LiveLogFollower {
    stream: LiveStream,
    has_written_lines: bool,
    ends_mid_line: bool,
}

impl LiveLogFollower {
    /// Write the lines that come until `deadline`, opening the stream first when it isn't
    /// open. A message that isn't a line is skipped. A stream that fails is opened again
    /// the next time; one that can't be opened is given up with a warning.
    fn write_lines_until(
        &mut self,
        deadline: Instant,
        client: &Client,
        context: &CliContext,
        project: &TracelProject,
        num: i32,
    ) -> anyhow::Result<()> {
        if let LiveStream::NotOpened = self.stream {
            self.stream = open_or_warn(client, context, project, num);
        }
        let LiveStream::Open(stream) = &mut self.stream else {
            return Ok(());
        };
        while Instant::now() < deadline {
            match stream.receive::<JobLogStreamMessage>() {
                Ok(Some(JobLogStreamMessage::Log(line))) => {
                    context.output().event(&LogEvent::LiveLog { logs: &line })?;
                    self.has_written_lines = true;
                    self.ends_mid_line = !line.ends_with('\n');
                }
                Ok(None) => std::thread::sleep(
                    PAUSE_BETWEEN_EMPTY_LIVE_LOG_READS
                        .min(deadline.saturating_duration_since(Instant::now())),
                ),
                Err(WebSocketError::SerializationError(_)) => {}
                Err(_) => {
                    self.stream = LiveStream::NotOpened;
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

fn open_or_warn(
    client: &Client,
    context: &CliContext,
    project: &TracelProject,
    num: i32,
) -> LiveStream {
    match client.open_job_log_websocket(&project.namespace, &project.name, num) {
        Ok(stream) => LiveStream::Open(Box::new(stream)),
        Err(error) => {
            context.terminal().warning(&format!(
                "Can't follow job {num}'s log live: {error}. It shows once the job has ended."
            ));
            LiveStream::Unavailable
        }
    }
}

/// Write job `num`'s logs from byte `start` until the job has finished, and return it as
/// last read. Its compute provider uploads the stored log once the job has ended, so while
/// the stored log is empty, the lines the provider streams are written live instead, and
/// the stored log isn't written after them. The job is read before the stored log, so the
/// stored log read after a final status is complete.
pub fn follow_logs(
    client: &Client,
    context: &CliContext,
    project: &TracelProject,
    num: i32,
    start: u64,
) -> anyhow::Result<JobResponse> {
    let output = context.output();
    let mut stored_log = StoredLogFollower {
        start,
        has_written_logs: false,
        ends_mid_line: false,
    };
    let mut live_log = LiveLogFollower {
        stream: LiveStream::NotOpened,
        has_written_lines: false,
        ends_mid_line: false,
    };
    let job = loop {
        let job = read_job(client, project, num)?;
        if !live_log.has_written_lines {
            stored_log.write_what_is_new(client, output, project, num)?;
        }
        if final_status(&job.status).is_some() {
            break job;
        }
        let next_read_at = Instant::now() + FOLLOW_INTERVAL;
        if !stored_log.has_written_logs {
            live_log.write_lines_until(next_read_at, client, context, project, num)?;
        }
        std::thread::sleep(next_read_at.saturating_duration_since(Instant::now()));
    };
    let ends_mid_line = match live_log.has_written_lines {
        true => live_log.ends_mid_line,
        false => stored_log.ends_mid_line,
    };
    output.event(&LogEvent::End {
        status: &job.status,
        mid_line: ends_mid_line,
    })?;
    if live_log.has_written_lines {
        context.terminal().info(&format!(
            "The whole log, with what came before following it: `tracel jobs logs {num}`."
        ));
    }
    Ok(job)
}

/// Follow job `num`'s logs from the start until it finishes, and fail unless it
/// completed.
pub fn follow_job(
    client: &Client,
    context: &CliContext,
    project: &TracelProject,
    num: i32,
) -> anyhow::Result<JobResponse> {
    let job = follow_logs(client, context, project, num, 0)?;
    require_completed(&job)?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::plain;

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
    fn given_a_page_when_reading_on_then_the_next_read_starts_where_it_ends() {
        let next = next_read(0, &page(8, true)).unwrap();

        assert_eq!(next, (8, true));
    }

    #[test]
    fn given_a_page_that_ends_before_the_start_when_reading_on_then_the_start_is_kept() {
        let next = next_read(16, &page(8, false)).unwrap();

        assert_eq!(next, (16, false));
    }

    #[test]
    fn given_more_logs_but_no_progress_when_reading_on_then_it_fails() {
        let next = next_read(8, &page(8, true));

        assert!(next.is_err());
    }

    #[test]
    fn given_logs_that_end_mid_line_when_the_job_ends_then_the_line_is_ended() {
        let end = LogEvent::End {
            status: "completed",
            mid_line: true,
        };

        let text = plain(&end);

        assert_eq!(text, "\n");
    }
}
