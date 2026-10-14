use anyhow::Context;
use tracel_client::console::Client;
use tracel_client::console::model::request::CreateModelRequest;

use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::ui::Unanswered;

/// Make sure `model_name` exists, creating it when `auto_create` says so or the user
/// agrees. False when the user declined to create it.
pub fn ensure_model_exists(
    context: &CliContext,
    client: &Client,
    namespace: &str,
    project: &str,
    model_name: &str,
    auto_create: Option<bool>,
    description: Option<String>,
) -> anyhow::Result<bool> {
    match client.get_model(namespace, project, model_name) {
        Ok(_) => return Ok(true),
        Err(e) if e.is_not_found() => {}
        Err(e) => return Err(e).with_context(|| format!("Can't look up model '{model_name}'")),
    }

    let create = match auto_create {
        Some(create) => create,
        None if context.terminal().is_interactive() => {
            if !context.terminal().confirm(
                &format!("Create model '{model_name}'?"),
                false,
                Unanswered::new(
                    format!("Creating model '{model_name}' needs confirmation."),
                    "Pass --auto-create true to create it.",
                ),
            )? {
                return Ok(false);
            }
            true
        }
        None => false,
    };

    if !create {
        return Err(CliError::new(
            ErrorKind::Usage,
            format!("No model '{model_name}' in {namespace}/{project}."),
        )
        .with_hint("Pass --auto-create true to create it.")
        .into());
    }

    let description = match description {
        Some(description) => Some(description),
        None if auto_create.is_none() => {
            let description = context.terminal().input(
                "Description (optional)",
                Unanswered::new(
                    format!("Creating model '{model_name}' needs a description."),
                    "Pass --description <text>.",
                ),
            )?;
            let description = description.trim().to_string();
            (!description.is_empty()).then_some(description)
        }
        None => None,
    };

    client.create_model(
        namespace,
        project,
        CreateModelRequest {
            name: model_name.to_string(),
            description,
        },
    )?;

    context
        .terminal()
        .step(&format!("Created model '{model_name}'"));

    Ok(true)
}
