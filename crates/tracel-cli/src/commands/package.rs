use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::Context;
use clap::{Args, ValueEnum};
use serde::Serialize;
use tracel_client::console::Client;
use tracel_client::console::project::request::{
    PublishArtifactRequest, PublishBinaryRequest, PublishProjectVersionRequest,
    PublishSourceRequest,
};

use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::{
    require_cargo_workspace, resolve_namespace_project, validate_project_exists_on_server,
};
use crate::tools::build_driver::{self, BuildDriver};
use crate::tools::fs::{file_sha256_and_size, manifest_digest};
use crate::tools::packager::{PackageEvent, package_workspace};
use crate::tools::project_context::ProjectContext;
use crate::tools::{cargo, target};
use crate::ui::{Outcome, Render};

#[derive(Args, Debug)]
pub struct PackageArgs {
    /// Package a compiled binary or source (required without prompts)
    #[arg(long, value_enum)]
    pub mode: Option<Mode>,
    /// Rust target triple to build (repeatable; binary mode only)
    #[arg(long = "target", value_name = "TRIPLE")]
    pub targets: Vec<String>,
    /// Name of the binary to upload when several are built
    #[arg(long, value_name = "NAME")]
    pub bin: Option<String>,
    /// Install missing Rust targets without asking
    #[arg(long)]
    pub install_targets: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Binary,
    Source,
}

/// An artifact prepared for upload: the code version digest of its content, the publish
/// request describing it, plus the `(upload-url key, file path)` pairs whose bytes must be
/// PUT to the presigned URLs the server returns.
struct PreparedArtifact {
    digest: String,
    request: PublishArtifactRequest,
    uploads: Vec<(String, PathBuf)>,
    targets: Vec<String>,
}

/// A code version of the project. `uploaded` is false when the same content was already
/// packaged.
#[derive(Serialize)]
struct Packaged {
    namespace: String,
    project: String,
    digest: String,
    version_id: String,
    mode: Mode,
    targets: Vec<String>,
    uploaded: bool,
}

impl Render for Packaged {}

pub fn handle_command(args: PackageArgs, context: CliContext) -> anyhow::Result<Outcome> {
    if args.mode.is_none() && !context.terminal().is_interactive() {
        return Err(CliError::new(
            ErrorKind::Usage,
            "Missing --mode. Valid values: binary, source.",
        )
        .with_hint("Pass --mode binary or --mode source.")
        .into());
    }
    if let Some(mode) = args.mode {
        check_targets_allowed(mode, &args.targets)?;
    }
    for triple in &args.targets {
        target::parse_target(triple)?;
    }
    context.terminal().command_title("Package project");

    // 1. Require a workspace and a project that exists on the server.
    let workspace_info = require_cargo_workspace()?;
    let resolved = resolve_namespace_project(&context)?;
    let project = ProjectContext {
        workspace_info,
        project: resolved.project,
    };
    let client = get_client_and_login_if_needed(&context)?;
    validate_project_exists_on_server(&project, &client)?;

    // 2. Choose how to package.
    let mode = match args.mode {
        Some(mode) => mode,
        None => context.terminal().select(
            "How would you like to package your code?",
            "mode",
            &[
                (
                    Mode::Binary,
                    "Binary (more secure)",
                    "ship a compiled binary; your source is not uploaded",
                ),
                (
                    Mode::Source,
                    "Source (more portable)",
                    "upload source; it is built on the compute provider",
                ),
            ],
            None,
            &["binary", "source"],
        )?,
    };
    check_targets_allowed(mode, &args.targets)?;

    let artifact = match mode {
        Mode::Source => build_source_artifact(&context, &project)?,
        Mode::Binary => build_binary_artifact(&context, &project, &args)?,
    };

    // 3. Upload.
    Ok(upload(&context, &client, &project, mode, artifact)?.into())
}

fn check_targets_allowed(mode: Mode, targets: &[String]) -> Result<(), CliError> {
    if mode == Mode::Source && !targets.is_empty() {
        return Err(CliError::new(
            ErrorKind::Usage,
            "--target is only valid with --mode binary.",
        ));
    }
    Ok(())
}

fn build_source_artifact(
    context: &CliContext,
    project: &ProjectContext,
) -> anyhow::Result<PreparedArtifact> {
    let spinner = context.terminal().spinner();
    spinner.start("Packaging workspace...");
    let spinner_clone = spinner.clone();
    let result = package_workspace(
        &project.workspace_info,
        Arc::new(move |msg: PackageEvent| {
            spinner_clone.set_message(msg.message);
        }),
    )
    .map_err(|e| {
        spinner.error("Packaging failed.");
        anyhow::anyhow!("Failed to package workspace: {e}")
    })?;
    spinner.stop("Workspace packaged.");

    Ok(PreparedArtifact {
        digest: result.digest,
        request: PublishArtifactRequest::Source {
            source: PublishSourceRequest {
                checksum: result.checksum,
                size: result.size,
            },
        },
        uploads: vec![("source.zip".to_string(), result.path)],
        targets: Vec::new(),
    })
}

fn build_binary_artifact(
    context: &CliContext,
    project: &ProjectContext,
    args: &PackageArgs,
) -> anyhow::Result<PreparedArtifact> {
    let host = target::host_target()?;
    let installed = target::installed_targets();

    let selected = target::select_targets(context.terminal(), &args.targets, host, &installed)?;

    // rustup preflight: offer to install any selected cross target whose std is missing.
    let missing: Vec<&str> = selected
        .iter()
        .filter(|&&(os, arch)| (os, arch) != host)
        .map(|&(os, arch)| target::target_triple(os, arch))
        .filter(|triple| !installed.contains(*triple))
        .collect();
    target::install_missing_target(context.terminal(), missing, args.install_targets)?;

    let root = project.get_workspace_root();
    let drivers = build_driver::detect();
    let mut binaries = Vec::new();
    let mut uploads = Vec::new();

    for &(os, arch) in &selected {
        let triple = target::target_triple(os, arch);
        let is_host = (os, arch) == host;
        let driver = if is_host {
            BuildDriver::Cargo
        } else {
            build_driver::choose(host, (os, arch), &drivers)
        };

        let linker = if is_host {
            context.terminal().print_warning(&format!(
                "Building for this machine ({triple}). It will only run on compute providers with the same OS and architecture."
            ));
            None
        } else {
            build_driver::cross_preflight(context.terminal(), root, host, (os, arch), driver)?
        };

        let path = build_release_binary(
            context,
            (!is_host).then_some(triple),
            driver,
            linker,
            args.bin.as_deref(),
        )?;
        let (checksum, size) = file_sha256_and_size(&path)?;
        binaries.push(PublishBinaryRequest {
            os,
            architecture: arch,
            checksum,
            size,
        });
        uploads.push((triple.to_string(), path));
    }

    // The code version digest covers each uploaded binary and the target it is uploaded for.
    let digest = manifest_digest(binaries.iter().map(|binary| {
        (
            target::target_triple(binary.os, binary.architecture),
            binary.checksum.as_str(),
        )
    }));

    Ok(PreparedArtifact {
        digest,
        request: PublishArtifactRequest::Binaries { binaries },
        uploads,
        targets: selected
            .iter()
            .map(|&(os, arch)| target::target_triple(os, arch).to_string())
            .collect(),
    })
}

/// Run the release build with `driver` (optionally for a cross `--target`) and return
/// the path to the produced executable (prompting if the build produced more than one).
fn build_release_binary(
    context: &CliContext,
    target: Option<&str>,
    driver: BuildDriver,
    linker: Option<&str>,
    bin: Option<&str>,
) -> anyhow::Result<PathBuf> {
    let mut cmd_label = match target {
        Some(triple) => format!("{} --release --target {triple}", driver.label()),
        None => format!("{} --release", driver.label()),
    };
    if let Some(linker) = linker {
        cmd_label.push_str(&format!(" (linker {linker})"));
    }
    context
        .terminal()
        .print(&format!("Building release binary ({cmd_label})..."));

    let mut command = cargo::command();
    for arg in driver.subcommand_args() {
        command.arg(arg);
    }
    command.arg("--release").arg("--message-format=json");
    if let Some(triple) = target {
        command.arg("--target").arg(triple);
        if let Some(linker) = linker {
            command
                .arg("--config")
                .arg(format!("target.{triple}.linker=\"{linker}\""));
        }
    }
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("Failed to run `{cmd_label}`"))?;

    if !output.status.success() {
        anyhow::bail!("`{cmd_label}` failed");
    }

    let mut executables: Vec<(String, PathBuf)> = Vec::new();
    for line in output.stdout.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(msg) = serde_json::from_slice::<serde_json::Value>(line) {
            if msg.get("reason").and_then(|r| r.as_str()) == Some("compiler-artifact") {
                if let Some(exe) = msg.get("executable").and_then(|e| e.as_str()) {
                    let name = msg["target"]["name"].as_str().ok_or_else(|| {
                        anyhow::anyhow!("Cargo did not return a name for binary {exe}")
                    })?;
                    executables.push((name.to_string(), PathBuf::from(exe)));
                }
            }
        }
    }

    let names: Vec<&str> = executables.iter().map(|(name, _)| name.as_str()).collect();
    if let Some(bin) = bin {
        return executables
            .iter()
            .find(|(name, _)| name == bin)
            .map(|(_, path)| path.clone())
            .ok_or_else(|| {
                CliError::new(
                    ErrorKind::Usage,
                    format!("Invalid --bin '{bin}'. Valid values: {}.", names.join(", ")),
                )
                .with_hint("Pass --bin <name> using one of the valid values.")
                .into()
            });
    }
    match executables.len() {
        0 => anyhow::bail!("The build did not produce any binary target."),
        1 => Ok(executables[0].1.clone()),
        _ => {
            let items: Vec<(PathBuf, String, &str)> = executables
                .iter()
                .map(|(_, path)| {
                    (
                        path.clone(),
                        path.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string()),
                        "",
                    )
                })
                .collect();
            context.terminal().select(
                "Multiple binaries were built. Select which to upload",
                "bin",
                &items,
                None,
                &names,
            )
        }
    }
}

fn upload(
    context: &CliContext,
    client: &Client,
    project: &ProjectContext,
    mode: Mode,
    prepared: PreparedArtifact,
) -> anyhow::Result<Packaged> {
    let bc_project = project.get_project();
    let digest = prepared.digest.as_str();

    let response = client
        .publish_project_version_urls(
            &bc_project.owner,
            &bc_project.name,
            PublishProjectVersionRequest {
                digest: digest.to_string(),
                artifact: prepared.request,
            },
        )
        .with_context(|| {
            format!(
                "Failed to request upload URLs for {}/{}",
                bc_project.owner, bc_project.name
            )
        })?;

    let packaged = |uploaded| Packaged {
        namespace: bc_project.owner.clone(),
        project: bc_project.name.clone(),
        digest: digest.to_string(),
        version_id: response.id.clone(),
        mode,
        targets: prepared.targets.clone(),
        uploaded,
    };
    let Some(urls) = response.urls else {
        context.terminal().print_success(&format!(
            "This code is already packaged as code version {digest} ({}).",
            response.id
        ));
        context.terminal().finalize("Nothing to upload.");
        return Ok(packaged(false));
    };

    let spinner = context.terminal().spinner();
    spinner.start("Uploading artifacts...");
    for (key, path) in prepared.uploads {
        let url = urls.get(&key).ok_or_else(|| {
            spinner.error("Upload failed.");
            anyhow::anyhow!("Server did not return an upload URL for `{key}`")
        })?;
        let bytes =
            std::fs::read(&path).with_context(|| format!("Failed to read {}", path.display()))?;
        client.upload_bytes_to_url(url, bytes).map_err(|e| {
            spinner.error("Upload failed.");
            anyhow::Error::new(e).context(format!("Failed to upload `{key}`"))
        })?;
    }
    spinner.stop("Artifacts uploaded.");

    client
        .complete_project_version_upload(&bc_project.owner, &bc_project.name, &response.id)
        .with_context(|| {
            format!(
                "Failed to finalize upload for {}/{}",
                bc_project.owner, bc_project.name
            )
        })?;

    context
        .terminal()
        .print_success(&format!("New code version uploaded: {}", response.digest));
    context
        .terminal()
        .finalize("Project packaged successfully.");
    Ok(packaged(true))
}
