use std::borrow::Cow;

use anyhow::Context;
use serde::Serialize;

use super::RunArgs;
use crate::commands::jobs::follow_job;
use crate::commands::login::get_client_and_login_if_needed;
use crate::commands::package::{Mode, PackageArgs, build_package};
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::{require_workspace_project, validate_project_exists_on_server};
use crate::ui::{Declined, Human, Outcome, Render, Unanswered};

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

/// Package the workspace like `tracel package` with `packaging`, then queue it as a job on
/// `group` that runs the program with `command`.
pub fn handle_command(
    group: &str,
    command: String,
    packaging: &PackageArgs,
    args: &RunArgs,
    context: &CliContext,
) -> anyhow::Result<Outcome> {
    packaging.check(context.terminal())?;
    let terminal = context.terminal();

    let (project, source) = require_workspace_project(context)?;
    let tracel_project = project.get_project();
    let arguments = if command.is_empty() {
        "no arguments".to_string()
    } else {
        format!("arguments `{command}`")
    };
    let description = format!(
        "a job in {}/{} on compute provider group '{group}' with {arguments}",
        tracel_project.namespace, tracel_project.name
    );
    if !args.yes && !args.dry_run {
        terminal.require_confirmation(queueing(group))?;
    }

    if args.dry_run {
        let package = build_package(context, &project, packaging)?;
        terminal.info(&format!(
            "Would queue {description}, running code version {}.",
            package.digest
        ));
        terminal.outro("Dry run: nothing was uploaded or queued.");
        return Ok(Outcome::data(DryRun {
            namespace: tracel_project.namespace.clone(),
            project: tracel_project.name.clone(),
            group: group.to_string(),
            mode: package.mode,
            digest: package.digest,
            command,
        }));
    }

    let client = get_client_and_login_if_needed(context)?;
    validate_project_exists_on_server(tracel_project, &source, &client)?;
    let package = build_package(context, &project, packaging)?;
    if !args.yes {
        let confirmed = terminal.confirm(
            &format!(
                "Queue {description}, running code version {}? It may incur costs.",
                package.digest
            ),
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
            &tracel_project.namespace,
            &tracel_project.name,
            group,
            &version.digest,
            &command,
        )
        .with_context(|| format!("Can't queue a job on compute provider group '{group}'"))?;
    let job = QueuedJob {
        job_num: queued.job_num,
        job_id: queued.job_id,
        group: group.to_string(),
        digest: version.digest,
        version_id: version.version_id,
        command,
        uploaded: version.uploaded,
    };
    if args.follow {
        context.output().event(&QueuedEvent { job: &job })?;
    }

    let Some(num) = job.job_num else {
        terminal.step(&format!("Queued job {} on {group}", job.job_id));
        if args.follow {
            return Err(CliError::new(
                ErrorKind::Internal,
                format!(
                    "Job {} was queued, but the server did not return its number to follow it.",
                    job.job_id
                ),
            )
            .with_hint(
                "Find the job with `tracel jobs list`, then run `tracel jobs logs <NUM> --follow`.",
            )
            .into());
        }
        terminal.info("Find its number with `tracel jobs list`.");
        terminal.outro("Job queued.");
        return Ok(Outcome::data(job));
    };

    if args.follow {
        terminal.step(&format!("Queued job {num} on {group}"));
        follow_job(&client, context, tracel_project, num)?;
        terminal.outro(&format!("Job {num} completed."));
        return Ok(Outcome::streamed());
    }

    terminal.info(&format!(
        "Follow its logs with `tracel jobs logs {num} --follow`, or wait for it with `tracel jobs wait {num}`."
    ));
    terminal.outro(&format!("Queued job {num} on {group}."));
    Ok(Outcome::data(job))
}

/// What queueing a job says when the user can't confirm it.
fn queueing(group: &str) -> Unanswered {
    Unanswered::new(
        format!("Queueing a job on '{group}' needs confirmation."),
        "Pass --yes to queue it without asking.",
    )
}

/// The job command that gives the program `arguments` unchanged.
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn command(arguments: &[&str]) -> String {
        job_command(
            &arguments
                .iter()
                .map(|argument| argument.to_string())
                .collect::<Vec<_>>(),
        )
    }

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
        assert_eq!(serde_json::to_value(&job).unwrap(), data);
    }

    #[test]
    fn plain_arguments_are_joined_unchanged() {
        assert_eq!(command(&[]), "");
        assert_eq!(
            command(&["train", "--epochs", "10", "--lr=0.01"]),
            "train --epochs 10 --lr=0.01"
        );
        assert_eq!(
            command(&["$HOME", "*.rs", "~/data", "a#b", "x|y;z", "--name=é"]),
            "$HOME *.rs ~/data a#b x|y;z --name=é"
        );
    }

    #[test]
    fn arguments_the_split_would_change_are_single_quoted() {
        for (argument, quoted) in [
            ("", "''"),
            ("my run", "'my run'"),
            ("tab\there", "'tab\there'"),
            ("two\nlines", "'two\nlines'"),
            ("#tag", "'#tag'"),
            (r#"say "hi""#, r#"'say "hi"'"#),
            (r"C:\data", r"'C:\data'"),
            ("it's", r"'it'\''s'"),
            ("'", r"''\'''"),
        ] {
            assert_eq!(command(&[argument]), quoted, "{argument:?}");
        }
        assert_eq!(
            command(&["--name", "my run", "", "--tag", "it's"]),
            r"--name 'my run' '' --tag 'it'\''s'"
        );
    }
}
