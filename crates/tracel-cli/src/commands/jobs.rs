use std::io::{self, Write};
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use serde::Serialize;
use tracel_client::ClientError;
use tracel_client::console::Client;
use tracel_client::console::job::response::{
    ExecutionContextResponse, JobLogResponse, JobResponse, MoneyResponse, ProjectJobsResponse,
};

use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::{Resource, map_resource_error, resolve_namespace_project};
use crate::tools::tracel_config::TracelProject;
use crate::ui::{Details, Human, Outcome, Render, Table, Terminal};

const FOLLOW_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Args, Debug)]
pub struct JobsArgs {
    #[command(subcommand)]
    pub command: JobsCommands,
}

#[derive(Subcommand, Debug)]
pub enum JobsCommands {
    /// List jobs in the selected project, newest first.
    List,
    /// Show a job by its project-scoped number.
    Get(GetArgs),
    /// Read job logs, optionally following until the job finishes.
    Logs(LogsArgs),
    /// Cancel a new, queued, or running job.
    Cancel(CancelArgs),
    /// Wait until a job finishes; exit 9 when it fails or is cancelled.
    Wait(WaitArgs),
}

#[derive(Args, Debug)]
pub struct GetArgs {
    /// Project-scoped job number.
    #[arg(value_parser = clap::value_parser!(i32).range(1..))]
    pub num: i32,
}

#[derive(Args, Debug)]
pub struct LogsArgs {
    /// Project-scoped job number.
    #[arg(value_parser = clap::value_parser!(i32).range(1..))]
    pub num: i32,
    /// Byte offset in the log file to read from (default: 0).
    #[arg(long, value_name = "BYTE")]
    pub start: Option<u64>,
    /// Poll every 2 seconds until finished; JSON output is NDJSON log and end events.
    #[arg(long)]
    pub follow: bool,
}

#[derive(Args, Debug)]
pub struct CancelArgs {
    /// Project-scoped job number.
    #[arg(value_parser = clap::value_parser!(i32).range(1..))]
    pub num: i32,
    /// Cancel without asking for confirmation.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct WaitArgs {
    /// Project-scoped job number.
    #[arg(value_parser = clap::value_parser!(i32).range(1..))]
    pub num: i32,
    /// Stop waiting after this many seconds (default: no limit).
    #[arg(long, value_name = "SECONDS")]
    pub timeout: Option<u64>,
    /// Seconds between status checks.
    #[arg(long, value_name = "SECONDS", default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..))]
    pub interval: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FinalStatus {
    Completed,
    Unsuccessful,
}

/// How a job ended, from the server's status, or `None` while the status can still change.
fn final_status(status: &str) -> Option<FinalStatus> {
    match status {
        "completed" => Some(FinalStatus::Completed),
        "failed" | "cancelled" => Some(FinalStatus::Unsuccessful),
        _ => None,
    }
}

/// The job as read after `cancel`, or nothing when the person declined.
#[derive(Serialize)]
#[serde(transparent)]
struct Cancelled(Option<JobResponse>);

impl Render for Cancelled {}

/// A job that completed, as `wait` last read it.
#[derive(Serialize)]
#[serde(transparent)]
struct Completed(JobResponse);

impl Render for Completed {}

impl Render for ProjectJobsResponse {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        Table::new([
            "JOB",
            "STATUS",
            "COMMAND",
            "CREATED AT",
            "STARTED AT",
            "COMPLETED AT",
        ])
        .shrink("COMMAND")
        .rows(self.jobs.iter().map(|job| {
            [
                job.job_num.to_string(),
                job.status.clone(),
                job.job_command.clone(),
                job.created_at.clone(),
                job.started_at.clone().unwrap_or_default(),
                job.completed_at.clone().unwrap_or_default(),
            ]
        }))
        .write(out)
    }
}

impl Render for JobResponse {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        let details = Details::new()
            .field("Job", self.job_num)
            .field(
                "Project",
                format!(
                    "{}/{}",
                    self.job_project.namespace_name, self.job_project.project_name
                ),
            )
            .field("Status", &self.status)
            .field("Status message", &self.status_message)
            .field("Command", &self.job_command)
            .field("Code version", &self.job_project_version)
            .optional("Compute provider ID", self.compute_provider_id);
        let details = match &self.execution_context {
            Some(ExecutionContextResponse::Managed {
                compute_provider_name,
                memory_gb,
                cpu_cores,
                num_gpus,
                lock_price_per_hour,
                final_cost,
                estimated_cost,
            }) => details
                .field(
                    "Compute provider",
                    format!("{compute_provider_name} (managed)"),
                )
                .field("CPU cores", cpu_cores)
                .field("Memory", format!("{memory_gb} GB"))
                .optional("GPUs", *num_gpus)
                .field("Price per hour", money(lock_price_per_hour))
                .optional("Estimated cost", estimated_cost.as_ref().map(money))
                .optional("Final cost", final_cost.as_ref().map(money)),
            Some(ExecutionContextResponse::SelfManaged {
                compute_provider_name,
            }) => details.field(
                "Compute provider",
                format!("{compute_provider_name} (self-managed)"),
            ),
            None => details,
        };
        details
            .field("Created at", &self.created_at)
            .optional("Started at", self.started_at.as_ref())
            .optional("Completed at", self.completed_at.as_ref())
            .write(out)
    }
}

fn money(money: &MoneyResponse) -> String {
    format!("{} {}", money.amount, money.currency)
}

/// One page of logs, ending with where the next page starts when there is one.
impl Render for JobLogResponse {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        out.write_all(self.logs.as_bytes())?;
        if !self.logs.is_empty() && !self.logs.ends_with('\n') {
            writeln!(out)?;
        }
        if self.has_more {
            let footer = out.dim(format!(
                "Showing bytes {} to {} of {}; read on with --start {}.",
                self.start, self.end, self.total_size, self.end
            ));
            writeln!(out, "{footer}")?;
        }
        Ok(())
    }
}

/// An event of `logs --follow`. As JSON, a page of logs gains `"type": "log"`.
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

fn missing_job(error: ClientError, project: &TracelProject, num: i32) -> anyhow::Error {
    map_resource_error(error, &project.owner, &project.name, Resource::Job(num))
}

fn read_job(client: &Client, project: &TracelProject, num: i32) -> anyhow::Result<JobResponse> {
    client
        .get_job(&project.owner, &project.name, num)
        .map_err(|error| missing_job(error, project, num))
}

fn cancel_error(error: ClientError, project: &TracelProject, num: i32) -> anyhow::Error {
    if error.is_conflict() {
        return CliError::new(
            ErrorKind::Conflict,
            format!("Job {num} can only be cancelled while new, queued, or running."),
        )
        .with_hint(format!("Show its status with `tracel jobs get {num}`."))
        .into();
    }
    missing_job(error, project, num)
}

/// Write a job's logs from byte offset `start` until the job is completed, failed, or
/// cancelled, and return the job as last read.
fn follow_logs(
    client: &Client,
    context: &CliContext,
    project: &TracelProject,
    num: i32,
    mut start: u64,
) -> anyhow::Result<JobResponse> {
    let output = context.output();
    // The job is read before the logs, so logs read after a final status are complete.
    let mut job = read_job(client, project, num)?;
    let mut mid_line = false;
    loop {
        let page = client
            .get_job_logs(&project.owner, &project.name, num, Some(start))
            .map_err(|error| missing_job(error, project, num))?;
        if !page.logs.is_empty() {
            output.event(&LogEvent::Log(&page))?;
            mid_line = !page.logs.ends_with('\n');
        }
        let previous = start;
        start = page.end.max(previous);
        if page.has_more {
            if start == previous {
                return Err(CliError::new(
                    ErrorKind::Internal,
                    "Log response has more data but no advancing position.",
                )
                .into());
            }
            // Drain a backlog right away; wait only once caught up.
            continue;
        }
        if final_status(&job.status).is_some() {
            output.event(&LogEvent::End {
                status: &job.status,
                mid_line,
            })?;
            context
                .terminal()
                .print(&format!("Job {num} ended with status {}.", job.status));
            return Ok(job);
        }
        std::thread::sleep(FOLLOW_INTERVAL);
        job = read_job(client, project, num)?;
    }
}

/// Follow a job's logs from the start like `jobs logs --follow`, then fail like `jobs wait`
/// unless the job completed.
pub fn follow_job(
    client: &Client,
    context: &CliContext,
    project: &TracelProject,
    num: i32,
) -> anyhow::Result<JobResponse> {
    let job = follow_logs(client, context, project, num, 0)?;
    if final_status(&job.status) == Some(FinalStatus::Completed) {
        return Ok(job);
    }
    Err(job_failed(&job).into())
}

fn job_failed(job: &JobResponse) -> CliError {
    let num = job.job_num;
    let message = if job.status_message.is_empty() {
        format!("Job {num} ended with status {}.", job.status)
    } else {
        format!(
            "Job {num} ended with status {}: {}",
            job.status, job.status_message
        )
    };
    CliError::new(ErrorKind::JobFailed, message)
        .with_hint(format!("Read the logs with `tracel jobs logs {num}`."))
}

/// Poll a job until its status is final. The spinner is left for the caller to settle
/// when the job completed.
fn wait_for_job(
    client: &Client,
    terminal: &Terminal,
    project: &TracelProject,
    num: i32,
    timeout: Option<Duration>,
    interval: Duration,
) -> anyhow::Result<JobResponse> {
    let started = Instant::now();
    let spinner = terminal.spinner();
    spinner.start(format!("Waiting for job {num}..."));
    let mut last_status = None;
    let result = (|| -> anyhow::Result<JobResponse> {
        loop {
            let job = read_job(client, project, num)?;
            if final_status(&job.status).is_some() {
                return Ok(job);
            }
            if last_status.as_ref() != Some(&job.status) {
                spinner.set_message(format!("Job {num} is {}.", job.status));
                last_status = Some(job.status.clone());
            }
            let pause = match timeout {
                Some(timeout) => {
                    let remaining = timeout.saturating_sub(started.elapsed());
                    if remaining.is_zero() {
                        return Err(CliError::new(
                            ErrorKind::Timeout,
                            format!(
                                "Job {num} is still {} after {} seconds.",
                                job.status,
                                timeout.as_secs()
                            ),
                        )
                        .with_hint(format!(
                            "Call `tracel jobs wait {num}` again; the job has not finished."
                        ))
                        .into());
                    }
                    interval.min(remaining)
                }
                None => interval,
            };
            std::thread::sleep(pause);
        }
    })();
    let job = result.inspect_err(|_| spinner.error(format!("Stopped waiting for job {num}.")))?;
    if final_status(&job.status) == Some(FinalStatus::Completed) {
        return Ok(job);
    }
    spinner.error(format!("Job {num} {}.", job.status));
    Err(job_failed(&job).into())
}

pub fn handle_command(args: JobsArgs, context: CliContext) -> anyhow::Result<Outcome> {
    let project = resolve_namespace_project(&context)?.project;
    let client = || get_client_and_login_if_needed(&context);
    match args.command {
        JobsCommands::List => Ok(client()?.list_jobs(&project.owner, &project.name)?.into()),
        JobsCommands::Get(args) => Ok(read_job(&client()?, &project, args.num)?.into()),
        JobsCommands::Logs(args) if args.follow => {
            let start = args.start.unwrap_or(0);
            follow_logs(&client()?, &context, &project, args.num, start)?;
            Ok(Outcome::streamed())
        }
        JobsCommands::Logs(args) => Ok(client()?
            .get_job_logs(&project.owner, &project.name, args.num, args.start)
            .map_err(|error| missing_job(error, &project, args.num))?
            .into()),
        JobsCommands::Cancel(args) => Ok(cancel(args, &context, &project)?.into()),
        JobsCommands::Wait(args) => Ok(wait(args, &context, &project)?.into()),
    }
}

fn cancel(
    args: CancelArgs,
    context: &CliContext,
    project: &TracelProject,
) -> anyhow::Result<Cancelled> {
    let (terminal, num) = (context.terminal(), args.num);
    terminal.command_title("Cancel job");
    if !args.yes {
        let confirmed = terminal.confirm(
            &format!(
                "Cancel job {num} in {}/{}? A running job is stopped.",
                project.owner, project.name
            ),
            "yes",
            false,
        )?;
        if !confirmed {
            terminal.cancel_finalize(&format!("Job {num} was not cancelled."));
            return Ok(Cancelled(None));
        }
    }
    let client = get_client_and_login_if_needed(context)?;
    client
        .cancel_job(&project.owner, &project.name, num)
        .map_err(|error| cancel_error(error, project, num))?;
    let job = read_job(&client, project, num)?;
    terminal.finalize(&format!("Job {num} is {}.", job.status));
    Ok(Cancelled(Some(job)))
}

fn wait(
    args: WaitArgs,
    context: &CliContext,
    project: &TracelProject,
) -> anyhow::Result<Completed> {
    let terminal = context.terminal();
    terminal.command_title("Wait for job");
    let client = get_client_and_login_if_needed(context)?;
    let job = wait_for_job(
        &client,
        terminal,
        project,
        args.num,
        args.timeout.map(Duration::from_secs),
        Duration::from_secs(args.interval),
    )?;
    terminal.finalize(&format!("Job {} completed.", args.num));
    Ok(Completed(job))
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};
    use serde_json::json;

    use super::*;
    use crate::cli::{CliArgs, Commands};

    fn parse_command(arguments: &[&str]) -> JobsArgs {
        let args = CliArgs::try_parse_from(arguments).unwrap();
        let Some(Commands::Jobs(args)) = args.command else {
            panic!("Expected jobs command");
        };
        args
    }

    #[test]
    fn only_completed_failed_and_cancelled_are_final() {
        assert_eq!(final_status("completed"), Some(FinalStatus::Completed));
        for status in ["failed", "cancelled"] {
            assert_eq!(final_status(status), Some(FinalStatus::Unsuccessful));
        }
        for status in [
            "new",
            "queued",
            "running",
            "pending_cancellation",
            "",
            "Completed",
            "unknown",
        ] {
            assert_eq!(final_status(status), None);
        }
    }

    #[test]
    fn command_definitions_are_valid_and_numbers_are_positive() {
        CliArgs::command().debug_assert();
        assert!(matches!(
            parse_command(&["tracel", "jobs", "list"]).command,
            JobsCommands::List
        ));
        let JobsCommands::Get(args) = parse_command(&["tracel", "jobs", "get", "12"]).command
        else {
            panic!("Expected get command");
        };
        assert_eq!(args.num, 12);
        for subcommand in ["get", "logs", "cancel", "wait"] {
            for value in ["0", "-1", "latest"] {
                assert!(CliArgs::try_parse_from(["tracel", "jobs", subcommand, value]).is_err());
            }
            assert!(CliArgs::try_parse_from(["tracel", "jobs", subcommand]).is_err());
        }
        assert!(CliArgs::try_parse_from(["tracel", "jobs", "list", "--page", "0"]).is_err());
    }

    #[test]
    fn logs_read_from_a_byte_offset_and_can_follow() {
        let args = parse_command(&["tracel", "jobs", "logs", "12"]);
        let JobsCommands::Logs(args) = args.command else {
            panic!("Expected logs command");
        };
        assert_eq!(args.num, 12);
        assert_eq!(args.start, None);
        assert!(!args.follow);

        let args = parse_command(&[
            "tracel", "jobs", "logs", "12", "--start", "4096", "--follow", "--json",
        ]);
        let JobsCommands::Logs(args) = args.command else {
            panic!("Expected logs command");
        };
        assert_eq!(args.start, Some(4096));
        assert!(args.follow);
        assert!(
            CliArgs::try_parse_from(["tracel", "jobs", "logs", "12", "--start", "-1"]).is_err()
        );
    }

    fn page(logs: &str, has_more: bool) -> JobLogResponse {
        JobLogResponse {
            logs: logs.into(),
            start: 0,
            end: logs.len() as u64,
            total_size: 4096,
            has_more,
        }
    }

    fn text(render: &impl Render) -> String {
        let mut text = Vec::new();
        render.render(&mut Human::plain(&mut text)).unwrap();
        String::from_utf8(text).unwrap()
    }

    #[test]
    fn a_log_page_ends_its_line_and_says_where_the_next_one_starts() {
        assert_eq!(text(&page("", false)), "");
        assert_eq!(text(&page("epoch 1\nepoch 2", false)), "epoch 1\nepoch 2\n");
        assert_eq!(
            text(&page("epoch 1\n", true)),
            "epoch 1\nShowing bytes 0 to 8 of 4096; read on with --start 8.\n"
        );
    }

    #[test]
    fn followed_logs_are_typed_events() {
        let page = page("epoch 1\nepo", true);
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
        for mid_line in [false, true] {
            let end = LogEvent::End {
                status: "completed",
                mid_line,
            };
            assert_eq!(
                serde_json::to_value(&end).unwrap(),
                json!({"type": "end", "status": "completed"})
            );
            assert_eq!(text(&end), if mid_line { "\n" } else { "" });
        }
        assert_eq!(text(&LogEvent::Log(&page)), "epoch 1\nepo");
    }

    #[test]
    fn cancel_confirmation_can_be_skipped() {
        for (arguments, yes) in [
            (vec!["tracel", "jobs", "cancel", "12"], false),
            (vec!["tracel", "jobs", "cancel", "12", "--yes"], true),
            (vec!["tracel", "jobs", "cancel", "12", "-y"], true),
        ] {
            let JobsCommands::Cancel(args) = parse_command(&arguments).command else {
                panic!("Expected cancel command");
            };
            assert_eq!(args.num, 12);
            assert_eq!(args.yes, yes);
        }
    }

    #[test]
    fn wait_defaults_to_no_timeout_and_a_five_second_interval() {
        let JobsCommands::Wait(args) = parse_command(&["tracel", "jobs", "wait", "12"]).command
        else {
            panic!("Expected wait command");
        };
        assert_eq!(args.num, 12);
        assert_eq!(args.timeout, None);
        assert_eq!(args.interval, 5);

        let JobsCommands::Wait(args) = parse_command(&[
            "tracel",
            "jobs",
            "wait",
            "12",
            "--timeout",
            "0",
            "--interval",
            "1",
        ])
        .command
        else {
            panic!("Expected wait command");
        };
        assert_eq!(args.timeout, Some(0));
        assert_eq!(args.interval, 1);
        for (flag, value) in [
            ("--interval", "0"),
            ("--interval", "-1"),
            ("--timeout", "-1"),
            ("--timeout", "soon"),
        ] {
            assert!(
                CliArgs::try_parse_from(["tracel", "jobs", "wait", "12", flag, value]).is_err()
            );
        }
    }
}
