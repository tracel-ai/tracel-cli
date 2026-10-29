use std::io;
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use tracel_client::ClientError;
use tracel_client::console::job::response::{
    ExecutionContextResponse, JobResponse, ProjectJobsResponse,
};

use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::{
    FinalStatus, final_status, follow_logs, job_invocation, missing_job, poll_job, read_job,
    require_completed, resolve_namespace_project, show_logs,
};
use crate::tools::tracel_config::TracelProject;
use crate::ui::{
    Declined, Details, Human, Layout, Outcome, Render, Table, Unanswered, elapsed, list_summary,
    short_digest, when,
};

#[derive(Args, Debug)]
pub struct JobsArgs {
    #[command(subcommand)]
    pub command: JobsCommands,
}

impl JobsArgs {
    pub fn layout(&self) -> Layout {
        match &self.command {
            JobsCommands::Logs(args) if args.follow => {
                Layout::flow(format!("Follow the logs of job {}", args.num))
            }
            JobsCommands::Logs(_) => Layout::Text,
            JobsCommands::Cancel(args) => Layout::flow(format!("Cancel job {}", args.num)),
            JobsCommands::Wait(args) => Layout::flow(format!("Wait for job {}", args.num)),
            _ => Layout::Read,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum JobsCommands {
    /// List the project's jobs, newest first
    List,
    /// Show a job
    Get(GetArgs),
    /// Show a job's logs
    Logs(LogsArgs),
    /// Cancel a job
    Cancel(CancelArgs),
    /// Wait for a job to finish
    Wait(WaitArgs),
}

#[derive(Args, Debug)]
pub struct GetArgs {
    /// Job number
    #[arg(value_parser = clap::value_parser!(i32).range(1..))]
    pub num: i32,
}

#[derive(Args, Debug)]
pub struct LogsArgs {
    /// Job number
    #[arg(value_parser = clap::value_parser!(i32).range(1..))]
    pub num: i32,
    /// Show only its last N lines, or start following with them (10 by default)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub tail: Option<u32>,
    /// Keep showing new lines until it finishes
    #[arg(long)]
    pub follow: bool,
}

#[derive(Args, Debug)]
pub struct CancelArgs {
    /// Job number
    #[arg(value_parser = clap::value_parser!(i32).range(1..))]
    pub num: i32,
    /// Cancel without asking
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct WaitArgs {
    /// Job number
    #[arg(value_parser = clap::value_parser!(i32).range(1..))]
    pub num: i32,
    /// Stop waiting after this many seconds
    #[arg(long, value_name = "SECONDS")]
    pub timeout: Option<u64>,
    /// Check the job every this many seconds
    #[arg(long, value_name = "SECONDS", default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..))]
    pub interval: u64,
}

/// Name what a job with `job_command` runs: the job, for one `tracel run <job> --remote`
/// queued, or else the command.
fn command(job_command: &str) -> String {
    match job_invocation(job_command) {
        Some((name, _)) => name,
        None => job_command.to_string(),
    }
}

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
                command(&job.job_command),
                when(&job.created_at),
                job.started_at.as_deref().map(when).unwrap_or_default(),
                job.completed_at.as_deref().map(when).unwrap_or_default(),
            ]
        }))
        .write(out)
    }

    fn summary(&self) -> Option<String> {
        Some(list_summary(
            self.jobs.len(),
            self.jobs.len() as u64,
            ["job", "jobs"],
            "No jobs in this project yet.",
        ))
    }
}

impl Render for JobResponse {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        let details = Details::new()
            .field("Command", command(&self.job_command))
            .field("Code version", short_digest(&self.job_project_version));
        let details = match &self.execution_context {
            Some(ExecutionContextResponse::SelfManaged {
                compute_provider_name,
            }) => details.field(
                "Compute provider",
                format!("{compute_provider_name} (self-managed)"),
            ),
            None => details,
        };
        let details = details.optional("Ran", ran(self));
        match job_invocation(&self.job_command) {
            Some((_, input)) => details.section("Input", &input),
            None => details,
        }
        .write(out)
    }

    fn summary(&self) -> Option<String> {
        Some(state(self))
    }
}

/// Say when `job` started and for how long it ran, or has run so far.
fn ran(job: &JobResponse) -> Option<String> {
    let started = job.started_at.as_deref()?;
    let completed = job.completed_at.as_deref();
    let elapsed = elapsed(started, completed)?;
    let so_far = if completed.is_some() { "" } else { " so far" };
    Some(format!("{}, {elapsed}{so_far}", when(started)))
}

/// Say how `job` is going, or how it ended.
fn state(job: &JobResponse) -> String {
    let num = job.job_num;
    match job.status.as_str() {
        "completed" => format!("Job {num} completed."),
        "failed" => format!("Job {num} failed."),
        "cancelled" => format!("Job {num} was cancelled."),
        "pending_cancellation" => format!("Job {num} is stopping."),
        status => format!("Job {num} is {status}."),
    }
}

fn cancel_error(error: ClientError, project: &TracelProject, num: i32) -> anyhow::Error {
    if error.is_conflict() {
        return CliError::new(
            ErrorKind::Conflict,
            format!("Job {num} is no longer running, so it can't be cancelled."),
        )
        .into();
    }
    missing_job(error, project, num)
}

pub fn handle_command(args: JobsArgs, context: CliContext) -> anyhow::Result<Outcome> {
    let project = resolve_namespace_project(&context)?.project;
    let client = || get_client_and_login_if_needed(&context);
    match args.command {
        JobsCommands::List => Ok(client()?
            .list_jobs(&project.namespace, &project.name)?
            .into()),
        JobsCommands::Get(args) => Ok(read_job(&client()?, &project, args.num)?.into()),
        JobsCommands::Logs(args) if args.follow => {
            let job = follow_logs(&client()?, context.output(), &project, args.num, args.tail)?;
            context.terminal().outro(&state(&job));
            Ok(Outcome::streamed())
        }
        JobsCommands::Logs(args) => {
            show_logs(&client()?, context.output(), &project, args.num, args.tail)?;
            Ok(Outcome::streamed())
        }
        JobsCommands::Cancel(args) => Ok(Outcome::data(cancel(args, &context, &project)?)),
        JobsCommands::Wait(args) => Ok(Outcome::data(wait(args, &context, &project)?)),
    }
}

/// Cancel job `num` once the user agrees, and return it as read afterwards.
fn cancel(
    args: CancelArgs,
    context: &CliContext,
    project: &TracelProject,
) -> anyhow::Result<JobResponse> {
    let (terminal, num) = (context.terminal(), args.num);
    if !args.yes {
        let confirmed = terminal.confirm(
            "Cancel it? A running job is stopped.",
            false,
            Unanswered::new(
                format!("Cancelling job {num} needs confirmation."),
                "Pass --yes to cancel without asking.",
            ),
        )?;
        if !confirmed {
            return Err(Declined::new(format!("Job {num} was not cancelled.")).into());
        }
    }
    let client = get_client_and_login_if_needed(context)?;
    client
        .cancel_job(&project.namespace, &project.name, num)
        .map_err(|error| cancel_error(error, project, num))?;
    let job = read_job(&client, project, num)?;
    terminal.outro(&match final_status(&job.status) {
        Some(FinalStatus::Cancelled) => format!("Cancelled job {num}."),
        _ => format!("Job {num} is stopping."),
    });
    Ok(job)
}

/// Wait for job `num` to finish, and return it once it completed.
fn wait(
    args: WaitArgs,
    context: &CliContext,
    project: &TracelProject,
) -> anyhow::Result<JobResponse> {
    let (terminal, num) = (context.terminal(), args.num);
    let client = get_client_and_login_if_needed(context)?;
    let deadline = args
        .timeout
        .map(|timeout| Instant::now() + Duration::from_secs(timeout));
    let spinner = terminal.spinner(format!("Waiting for job {num}"));
    let mut status = String::new();
    let job = poll_job(
        &client,
        project,
        num,
        Duration::from_secs(args.interval),
        deadline,
        |job| {
            if job.status != status {
                status.clone_from(&job.status);
                spinner.set_message(format!("Waiting for job {num}, which is {status}"));
            }
            Ok(())
        },
    );
    spinner.clear();
    let job = job?;
    waited(&job, args.timeout)?;
    terminal.outro(&state(&job));
    Ok(job)
}

/// Decide what waiting for a job comes to, with `job` as last read: it passes when the job
/// completed, fails with `JOB_FAILED` when it failed or was cancelled, and with `TIMEOUT`
/// when it is still going after `timeout` seconds.
fn waited(job: &JobResponse, timeout: Option<u64>) -> Result<(), CliError> {
    let num = job.job_num;
    if final_status(&job.status).is_none() {
        return Err(CliError::new(
            ErrorKind::Timeout,
            format!(
                "Job {num} is still {} after {} seconds.",
                job.status,
                timeout.unwrap_or_default()
            ),
        )
        .with_hint(format!("Wait for it again with `tracel jobs wait {num}`.")));
    }
    require_completed(job)
        .map_err(|error| error.with_hint(format!("Read its logs with `tracel jobs logs {num}`.")))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use tracel_client::console::job::response::JobProjectResponse;

    use super::*;
    use crate::error::ErrorReport;

    fn job(status: &str) -> JobResponse {
        JobResponse {
            job_num: 14,
            compute_provider_id: None,
            job_project: JobProjectResponse {
                project_name: "demo".into(),
                namespace_name: "test".into(),
            },
            job_project_version: "67c31f69845e9a1e".into(),
            job_command: "train".into(),
            status: status.into(),
            status_message: String::new(),
            execution_context: None,
            created_at: "2026-10-13T15:00:00Z".into(),
            started_at: Some("2026-10-13T15:02:00Z".into()),
            completed_at: Some("2026-10-13T15:03:05Z".into()),
        }
    }

    #[test]
    fn given_a_completed_job_when_waiting_for_it_then_the_wait_passes() {
        let waited = waited(&job("completed"), None);

        assert!(waited.is_ok());
    }

    #[rstest]
    #[case::failed("failed")]
    #[case::cancelled("cancelled")]
    fn given_a_job_that_did_not_complete_when_waiting_for_it_then_the_wait_fails_saying_how_it_ended(
        #[case] status: &str,
    ) {
        let error = waited(&job(status), None).unwrap_err();

        assert_eq!(error.kind, ErrorKind::JobFailed);
        assert!(error.to_string().contains(status), "{error}");
    }

    #[test]
    fn given_a_failed_job_when_waiting_for_it_then_the_hint_points_at_its_logs() {
        let error = anyhow::Error::new(waited(&job("failed"), None).unwrap_err());

        let hint = ErrorReport::new(&error).hint.unwrap();

        assert!(hint.contains("tracel jobs logs 14"), "{hint}");
    }

    #[test]
    fn given_a_job_still_running_when_the_wait_times_out_then_it_is_a_timeout() {
        let error = waited(&job("running"), Some(60)).unwrap_err();

        assert_eq!(error.kind, ErrorKind::Timeout);
    }

    #[test]
    fn given_a_job_tracel_run_queued_by_name_when_showing_its_command_then_it_is_the_job_name() {
        let shown = command(r#"train '{"epochs":2}'"#);

        assert_eq!(shown, "train");
    }

    #[test]
    fn given_another_command_when_showing_it_then_it_is_shown_as_it_is() {
        let shown = command("train --epochs 2");

        assert_eq!(shown, "train --epochs 2");
    }

    #[test]
    fn given_a_finished_job_when_saying_how_long_it_ran_then_it_is_from_its_start_to_its_end() {
        let ran = ran(&job("completed"));

        assert_eq!(
            ran,
            Some(format!("{}, 1 min 5 s", when("2026-10-13T15:02:00Z")))
        );
    }

    #[test]
    fn given_a_running_job_when_saying_how_long_it_ran_then_it_is_so_far() {
        let running = JobResponse {
            completed_at: None,
            ..job("running")
        };

        let ran = ran(&running).unwrap();

        assert!(ran.ends_with(" so far"), "{ran}");
    }

    #[test]
    fn given_a_job_that_has_not_started_when_saying_how_long_it_ran_then_there_is_nothing_to_say() {
        let not_started = JobResponse {
            started_at: None,
            completed_at: None,
            ..job("new")
        };

        let ran = ran(&not_started);

        assert_eq!(ran, None);
    }
}
