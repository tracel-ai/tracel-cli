use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use serde::Serialize;
use tracel_client::console::Client;
use tracel_client::console::project::request::{
    PublishArtifactRequest, PublishBinaryRequest, PublishProjectVersionRequest,
    PublishSourceRequest,
};

use super::{Mode, PackageArgs, check_targets_allowed};
use crate::context::CliContext;
use crate::tools::build_driver::{self, BuildDriver};
use crate::tools::fs::file_sha256_and_size;
use crate::tools::packager::{self, PackageEvent};
use crate::tools::project_context::ProjectContext;
use crate::tools::{cargo, target};
use crate::ui::Render;

/// A workspace packaged for upload: the code version digest of its content, the publish
/// request describing it, plus the `(upload-url key, file path)` pairs whose bytes must be
/// PUT to the presigned URLs the server returns.
pub struct BuiltPackage {
    pub mode: Mode,
    pub digest: String,
    targets: Vec<String>,
    request: PublishArtifactRequest,
    uploads: Vec<(String, PathBuf)>,
}

/// A code version of the project, as `tracel package` reports it.
#[derive(Serialize)]
pub struct PackagedCodeVersion {
    pub namespace: String,
    pub project: String,
    pub digest: String,
    pub version_id: String,
    pub mode: Mode,
    pub targets: Vec<String>,
    /// False when the same content was already packaged.
    pub uploaded: bool,
}

impl Render for PackagedCodeVersion {}

/// Build the workspace in the mode from `args`, asking for one when it is missing.
pub fn build_package(
    context: &CliContext,
    project: &ProjectContext,
    args: &PackageArgs,
) -> anyhow::Result<BuiltPackage> {
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

    match mode {
        Mode::Source => build_source_package(context, project),
        Mode::Binary => build_binary_package(context, project, args),
    }
}

fn build_source_package(
    context: &CliContext,
    project: &ProjectContext,
) -> anyhow::Result<BuiltPackage> {
    let spinner = context.terminal().spinner();
    spinner.start("Packaging workspace...");
    let spinner_clone = spinner.clone();
    let result = packager::package_workspace(
        &project.workspace_info,
        Arc::new(move |msg: PackageEvent| {
            spinner_clone.set_message(msg.message);
        }),
    )
    .map_err(|e| {
        spinner.error("Packaging failed.");
        e.context("Failed to package workspace")
    })?;
    spinner.stop("Workspace packaged.");

    Ok(BuiltPackage {
        mode: Mode::Source,
        digest: result.digest,
        targets: Vec::new(),
        request: PublishArtifactRequest::Source {
            source: PublishSourceRequest {
                checksum: result.checksum,
                size: result.size,
            },
        },
        uploads: vec![("source.zip".to_string(), result.path)],
    })
}

fn build_binary_package(
    context: &CliContext,
    project: &ProjectContext,
    args: &PackageArgs,
) -> anyhow::Result<BuiltPackage> {
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

    // Only the binaries are uploaded, but the code version is the source they are built from.
    let digest = packager::workspace_digest(&project.workspace_info)?;

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

    Ok(BuiltPackage {
        mode: Mode::Binary,
        digest,
        targets: selected
            .iter()
            .map(|&(os, arch)| target::target_triple(os, arch).to_string())
            .collect(),
        request: PublishArtifactRequest::Binaries { binaries },
        uploads,
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
    command.arg("--release");
    if let Some(triple) = target {
        command.arg("--target").arg(triple);
        if let Some(linker) = linker {
            command
                .arg("--config")
                .arg(format!("target.{triple}.linker=\"{linker}\""));
        }
    }
    let executables = cargo::build_executables(command, &cmd_label)?;

    let names: Vec<&str> = executables.iter().map(|(name, _)| name.as_str()).collect();
    if let Some(bin) = bin {
        return executables
            .iter()
            .find(|(name, _)| name == bin)
            .map(|(_, path)| path.clone())
            .ok_or_else(|| cargo::invalid_bin(bin, &names).into());
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

impl BuiltPackage {
    /// Upload the package as a code version of the project, unless the project already
    /// has a code version with this digest.
    pub fn publish(
        self,
        context: &CliContext,
        client: &Client,
        project: &ProjectContext,
    ) -> anyhow::Result<PackagedCodeVersion> {
        let bc_project = project.get_project();

        let response = client
            .publish_project_version_urls(
                &bc_project.owner,
                &bc_project.name,
                PublishProjectVersionRequest {
                    digest: self.digest.clone(),
                    artifact: self.request,
                },
            )
            .with_context(|| {
                format!(
                    "Failed to request upload URLs for {}/{}",
                    bc_project.owner, bc_project.name
                )
            })?;

        let mut version = PackagedCodeVersion {
            namespace: bc_project.owner.clone(),
            project: bc_project.name.clone(),
            digest: self.digest,
            version_id: response.id,
            mode: self.mode,
            targets: self.targets,
            uploaded: false,
        };
        let Some(urls) = response.urls else {
            context.terminal().print_success(&format!(
                "This code is already packaged as code version {} ({}).",
                version.digest, version.version_id
            ));
            return Ok(version);
        };

        let spinner = context.terminal().spinner();
        spinner.start("Uploading artifacts...");
        for (key, path) in self.uploads {
            let url = urls.get(&key).ok_or_else(|| {
                spinner.error("Upload failed.");
                anyhow::anyhow!("Server did not return an upload URL for `{key}`")
            })?;
            let bytes = std::fs::read(&path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            client.upload_bytes_to_url(url, bytes).map_err(|e| {
                spinner.error("Upload failed.");
                anyhow::Error::new(e).context(format!("Failed to upload `{key}`"))
            })?;
        }
        spinner.stop("Artifacts uploaded.");

        client
            .complete_project_version_upload(
                &bc_project.owner,
                &bc_project.name,
                &version.version_id,
            )
            .with_context(|| {
                format!(
                    "Failed to finalize upload for {}/{}",
                    bc_project.owner, bc_project.name
                )
            })?;

        context
            .terminal()
            .print_success(&format!("New code version uploaded: {}", response.digest));
        version.uploaded = true;
        Ok(version)
    }
}
