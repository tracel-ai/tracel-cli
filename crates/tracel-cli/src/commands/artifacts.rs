use std::io;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;
use tracel_client::console::artifact::response::{ArtifactListResponse, ArtifactResponse};

use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::{
    DownloadFile, DownloadResult, Resource, download_files, map_resource_error,
    resolve_namespace_project, select_artifact, validate_rel_path,
};
use crate::ui::{Human, Layout, Outcome, Render, Table, list_summary, size, when};

#[derive(Args, Debug)]
pub struct ArtifactsArgs {
    #[command(subcommand)]
    pub command: ArtifactsCommands,
}

impl ArtifactsArgs {
    pub fn layout(&self) -> Layout {
        match &self.command {
            ArtifactsCommands::List(_) => Layout::Read,
            ArtifactsCommands::Download(args) => Layout::flow(format!(
                "Download artifact '{}' of experiment {}",
                args.artifact, args.experiment
            )),
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum ArtifactsCommands {
    /// List an experiment's artifacts
    List(ListArgs),
    /// Download an artifact by name or id
    Download(DownloadArgs),
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Experiment number
    #[arg(value_parser = clap::value_parser!(i32).range(1..))]
    pub experiment: i32,
    /// Show only artifacts whose name contains this text
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub name: Option<String>,
}

#[derive(Args, Debug)]
pub struct DownloadArgs {
    /// Experiment number
    #[arg(value_parser = clap::value_parser!(i32).range(1..))]
    pub experiment: i32,
    /// Artifact name or id
    #[arg(value_name = "NAME|ID", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub artifact: String,
    /// Download into this directory
    #[arg(short, long, value_name = "DIR")]
    pub directory: Option<PathBuf>,
    /// Overwrite existing files
    #[arg(long)]
    pub force: bool,
}

fn default_directory(name: &str) -> Result<PathBuf, CliError> {
    if validate_rel_path(name).is_err() || Path::new(name).components().count() != 1 {
        return Err(CliError::new(
            ErrorKind::Usage,
            format!("Artifact name '{name}' needs an explicit destination directory."),
        )
        .with_hint("Pass --directory <DIR>."));
    }
    Ok(PathBuf::from(format!("./{name}")))
}

fn manifest_files(manifest: &Value) -> Option<&Vec<Value>> {
    manifest.get("files").and_then(Value::as_array)
}

fn file_integrity(manifest: &Value, rel_path: &str) -> (Option<u64>, Option<String>) {
    let file = manifest_files(manifest).and_then(|files| {
        files
            .iter()
            .find(|file| file.get("rel_path").and_then(Value::as_str) == Some(rel_path))
    });
    match file {
        Some(file) => (
            file.get("size_bytes").and_then(Value::as_u64),
            file.get("checksum")
                .and_then(Value::as_str)
                .map(str::to_owned),
        ),
        None => (None, None),
    }
}

#[derive(Serialize)]
struct ArtifactDownloaded {
    experiment: i32,
    artifact: ArtifactResponse,
    directory: PathBuf,
    files: Vec<DownloadResult>,
    bytes: u64,
}

impl Render for ArtifactListResponse {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        Table::new(["NAME", "KIND", "FILES", "CREATED AT"])
            .rows(self.items.iter().map(|artifact| {
                [
                    artifact.name.clone(),
                    artifact.kind.clone(),
                    manifest_files(&artifact.manifest)
                        .map(|files| files.len().to_string())
                        .unwrap_or_default(),
                    when(&artifact.created_at),
                ]
            }))
            .write(out)
    }

    fn summary(&self) -> Option<String> {
        Some(list_summary(
            self.items.len(),
            self.total as u64,
            ["artifact", "artifacts"],
            "No artifacts in this experiment.",
        ))
    }
}

pub fn handle_command(args: ArtifactsArgs, context: CliContext) -> anyhow::Result<Outcome> {
    let project = resolve_namespace_project(&context)?.project;
    if let ArtifactsCommands::Download(args) = &args.command
        && let Some(directory) = &args.directory
        && (directory.as_os_str().is_empty() || (directory.exists() && !directory.is_dir()))
    {
        return Err(CliError::new(
            ErrorKind::Usage,
            format!("'{}' isn't a directory.", directory.display()),
        )
        .into());
    }
    let client = get_client_and_login_if_needed(&context)?;
    let namespace = &project.namespace;
    let name = &project.name;
    match args.command {
        ArtifactsCommands::List(args) => Ok(match args.name {
            Some(filter) => {
                client.list_artifacts_by_name(namespace, name, args.experiment, &filter)
            }
            None => client.list_artifacts(namespace, name, args.experiment),
        }
        .map_err(|error| {
            map_resource_error(
                error,
                namespace,
                name,
                Resource::Experiment(args.experiment),
            )
        })?
        .into()),
        ArtifactsCommands::Download(args) => {
            let map_error = |error| {
                map_resource_error(
                    error,
                    namespace,
                    name,
                    Resource::Experiment(args.experiment),
                )
            };
            let mut artifacts = client
                .list_artifacts(namespace, name, args.experiment)
                .map_err(map_error)?;
            let index = select_artifact(
                artifacts
                    .items
                    .iter()
                    .map(|artifact| (artifact.id.as_str(), artifact.name.as_str())),
                &args.artifact,
                args.experiment,
            )?;
            let artifact = artifacts.items.swap_remove(index);
            let directory = match args.directory {
                Some(directory) => directory,
                None => default_directory(&artifact.name)?,
            };
            let response = client
                .presign_artifact_download(namespace, name, args.experiment, &artifact.id)
                .map_err(|error| {
                    map_resource_error(
                        error,
                        namespace,
                        name,
                        Resource::Artifact {
                            experiment: args.experiment,
                            artifact: &artifact.name,
                        },
                    )
                })?;
            let files: Vec<_> = response
                .files
                .into_iter()
                .map(|file| {
                    let (size_bytes, checksum) = file_integrity(&artifact.manifest, &file.rel_path);
                    DownloadFile {
                        rel_path: file.rel_path,
                        url: file.url,
                        size_bytes,
                        checksum,
                    }
                })
                .collect();
            let files = download_files(context.terminal(), &directory, &files, args.force)?;
            let bytes = files.iter().map(|file| file.bytes).sum();
            context.terminal().outro(&format!(
                "Downloaded artifact '{}' ({}) to {}.",
                artifact.name,
                size(bytes),
                directory.display()
            ));
            Ok(Outcome::data(ArtifactDownloaded {
                experiment: args.experiment,
                bytes,
                artifact,
                directory,
                files,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde_json::json;

    use super::*;
    use crate::error::{ErrorReport, classify};

    #[test]
    fn given_a_plain_artifact_name_when_choosing_where_to_download_then_it_is_a_directory_named_after_it()
     {
        let directory = default_directory("weights").unwrap();

        assert_eq!(directory, PathBuf::from("./weights"));
    }

    #[rstest]
    #[case::empty("")]
    #[case::nested("weights/best")]
    #[case::parent("../weights")]
    #[case::absolute("/weights")]
    fn given_a_name_that_is_not_one_plain_directory_when_choosing_where_to_download_then_one_must_be_passed(
        #[case] name: &str,
    ) {
        let error = anyhow::Error::new(default_directory(name).unwrap_err());

        assert_eq!(classify(&error), ErrorKind::Usage);
        assert!(
            ErrorReport::new(&error)
                .hint
                .unwrap()
                .contains("--directory")
        );
    }

    #[test]
    fn given_a_manifest_entry_for_a_file_when_reading_its_integrity_then_it_has_its_size_and_checksum()
     {
        let manifest =
            json!({"files": [{"rel_path": "weights.bin", "size_bytes": 7, "checksum": "abc"}]});

        let integrity = file_integrity(&manifest, "weights.bin");

        assert_eq!(integrity, (Some(7), Some("abc".to_string())));
    }

    #[test]
    fn given_a_file_the_manifest_does_not_list_when_reading_its_integrity_then_there_is_none() {
        let manifest = json!({"files": [{"rel_path": "weights.bin", "size_bytes": 7}]});

        let integrity = file_integrity(&manifest, "config.json");

        assert_eq!(integrity, (None, None));
    }
}
