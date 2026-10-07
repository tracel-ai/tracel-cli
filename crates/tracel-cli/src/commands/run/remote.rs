use std::borrow::Cow;

use anyhow::Context;
use serde::Serialize;

use super::RunArgs;
use crate::commands::jobs::follow_job;
use crate::commands::login::get_client_and_login_if_needed;
use crate::commands::package::{Mode, build_package};
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::{require_workspace_project, validate_project_exists_on_server};
use crate::ui::{Outcome, Render};

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

/// The job `run --remote` queued, or nothing when the person declined.
#[derive(Serialize)]
#[serde(transparent)]
struct Queued(Option<QueuedJob>);

impl Render for Queued {}

/// The first event of `--follow`. As JSON, the queued job gains `"type": "queued"`.
#[derive(Serialize)]
#[serde(tag = "type", rename = "queued")]
struct QueuedEvent<'a> {
    #[serde(flatten)]
    job: &'a QueuedJob,
}

impl Render for QueuedEvent<'_> {}

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

impl Render for DryRun {}

/// Package the workspace like `tracel package`, then queue it as a job on `group` with the
/// forwarded arguments.
pub fn handle_command(
    group: &str,
    args: &RunArgs,
    context: &CliContext,
) -> anyhow::Result<Outcome> {
    args.package.check(context.terminal())?;
    let command = job_command(&args.forwarded);
    let terminal = context.terminal();
    terminal.command_title("Run remotely");

    let project = require_workspace_project(context)?;
    let tracel_project = project.get_project();
    let arguments = if command.is_empty() {
        "no arguments".to_string()
    } else {
        format!("arguments `{command}`")
    };
    let description = format!(
        "a job in {}/{} on compute provider group '{group}' with {arguments}",
        tracel_project.owner, tracel_project.name
    );
    if !args.yes && !args.dry_run {
        terminal
            .require_confirmation(&format!("Queue {description}? It may incur costs."), "yes")?;
    }

    if args.dry_run {
        let package = build_package(context, &project, &args.package)?;
        terminal.print(&format!(
            "Would queue {description}, running code version {}.",
            package.digest
        ));
        terminal.finalize("Dry run: nothing was uploaded or queued.");
        return Ok(DryRun {
            namespace: tracel_project.owner.clone(),
            project: tracel_project.name.clone(),
            group: group.to_string(),
            mode: package.mode,
            digest: package.digest,
            command,
        }
        .into());
    }

    let client = get_client_and_login_if_needed(context)?;
    validate_project_exists_on_server(&project, &client)?;
    let package = build_package(context, &project, &args.package)?;
    if !args.yes {
        let confirmed = terminal.confirm(
            &format!(
                "Queue {description}, running code version {}? It may incur costs.",
                package.digest
            ),
            "yes",
            false,
        )?;
        if !confirmed {
            terminal.cancel_finalize("No job was queued.");
            return Ok(Queued(None).into());
        }
    }

    let version = package.publish(context, &client, &project)?;
    let queued = client
        .queue_job(
            &tracel_project.owner,
            &tracel_project.name,
            group,
            &version.digest,
            &command,
        )
        .with_context(|| format!("Failed to queue a job on compute provider group '{group}'"))?;
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
        terminal.print_success(&format!("Queued job {} on {group}.", job.job_id));
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
        terminal.print("Find its number with `tracel jobs list`.");
        terminal.finalize("Job queued.");
        return Ok(Queued(Some(job)).into());
    };

    if args.follow {
        terminal.print_success(&format!("Queued job {num} on {group}. Following its logs."));
        follow_job(&client, context, tracel_project, num)?;
        terminal.finalize(&format!("Job {num} completed."));
        return Ok(Outcome::streamed());
    }

    terminal.print(&format!(
        "Follow its logs with `tracel jobs logs {num} --follow`, or wait for it with `tracel jobs wait {num}`."
    ));
    terminal.finalize(&format!("Queued job {num} on {group}."));
    Ok(Queued(Some(job)).into())
}

/// The job command that gives the program `arguments` unchanged.
///
/// The compute provider splits the command into words with shell quoting rules (quotes,
/// backslashes, and `#` comments, without expansions), so every argument the split would
/// change is single-quoted.
fn job_command(arguments: &[String]) -> String {
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
        assert_eq!(serde_json::to_value(Queued(Some(job))).unwrap(), data);
        assert_eq!(serde_json::to_value(Queued(None)).unwrap(), json!(null));
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
