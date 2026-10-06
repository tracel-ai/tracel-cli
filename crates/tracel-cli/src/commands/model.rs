use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tracel_client::console::model::request::{
    ModelFileSpecRequest, RequestModelVersionUploadRequest,
};

use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::{
    build_part_tasks, ensure_model_exists, resolve_namespace_project, upload_parts,
};
use crate::tools::fs::{build_file_specs, collect_files};

#[derive(Args, Debug)]
pub struct ModelArgs {
    #[command(subcommand)]
    pub command: ModelCommands,
}

#[derive(Subcommand, Debug)]
pub enum ModelCommands {
    /// Upload a local directory of files as a new model version.
    Upload(UploadModelArgs),
}

#[derive(Args, Debug)]
pub struct UploadModelArgs {
    /// Name of the model to upload a version to.
    pub model_name: String,
    /// Local directory containing the files to upload.
    #[arg(short, long)]
    pub directory: PathBuf,
    /// Create a missing model (true/false); otherwise ask when interactive.
    #[arg(long, short)]
    pub auto_create: Option<bool>,
    /// Description to use when auto-creating the model. Requires --auto-create true.
    #[arg(long, requires = "auto_create")]
    pub description: Option<String>,
}

pub fn handle_command(args: ModelArgs, context: CliContext) -> anyhow::Result<Value> {
    match args.command {
        ModelCommands::Upload(upload_args) => upload_model_version(upload_args, context),
    }
}

fn upload_model_version(args: UploadModelArgs, context: CliContext) -> anyhow::Result<Value> {
    if args.description.is_some() && args.auto_create != Some(true) {
        return Err(CliError::new(
            ErrorKind::Usage,
            "--description can only be used together with --auto-create true.",
        )
        .into());
    }

    context.terminal().command_title("Model upload");

    let resolved = resolve_namespace_project(&context)?;
    let namespace = resolved.project.owner;
    let project = resolved.project.name;
    let client = crate::commands::login::get_client_and_login_if_needed(&context)?;

    context
        .terminal()
        .print(&format!("Uploading to {namespace}/{project}"));

    let spinner = context.terminal().spinner();
    spinner.start("Collecting files...");
    let files = collect_files(&args.directory).inspect_err(|_e| {
        spinner.error("Failed to collect files.");
    })?;
    spinner.stop(format!("Found {} file(s).", files.len()));

    ensure_model_exists(
        &context,
        &client,
        &namespace,
        &project,
        &args.model_name,
        args.auto_create,
        args.description.clone(),
    )?;

    let spinner = context.terminal().spinner();
    spinner.start("Computing checksums...");
    let file_specs = build_file_specs(&files).inspect_err(|_e| {
        spinner.error("Failed to compute checksums.");
    })?;
    spinner.stop("Checksums computed.");

    let file_sizes: std::collections::BTreeMap<String, u64> = file_specs
        .iter()
        .map(|f| (f.rel_path.clone(), f.size_bytes))
        .collect();
    let bytes: u64 = file_sizes.values().sum();

    let spinner = context.terminal().spinner();
    spinner.start("Requesting upload URLs...");
    let upload_request = RequestModelVersionUploadRequest {
        files: file_specs
            .into_iter()
            .map(|f| ModelFileSpecRequest {
                rel_path: f.rel_path,
                size_bytes: f.size_bytes,
                checksum: f.checksum,
            })
            .collect(),
        metadata: None,
    };
    let upload = client
        .request_model_version_upload(&namespace, &project, &args.model_name, upload_request)
        .map_err(|e| {
            spinner.error("Failed to request upload URLs.");
            anyhow::anyhow!(e)
        })?;
    spinner.stop(format!("Allocated model version {}.", upload.version));

    let tasks = build_part_tasks(&files, &file_sizes, &upload.files)?;
    upload_parts(&client, tasks, context.terminal())?;

    client.complete_model_version_upload(&namespace, &project, &args.model_name, upload.version)?;

    context.terminal().print_success(&format!(
        "Uploaded model '{}' version {} to {}/{}.",
        args.model_name, upload.version, namespace, project
    ));
    context
        .terminal()
        .finalize("Model version uploaded successfully.");

    Ok(json!({
        "namespace": namespace,
        "project": project,
        "model": args.model_name,
        "version": upload.version,
        "files": files.len(),
        "bytes": bytes,
    }))
}
