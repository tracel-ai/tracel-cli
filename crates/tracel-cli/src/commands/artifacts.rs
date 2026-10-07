use std::io::{self, Write};
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
use crate::output::{Outcome, Render, Table};

#[derive(Args, Debug)]
pub struct ArtifactsArgs {
    #[command(subcommand)]
    pub command: ArtifactsCommands,
}

#[derive(Subcommand, Debug)]
pub enum ArtifactsCommands {
    /// List artifacts in an experiment.
    List(ListArgs),
    /// Download an artifact selected by name or id.
    Download(DownloadArgs),
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Project-scoped experiment number.
    #[arg(value_parser = clap::value_parser!(i32).range(1..))]
    pub experiment: i32,
    /// Show only artifacts whose name contains this text.
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub name: Option<String>,
}

#[derive(Args, Debug)]
pub struct DownloadArgs {
    /// Project-scoped experiment number.
    #[arg(value_parser = clap::value_parser!(i32).range(1..))]
    pub experiment: i32,
    /// Artifact name or id in the experiment.
    #[arg(value_name = "NAME|ID", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub artifact: String,
    /// Destination directory (default: ./<artifact name>).
    #[arg(short, long, value_name = "DIR")]
    pub directory: Option<PathBuf>,
    /// Overwrite existing destination files.
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

impl Render for ArtifactDownloaded {}

impl Render for ArtifactListResponse {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        Table::new(["NAME", "KIND", "ID", "FILES", "CREATED AT"])
            .rows(self.items.iter().map(|artifact| {
                [
                    artifact.name.clone(),
                    artifact.kind.clone(),
                    artifact.id.clone(),
                    manifest_files(&artifact.manifest)
                        .map(|files| files.len().to_string())
                        .unwrap_or_default(),
                    artifact.created_at.clone(),
                ]
            }))
            .total(self.total)
            .write(out)
    }
}

pub fn handle_command(args: ArtifactsArgs, context: CliContext) -> anyhow::Result<Outcome> {
    let project = resolve_namespace_project(&context)?.project;
    if let ArtifactsCommands::Download(args) = &args.command {
        if let Some(directory) = &args.directory {
            if directory.as_os_str().is_empty() || (directory.exists() && !directory.is_dir()) {
                return Err(CliError::new(
                    ErrorKind::Usage,
                    format!("Invalid destination directory '{}'.", directory.display()),
                )
                .into());
            }
        }
    }
    let client = get_client_and_login_if_needed(&context)?;
    let namespace = &project.owner;
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
            context.terminal().command_title("Artifact download");
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
                .map_err(map_error)?;
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
            let files =
                download_files(&client, context.terminal(), &directory, &files, args.force)?;
            context.terminal().finalize(&format!(
                "Downloaded artifact '{}' to {}.",
                artifact.name,
                directory.display()
            ));
            Ok(ArtifactDownloaded {
                experiment: args.experiment,
                bytes: files.iter().map(|file| file.bytes).sum(),
                artifact,
                directory,
                files,
            }
            .into())
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};
    use serde_json::json;

    use super::*;
    use crate::cli::{CliArgs, Commands};

    fn parse_command(arguments: &[&str]) -> ArtifactsArgs {
        let args = CliArgs::try_parse_from(arguments).unwrap();
        let Some(Commands::Artifacts(args)) = args.command else {
            panic!("Expected artifacts command");
        };
        args
    }

    #[test]
    fn definitions_and_arguments_are_valid() {
        CliArgs::command().debug_assert();
        let args = parse_command(&["tracel", "artifacts", "list", "7", "--name", "weights"]);
        let ArtifactsCommands::List(args) = args.command else {
            panic!("Expected list");
        };
        assert_eq!(args.experiment, 7);
        assert_eq!(args.name.as_deref(), Some("weights"));
        let args = parse_command(&[
            "tracel",
            "artifacts",
            "download",
            "7",
            "artifact-id",
            "-d",
            "weights",
            "--force",
            "-o",
            "json",
        ]);
        let ArtifactsCommands::Download(args) = args.command else {
            panic!("Expected download");
        };
        assert_eq!(args.experiment, 7);
        assert_eq!(args.artifact, "artifact-id");
        assert_eq!(args.directory.unwrap(), Path::new("weights"));
        assert!(args.force);
        let args = parse_command(&["tracel", "artifacts", "download", "1", "weights"]);
        let ArtifactsCommands::Download(args) = args.command else {
            panic!("Expected download");
        };
        assert!(args.directory.is_none());
        assert!(!args.force);
        for number in ["0", "-1", "2147483648", "latest"] {
            assert!(CliArgs::try_parse_from(["tracel", "artifacts", "list", number]).is_err());
        }
    }

    #[test]
    fn default_directory_and_integrity_use_manifest_fields() {
        assert_eq!(
            default_directory("weights").unwrap(),
            Path::new("./weights")
        );
        for name in [
            "",
            ".",
            "..",
            "../weights",
            "weights/nested",
            "/weights",
            "C:\\weights",
        ] {
            assert_eq!(default_directory(name).unwrap_err().kind, ErrorKind::Usage);
        }
        let manifest = json!({"files": [{"rel_path": "a", "size_bytes": 7, "checksum": "abc"}, {"rel_path": "b"}]});
        assert_eq!(manifest_files(&manifest).unwrap().len(), 2);
        assert_eq!(
            file_integrity(&manifest, "a"),
            (Some(7), Some("abc".into()))
        );
        assert_eq!(file_integrity(&manifest, "b"), (None, None));
        assert_eq!(file_integrity(&manifest, "missing"), (None, None));
        assert!(manifest_files(&Value::Null).is_none());
    }
}
