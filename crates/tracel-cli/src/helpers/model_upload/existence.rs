use anyhow::Context;
use tracel_client::console::Client;
use tracel_client::console::model::request::CreateModelRequest;

use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};

pub fn ensure_model_exists(
    context: &CliContext,
    client: &Client,
    namespace: &str,
    project: &str,
    model_name: &str,
    auto_create: Option<bool>,
    description: Option<String>,
) -> anyhow::Result<()> {
    match client.get_model(namespace, project, model_name) {
        Ok(_) => return Ok(()),
        Err(e) if e.is_not_found() => {}
        Err(e) => return Err(e).with_context(|| format!("Failed to check model '{model_name}'")),
    }

    if !context.terminal().is_interactive() && auto_create != Some(true) {
        return Err(CliError::new(
            ErrorKind::Usage,
            format!("Model '{model_name}' does not exist in {namespace}/{project}."),
        )
        .with_hint("Pass --auto-create true to create it")
        .into());
    }

    let create = match auto_create {
        Some(create) => create,
        None => context.terminal().confirm(
            &format!(
                "Model '{model_name}' does not exist in {namespace}/{project}. Create it now?"
            ),
            "auto-create",
            false,
        )?,
    };

    if !create {
        anyhow::bail!("Model upload cancelled: model '{model_name}' does not exist.");
    }

    let description = match description {
        Some(description) => Some(description),
        None if auto_create.is_none() => {
            let description = context
                .terminal()
                .input("Enter model description (optional)", "description")?;
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

    context.terminal().print_success(&format!(
        "Created model '{model_name}' in {namespace}/{project}."
    ));

    Ok(())
}
