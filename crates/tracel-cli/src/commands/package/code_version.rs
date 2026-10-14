use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use serde::Serialize;
use tracel_client::console::Client;
use tracel_client::console::project::request::{
    PublishArtifactRequest, PublishBinaryRequest, PublishProjectVersionRequest,
    PublishSourceRequest,
};

use super::{MODE_HINT, MODE_NEEDED, Mode, PackageArgs, check_targets_allowed};
use crate::context::CliContext;
use crate::tools::build_driver::{self, BuildDriver};
use crate::tools::cargo::{self, Binary};
use crate::tools::fs::file_sha256_and_size;
use crate::tools::packager::{self, PackageEvent};
use crate::tools::project_context::ProjectContext;
use crate::tools::target;
use crate::ui::Unanswered;

/// A code version ready to upload: the digest of its content, its publish request, each
/// file by the key of its upload URL, and the targets it was built for.
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

/// Build the workspace in the mode from `args`, asking for one when it is missing.
pub fn build_package(
    context: &CliContext,
    project: &ProjectContext,
    args: &PackageArgs,
) -> anyhow::Result<BuiltPackage> {
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

    match mode {
        Mode::Source => build_source_package(context, project),
        Mode::Binary => build_binary_package(context, project, args),
    }
}

fn build_source_package(
    context: &CliContext,
    project: &ProjectContext,
) -> anyhow::Result<BuiltPackage> {
    let spinner = context.terminal().spinner("Packaging the workspace");
    let reporter = spinner.clone();
    let result = packager::package_workspace(
        &project.workspace_info,
        Arc::new(move |event: PackageEvent| reporter.set_message(event.message)),
    )
    .context("Can't package the workspace")?;
    spinner.clear();

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

    // Offer to install the chosen cross targets whose standard library is missing.
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
    let bin = args
        .bin
        .as_deref()
        .map(|name| cargo::find_binary(&cargo::binaries(&project.workspace_info.metadata), name))
        .transpose()?;
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
            bin.as_ref(),
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

/// Build the release binary with `driver`, for `target` when cross-building, and return
/// its path. A `bin` is the only binary built, as `tracel run` builds it, so the build
/// `tracel run` made for this machine is reused; otherwise the user picks one when the
/// build makes several.
fn build_release_binary(
    context: &CliContext,
    target: Option<&str>,
    driver: BuildDriver,
    linker: Option<&str>,
    bin: Option<&Binary>,
) -> anyhow::Result<PathBuf> {
    let (command, label) = cargo::release_build(driver, target, linker, bin);
    let building = match target {
        Some(triple) => format!("Building for {triple}"),
        None => "Building the binary".to_string(),
    };
    let executables = cargo::build(context.terminal(), command, &label, &building)?;

    if let Some(bin) = bin {
        return executables
            .into_iter()
            .find(|(name, _)| *name == bin.name)
            .map(|(_, path)| path)
            .with_context(|| format!("`{label}` made no binary '{}'", bin.name));
    }
    let names: Vec<&str> = executables.iter().map(|(name, _)| name.as_str()).collect();
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

impl BuiltPackage {
    /// Upload the package as a code version of the project: the files the code version
    /// with this digest doesn't have yet, or nothing when it has them all.
    pub fn publish(
        self,
        context: &CliContext,
        client: &Client,
        project: &ProjectContext,
    ) -> anyhow::Result<PackagedCodeVersion> {
        let linked = project.get_project();

        let response = client
            .publish_project_version_urls(
                &linked.namespace,
                &linked.name,
                PublishProjectVersionRequest {
                    digest: self.digest.clone(),
                    artifact: self.request,
                },
            )
            .with_context(|| {
                format!(
                    "Can't start the upload to {}/{}",
                    linked.namespace, linked.name
                )
            })?;

        let mut version = PackagedCodeVersion {
            namespace: linked.namespace.clone(),
            project: linked.name.clone(),
            digest: self.digest,
            version_id: response.id,
            mode: self.mode,
            targets: self.targets,
            uploaded: false,
        };
        let Some(urls) = response.urls else {
            return Ok(version);
        };
        let uploads = requested(self.uploads, &urls);
        if uploads.is_empty() {
            return Ok(version);
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
            .complete_project_version_upload(&linked.namespace, &linked.name, &version.version_id)
            .with_context(|| {
                format!(
                    "Can't finish the upload to {}/{}",
                    linked.namespace, linked.name
                )
            })?;

        version.uploaded = true;
        Ok(version)
    }
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
