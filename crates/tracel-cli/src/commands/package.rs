use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::Context;
use clap::{Args, ValueEnum};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracel_client::console::Client;
use tracel_client::console::project::request::{
    PublishArtifactRequest, PublishBinaryRequest, PublishProjectVersionRequest,
    PublishSourceRequest,
};

use crate::commands::init::commit_sequence;
use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::{
    require_cargo_workspace, resolve_namespace_project, validate_project_exists_on_server,
};
use crate::tools::build_driver::{self, BuildDriver};
use crate::tools::packager::{PackageEvent, package_workspace};
use crate::tools::project_context::ProjectContext;
use crate::tools::{cargo, git, target};

#[derive(Args, Debug)]
pub struct PackageArgs {
    /// Package even if the git repository has uncommitted changes (skips the commit prompt).
    #[arg(long, action)]
    pub allow_dirty: bool,
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
    /// Commit all current changes before packaging
    #[arg(long, conflicts_with = "allow_dirty")]
    pub commit: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Binary,
    Source,
}

/// An artifact prepared for upload: the publish request describing it, plus the
/// `(upload-url key, file path)` pairs whose bytes must be PUT to the presigned
/// URLs the server returns.
struct PreparedArtifact {
    request: PublishArtifactRequest,
    uploads: Vec<(String, PathBuf)>,
    targets: Vec<String>,
}

pub fn handle_command(args: PackageArgs, context: CliContext) -> anyhow::Result<Value> {
    if args.mode.is_none() && !context.terminal().is_interactive() {
        return Err(CliError::new(
            ErrorKind::Usage,
            "Missing --mode. Valid values: binary, source.",
        )
        .with_hint("Pass --mode binary or --mode source.")
        .into());
    }
    if args.mode == Some(Mode::Source) && !args.targets.is_empty() {
        return Err(CliError::new(
            ErrorKind::Usage,
            "--target is only valid with --mode binary.",
        )
        .into());
    }
    for triple in &args.targets {
        target::parse_target(triple)?;
    }
    context.terminal().command_title("Package project");

    // 0. Require a workspace and a project that exists on the server.
    let workspace_info = require_cargo_workspace()?;
    let resolved = resolve_namespace_project(&context)?;
    let project = ProjectContext {
        workspace_info,
        build_profile: "release".to_string(),
        project: resolved.project,
    };
    let client = get_client_and_login_if_needed(&context)?;
    validate_project_exists_on_server(&context, &project, &client)?;

    // 1. Dirty check — warn and offer to commit, but allow proceeding.
    let has_commit = git::get_last_commit_hash().is_ok();
    if !has_commit && !args.commit && !context.terminal().is_interactive() {
        return Err(CliError::new(
            ErrorKind::Usage,
            "The repository needs at least one commit to package.",
        )
        .with_hint("Commit your changes, or pass --commit to create the first commit.")
        .into());
    }
    if args.commit {
        if !has_commit || git::is_repo_dirty()? {
            commit_sequence(context.terminal(), true)?;
        }
    } else if git::is_repo_dirty()? && !args.allow_dirty {
        context
            .terminal()
            .print_warning("Your repository has uncommitted changes.");
        if !context.terminal().is_interactive() {
            return Err(
                CliError::new(ErrorKind::Usage, "The repository has uncommitted changes.")
                    .with_hint("Commit your changes, or pass --commit or --allow-dirty")
                    .into(),
            );
        }
        if context
            .terminal()
            .confirm("Commit changes before packaging?", "commit", true)?
        {
            commit_sequence(context.terminal(), false)?;
        }
    }

    // 2. The code version is identified by the current commit hash.
    let digest = git::get_last_commit_hash().context(
        "Failed to read the current git commit. The repository needs at least one commit to package.",
    )?;
    if git::is_repo_dirty()? {
        context.terminal().print_warning(&format!(
            "Proceeding with uncommitted changes — they will not be part of code version {digest}."
        ));
    }

    // 3. Choose how to package.
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
    if mode == Mode::Source && !args.targets.is_empty() {
        return Err(CliError::new(
            ErrorKind::Usage,
            "--target is only valid with --mode binary.",
        )
        .into());
    }

    let artifact = match mode {
        Mode::Source => build_source_artifact(&context, &project)?,
        Mode::Binary => build_binary_artifact(&context, &project, &args)?,
    };

    // 4. Upload.
    upload(&context, &client, &project, &digest, mode, artifact)
}

fn build_source_artifact(
    context: &CliContext,
    project: &ProjectContext,
) -> anyhow::Result<PreparedArtifact> {
    let spinner = context.terminal().spinner();
    spinner.start("Packaging workspace...");
    let spinner_clone = spinner.clone();
    let result = package_workspace(
        project.get_workspace_name(),
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
        let (checksum, size) = sha256_and_size(&path)?;
        binaries.push(PublishBinaryRequest {
            os,
            architecture: arch,
            checksum,
            size,
        });
        uploads.push((triple.to_string(), path));
    }

    Ok(PreparedArtifact {
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

fn sha256_and_size(path: &Path) -> anyhow::Result<(String, u64)> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("Failed to read binary at {}", path.display()))?;
    let checksum = format!("{:x}", Sha256::digest(&bytes));
    Ok((checksum, bytes.len() as u64))
}

fn upload(
    context: &CliContext,
    client: &Client,
    project: &ProjectContext,
    digest: &str,
    mode: Mode,
    prepared: PreparedArtifact,
) -> anyhow::Result<Value> {
    let bc_project = project.get_project();

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

    let data = |uploaded| {
        json!({
            "namespace": bc_project.owner,
            "project": bc_project.name,
            "digest": digest,
            "version_id": response.id,
            "mode": match mode { Mode::Binary => "binary", Mode::Source => "source" },
            "targets": prepared.targets,
            "uploaded": uploaded,
        })
    };
    let Some(urls) = response.urls else {
        context.terminal().print_success(&format!(
            "This commit ({digest}) is already packaged (version {}).",
            response.id
        ));
        context.terminal().finalize("Nothing to upload.");
        return Ok(data(false));
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
    Ok(data(true))
}
