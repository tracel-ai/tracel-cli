use anyhow::Context;
use serde::Serialize;

use crate::commands::login::get_client_and_login_if_needed;
use crate::commands::package::{Mode, PackageArgs, build_package};
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::{follow_job, require_workspace_project, validate_project_exists_on_server};
use crate::ui::{Declined, Human, Outcome, Render, Terminal, Unanswered, short_digest};

/// A job `tracel run --remote` queues on a compute provider group.
pub struct RemoteRun<'a> {
    pub group: &'a str,
    /// The command the job runs the program with.
    pub command: String,
    pub packaging: PackageArgs,
    /// Queue it without asking.
    pub yes: bool,
    /// Package it and say what would be queued, without uploading or queuing anything.
    pub dry_run: bool,
    /// Follow its logs until it finishes.
    pub follow: bool,
}

/// A queued job: the data of `run --remote`, and its first NDJSON item with `--follow`.
#[derive(Serialize)]
struct QueuedJob {
    job_num: Option<i32>,
    job_id: String,
    group: String,
    digest: String,
    version_id: String,
    command: String,
    uploaded: bool,
}

/// The first event of `--follow`. As JSON, the queued job gains `"type": "queued"`.
#[derive(Serialize)]
#[serde(tag = "type", rename = "queued")]
struct QueuedEvent<'a> {
    #[serde(flatten)]
    job: &'a QueuedJob,
}

impl Render for QueuedEvent<'_> {
    fn render(&self, _: &mut Human<'_>) -> std::io::Result<()> {
        Ok(())
    }
}

/// The job `--dry-run` would queue.
#[derive(Serialize)]
struct DryRun {
    namespace: String,
    project: String,
    group: String,
    mode: Mode,
    digest: String,
    command: String,
}

/// Say what queueing a job on `group` needs when the user can't confirm it.
fn queueing(group: &str) -> Unanswered {
    Unanswered::new(
        format!("Queueing a job on '{group}' needs confirmation."),
        "Pass --yes to queue it without asking.",
    )
}

/// Fail before any slow work when a run on `group` needs answers the user can't give:
/// a packaging mode, or, when it `asks`, whether to queue the job.
pub fn check(
    group: &str,
    packaging: &PackageArgs,
    asks: bool,
    terminal: &Terminal,
) -> anyhow::Result<()> {
    packaging.check(terminal)?;
    if asks {
        terminal.require_confirmation(queueing(group))?;
    }
    Ok(())
}

/// Package the workspace as `tracel package` does, then queue it as a job, once the user
/// agrees, since a job may incur costs.
pub fn queue(run: RemoteRun, context: &CliContext) -> anyhow::Result<Outcome> {
    let terminal = context.terminal();
    let group = run.group;
    let (project, source) = require_workspace_project(context)?;
    let linked = project.get_project();

    if run.dry_run {
        let package = build_package(context, &project, &run.packaging)?;
        terminal.info(&format!(
            "Would queue it on {group} with code version {}.",
            short_digest(&package.digest)
        ));
        terminal.outro("Dry run: nothing was uploaded or queued.");
        return Ok(Outcome::data(DryRun {
            namespace: linked.namespace.clone(),
            project: linked.name.clone(),
            group: group.to_string(),
            mode: package.mode,
            digest: package.digest,
            command: run.command,
        }));
    }

    let client = get_client_and_login_if_needed(context)?;
    validate_project_exists_on_server(linked, &source, &client)?;
    let package = build_package(context, &project, &run.packaging)?;
    let digest = short_digest(&package.digest).to_string();
    if !run.yes {
        let confirmed = terminal.confirm(
            &format!("Queue it on {group} with code version {digest}? It may incur costs."),
            false,
            queueing(group),
        )?;
        if !confirmed {
            return Err(Declined::new("No job was queued.").into());
        }
    }

    let version = package.publish(context, &client, &project)?;
    let queued = client
        .queue_job(
            &linked.namespace,
            &linked.name,
            group,
            &version.digest,
            &run.command,
        )
        .with_context(|| format!("Can't queue a job on '{group}'"))?;
    let job = QueuedJob {
        job_num: queued.job_num,
        job_id: queued.job_id,
        group: group.to_string(),
        digest: version.digest,
        version_id: version.version_id,
        command: run.command,
        uploaded: version.uploaded,
    };
    if run.follow {
        context.output().event(&QueuedEvent { job: &job })?;
    }

    let Some(num) = job.job_num else {
        let queued = format!("Queued a job with code version {digest}.");
        if run.follow {
            terminal.success(&queued);
            return Err(CliError::new(
                ErrorKind::Internal,
                "The Tracel Console didn't send the job's number, so it can't be followed.",
            )
            .with_hint("Find it with `tracel jobs list`, then follow it with `tracel jobs logs <number> --follow`.")
            .into());
        }
        terminal.next_steps(&queued, &["tracel jobs list".to_string()]);
        return Ok(Outcome::data(job));
    };

    let queued = format!("Queued job {num} with code version {digest}.");
    if run.follow {
        terminal.success(&queued);
        follow_job(&client, context.output(), linked, num)?;
        terminal.outro(&format!("Job {num} completed."));
        return Ok(Outcome::streamed());
    }
    terminal.next_steps(
        &queued,
        &[
            format!("tracel jobs logs {num} --follow"),
            format!("tracel jobs wait {num}"),
        ],
    );
    Ok(Outcome::data(job))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_queued_job_is_the_data_and_the_first_followed_event() {
        let job = QueuedJob {
            job_num: Some(12),
            job_id: "0f6c".into(),
            group: "gpu".into(),
            digest: "9a1e".into(),
            version_id: "b2d4".into(),
            command: "train --epochs 10".into(),
            uploaded: true,
        };
        let data = json!({
            "job_num": 12,
            "job_id": "0f6c",
            "group": "gpu",
            "digest": "9a1e",
            "version_id": "b2d4",
            "command": "train --epochs 10",
            "uploaded": true,
        });
        let mut event = data.clone();
        event["type"] = json!("queued");
        assert_eq!(
            serde_json::to_value(QueuedEvent { job: &job }).unwrap(),
            event
        );
        assert_eq!(serde_json::to_value(job).unwrap(), data);
    }
}
