use std::collections::HashMap;
use std::path::PathBuf;
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
use crate::ui::{Outcome, Unanswered, short_digest};

#[derive(Args, Debug)]
pub struct PackageArgs {
    /// Package a compiled binary or the source
    #[arg(long, value_enum)]
    pub mode: Option<Mode>,
    /// Build for this target triple; repeat for more
    #[arg(long = "target", value_name = "TRIPLE")]
    pub targets: Vec<String>,
    /// Upload this binary when several are built
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

/// A code version ready to upload: the digest of its content, its publish request, each
/// file by the key of its upload URL, and the targets it was built for.
struct PreparedCodeVersion {
    digest: String,
    request: PublishArtifactRequest,
    uploads: Vec<(String, PathBuf)>,
    targets: Vec<String>,
}

/// The code version `package` made. `uploaded` is false when the same content was
/// already packaged.
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

pub fn handle_command(args: PackageArgs, context: CliContext) -> anyhow::Result<Outcome> {
    if args.mode.is_none() && !context.terminal().is_interactive() {
        return Err(CliError::new(ErrorKind::Usage, MODE_NEEDED)
            .with_hint(MODE_HINT)
            .into());
    }
    if let Some(mode) = args.mode {
        check_targets_allowed(mode, &args.targets)?;
    }
    for triple in &args.targets {
        target::parse_target(triple)?;
    }

    let workspace_info = require_cargo_workspace()?;
    let resolved = resolve_namespace_project(&context)?;
    let client = get_client_and_login_if_needed(&context)?;
    validate_project_exists_on_server(&resolved.project, &resolved.source, &client)?;
    let project = ProjectContext {
        workspace_info,
        project: resolved.project,
    };

    let mode = match args.mode {
        Some(mode) => mode,
        None => context.terminal().select(
            "Mode",
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
            Unanswered::new(MODE_NEEDED, MODE_HINT),
        )?,
    };
    check_targets_allowed(mode, &args.targets)?;

    let code_version = match mode {
        Mode::Source => prepare_source(&context, &project)?,
        Mode::Binary => prepare_binaries(&context, &project, &args)?,
    };

    Ok(Outcome::data(upload(
        &context,
        &client,
        &project,
        mode,
        code_version,
    )?))
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

fn prepare_source(
    context: &CliContext,
    project: &ProjectContext,
) -> anyhow::Result<PreparedCodeVersion> {
    let spinner = context.terminal().spinner("Packaging the workspace");
    let reporter = spinner.clone();
    let result = package_workspace(
        &project.workspace_info,
        Arc::new(move |event: PackageEvent| reporter.set_message(event.message)),
    )
    .context("Can't package the workspace")?;
    spinner.clear();

    Ok(PreparedCodeVersion {
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

fn prepare_binaries(
    context: &CliContext,
    project: &ProjectContext,
    args: &PackageArgs,
) -> anyhow::Result<PreparedCodeVersion> {
    let host = target::host_target()?;
    let installed = target::installed_targets();

    let selected = target::select_targets(context.terminal(), &args.targets, host, &installed)?;

    // Offer to install the chosen cross targets whose standard library is missing.
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
            context.terminal().warning(&format!(
                "The binary runs only on {} compute providers.",
                target::pretty_name(os, arch)
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

    // The digest covers each uploaded binary and the target it is uploaded for.
    let digest = manifest_digest(binaries.iter().map(|binary| {
        (
            target::target_triple(binary.os, binary.architecture),
            binary.checksum.as_str(),
        )
    }));

    Ok(PreparedCodeVersion {
        digest,
        request: PublishArtifactRequest::Binaries { binaries },
        uploads,
        targets: selected
            .iter()
            .map(|&(os, arch)| target::target_triple(os, arch).to_string())
            .collect(),
    })
}

/// Build the release binary with `driver`, for `target` when cross-building, and return
/// its path, asking which one when the build makes several.
fn build_release_binary(
    context: &CliContext,
    target: Option<&str>,
    driver: BuildDriver,
    linker: Option<&str>,
    bin: Option<&str>,
) -> anyhow::Result<PathBuf> {
    let cmd_label = match target {
        Some(triple) => format!("{} --release --target {triple}", driver.label()),
        None => format!("{} --release", driver.label()),
    };
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
    let building = match target {
        Some(triple) => format!("Building for {triple}"),
        None => "Building the binary".to_string(),
    };
    let spinner = context.terminal().spinner(&building);
    let built = cargo::build_quietly(command, &cmd_label, |status| {
        spinner.set_message(format!("{building}: {status}"));
    });
    spinner.clear();
    let built = built?;
    if !built.succeeded() {
        built.show_output();
    }
    let executables = built.executables()?;

    let names: Vec<&str> = executables.iter().map(|(name, _)| name.as_str()).collect();
    if let Some(bin) = bin {
        return executables
            .iter()
            .find(|(name, _)| name == bin)
            .map(|(_, path)| path.clone())
            .ok_or_else(|| {
                CliError::new(
                    ErrorKind::Usage,
                    format!("No binary '{bin}'. Binaries: {}.", names.join(", ")),
                )
                .into()
            });
    }
    match executables.len() {
        0 => anyhow::bail!("The build made no binary."),
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
                "Binary",
                &items,
                None,
                Unanswered::new(
                    format!("Several binaries were built: {}.", names.join(", ")),
                    "Pass --bin <name>.",
                ),
            )
        }
    }
}

const MODE_NEEDED: &str = "Packaging needs a mode.";
const MODE_HINT: &str = "Pass --mode binary or --mode source.";

fn upload(
    context: &CliContext,
    client: &Client,
    project: &ProjectContext,
    mode: Mode,
    prepared: PreparedCodeVersion,
) -> anyhow::Result<Packaged> {
    let linked = project.get_project();
    let digest = prepared.digest.as_str();

    let response = client
        .publish_project_version_urls(
            &linked.namespace,
            &linked.name,
            PublishProjectVersionRequest {
                digest: digest.to_string(),
                artifact: prepared.request,
            },
        )
        .with_context(|| {
            format!(
                "Can't start the upload to {}/{}",
                linked.namespace, linked.name
            )
        })?;

    let packaged = |uploaded| Packaged {
        namespace: linked.namespace.clone(),
        project: linked.name.clone(),
        digest: digest.to_string(),
        version_id: response.id.clone(),
        mode,
        targets: prepared.targets.clone(),
        uploaded,
    };
    let uploads = match &response.urls {
        Some(urls) => requested(prepared.uploads, urls),
        None => Vec::new(),
    };
    if uploads.is_empty() {
        context.terminal().outro(&format!(
            "Code version {} is already uploaded.",
            short_digest(digest)
        ));
        return Ok(packaged(false));
    }

    let spinner = context.terminal().spinner("Uploading the code version");
    for (key, url, path) in uploads {
        let bytes =
            std::fs::read(&path).with_context(|| format!("Can't read '{}'", path.display()))?;
        client
            .upload_bytes_to_url(url, bytes)
            .with_context(|| format!("Can't upload '{key}'"))?;
    }
    spinner.clear();

    client
        .complete_project_version_upload(&linked.namespace, &linked.name, &response.id)
        .with_context(|| {
            format!(
                "Can't finish the upload to {}/{}",
                linked.namespace, linked.name
            )
        })?;

    context.terminal().outro(&format!(
        "Uploaded code version {}.",
        short_digest(&response.digest)
    ));
    Ok(packaged(true))
}

/// The files of a package the Tracel Console asked for, each with its upload URL. A file
/// the code version already has gets no URL and is left out.
fn requested(
    uploads: Vec<(String, PathBuf)>,
    urls: &HashMap<String, String>,
) -> Vec<(String, &str, PathBuf)> {
    uploads
        .into_iter()
        .filter_map(|(key, path)| urls.get(&key).map(|url| (key, url.as_str(), path)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_files_the_console_asks_for_are_uploaded() {
        let uploads = vec![
            (
                "x86_64-unknown-linux-gnu".to_string(),
                PathBuf::from("linux"),
            ),
            ("aarch64-apple-darwin".to_string(), PathBuf::from("macos")),
        ];
        let urls = HashMap::from([(
            "aarch64-apple-darwin".to_string(),
            "https://storage/macos".to_string(),
        )]);

        let requested = requested(uploads, &urls);

        assert_eq!(
            requested,
            [(
                "aarch64-apple-darwin".to_string(),
                "https://storage/macos",
                PathBuf::from("macos")
            )]
        );
    }
}
