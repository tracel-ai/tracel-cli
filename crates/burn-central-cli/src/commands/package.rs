use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::Context;
use burn_central_client::Client;
use burn_central_client::request::{
    PublishArtifactRequest, PublishBinaryRequest, PublishProjectVersionRequest,
    PublishSourceRequest,
};
use burn_central_workspace::ProjectContext;
use burn_central_workspace::tools::cargo;
use burn_central_workspace::tools::cargo::package::{PackageEvent, package_workspace};
use burn_central_workspace::tools::git;
use clap::Args;
use sha2::{Digest, Sha256};

use crate::commands::init::commit_sequence;
use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::helpers::{require_linked_project, validate_project_exists_on_server};
use crate::tools::target;

#[derive(Args, Debug)]
pub struct PackageArgs {
    /// Package even if the git repository has uncommitted changes (skips the commit prompt).
    #[arg(long, action)]
    pub allow_dirty: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Binary,
    Source,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BinarySource {
    BuildFromCode,
    ProvidePaths,
}

/// An artifact prepared for upload: the publish request describing it, plus the
/// `(upload-url key, file path)` pairs whose bytes must be PUT to the presigned
/// URLs the server returns.
struct PreparedArtifact {
    request: PublishArtifactRequest,
    uploads: Vec<(String, PathBuf)>,
}

pub(crate) fn handle_command(args: PackageArgs, mut context: CliContext) -> anyhow::Result<()> {
    context.terminal().command_title("Package project");

    // 0. Ensure we have auth and a linked project that exists on the server.
    let client = get_client_and_login_if_needed(&mut context)?;
    let project = require_linked_project(&context)?;
    validate_project_exists_on_server(&context, &project, &client)?;

    // 1. Dirty check — warn and offer to commit, but allow proceeding.
    if git::is_repo_dirty()? && !args.allow_dirty {
        context
            .terminal()
            .print_warning("Your repository has uncommitted changes.");
        if cliclack::confirm("Commit changes before packaging?")
            .initial_value(true)
            .interact()?
        {
            commit_sequence()?;
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
    let mode = cliclack::select("How would you like to package your code?")
        .items(&[
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
        ])
        .interact()?;

    let artifact = match mode {
        Mode::Source => build_source_artifact(&context, &project)?,
        Mode::Binary => build_binary_artifact(&context)?,
    };

    // 4. Upload.
    upload(&context, &client, &project, &digest, artifact)
}

fn build_source_artifact(
    context: &CliContext,
    project: &ProjectContext,
) -> anyhow::Result<PreparedArtifact> {
    let spinner = context.terminal().spinner();
    spinner.start("Packaging workspace...");
    let spinner_clone = spinner.clone();
    let result = package_workspace(
        &project.burn_dir().artifacts_dir(),
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

    let archive = result
        .crate_metadata
        .into_iter()
        .next()
        .context("Packaging produced no archive")?;

    Ok(PreparedArtifact {
        request: PublishArtifactRequest::Source {
            source: PublishSourceRequest {
                checksum: archive.checksum,
                size: archive.size,
            },
        },
        // The server keys the source blob `source.zip` regardless of the actual
        // archive format (a `{name}-{version}/`-prefixed tar.gz).
        uploads: vec![("source.zip".to_string(), archive.path)],
    })
}

fn build_binary_artifact(context: &CliContext) -> anyhow::Result<PreparedArtifact> {
    let source = cliclack::select("Where should the binary come from?")
        .items(&[
            (
                BinarySource::BuildFromCode,
                "Build from code",
                "compile now for THIS machine's OS/architecture only",
            ),
            (
                BinarySource::ProvidePaths,
                "Provide path(s)",
                "use prebuilt binaries (e.g. cross-compiled for several targets)",
            ),
        ])
        .interact()?;

    let mut binaries = Vec::new();
    let mut uploads = Vec::new();

    match source {
        BinarySource::BuildFromCode => {
            let (os, arch) = target::host_target()?;
            context.terminal().print_warning(&format!(
                "Building for this machine ({}). It will only run on compute providers with the same OS and architecture.",
                target::target_triple(os, arch)
            ));
            let path = build_release_binary(context)?;
            let (checksum, size) = sha256_and_size(&path)?;
            binaries.push(PublishBinaryRequest {
                os,
                architecture: arch,
                checksum,
                size,
            });
            uploads.push((target::target_triple(os, arch).to_string(), path));
        }
        BinarySource::ProvidePaths => loop {
            let path: PathBuf = cliclack::input("Path to the binary")
                .validate(|s: &String| {
                    if Path::new(s.trim()).is_file() {
                        Ok(())
                    } else {
                        Err("No file exists at that path".to_string())
                    }
                })
                .interact::<String>()?
                .trim()
                .into();
            let os = target::prompt_os()?;
            let arch = target::prompt_arch()?;
            let (checksum, size) = sha256_and_size(&path)?;
            binaries.push(PublishBinaryRequest {
                os,
                architecture: arch,
                checksum,
                size,
            });
            uploads.push((target::target_triple(os, arch).to_string(), path));

            if !cliclack::confirm("Add another binary for a different target?")
                .initial_value(false)
                .interact()?
            {
                break;
            }
        },
    }

    Ok(PreparedArtifact {
        request: PublishArtifactRequest::Binaries { binaries },
        uploads,
    })
}

/// Run `cargo build --release` and return the path to the produced executable
/// (prompting if the build produced more than one).
fn build_release_binary(context: &CliContext) -> anyhow::Result<PathBuf> {
    context
        .terminal()
        .print("Building release binary (cargo build --release)...");

    let output = cargo::command()
        .arg("build")
        .arg("--release")
        .arg("--message-format=json")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .context("Failed to run `cargo build --release`")?;

    if !output.status.success() {
        anyhow::bail!("`cargo build --release` failed");
    }

    let mut executables: Vec<PathBuf> = Vec::new();
    for line in output.stdout.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(msg) = serde_json::from_slice::<serde_json::Value>(line) {
            if msg.get("reason").and_then(|r| r.as_str()) == Some("compiler-artifact") {
                if let Some(exe) = msg.get("executable").and_then(|e| e.as_str()) {
                    executables.push(PathBuf::from(exe));
                }
            }
        }
    }

    match executables.len() {
        0 => anyhow::bail!("The build did not produce any binary target."),
        1 => Ok(executables.into_iter().next().unwrap()),
        _ => {
            let items: Vec<(PathBuf, String, &str)> = executables
                .iter()
                .map(|p| {
                    (
                        p.clone(),
                        p.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| p.display().to_string()),
                        "",
                    )
                })
                .collect();
            cliclack::select("Multiple binaries were built. Select which to upload")
                .items(&items)
                .interact()
                .map_err(anyhow::Error::from)
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
    prepared: PreparedArtifact,
) -> anyhow::Result<()> {
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

    let Some(urls) = response.urls else {
        context.terminal().print_success(&format!(
            "This commit ({digest}) is already packaged (version {}).",
            response.id
        ));
        context.terminal().finalize("Nothing to upload.");
        return Ok(());
    };

    let spinner = context.terminal().spinner();
    spinner.start("Uploading artifacts...");
    for (key, path) in prepared.uploads {
        let url = urls.get(&key).ok_or_else(|| {
            spinner.error("Upload failed.");
            anyhow::anyhow!("Server did not return an upload URL for `{key}`")
        })?;
        let bytes = std::fs::read(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        client.upload_bytes_to_url(url, bytes).map_err(|e| {
            spinner.error("Upload failed.");
            anyhow::anyhow!("Failed to upload `{key}`: {e}")
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
    context.terminal().finalize("Project packaged successfully.");
    Ok(())
}
