use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;
use tracel_client::console::model::request::{
    ModelFileSpecRequest, ModelVersionListState, PromoteModelVersionRequest,
    RequestModelVersionUploadRequest, SetModelAliasRequest,
};
use tracel_client::console::model::response::{
    ModelAliasListResponse, ModelAliasResponse, ModelListResponse, ModelResponse,
    ModelVersionListResponse, ModelVersionResponse, ModelVersionSourceKindResponse,
    ModelVersionStateResponse,
};

use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::{
    DownloadFile, DownloadResult, Resource, build_part_tasks, download_files, ensure_model_exists,
    map_resource_error, parse_metadata, resolve_namespace_project, select_artifact, upload_parts,
    validate_auto_create, validate_rel_path,
};
use crate::tools::fs::{build_file_specs, collect_files};
use crate::tools::tracel_config::TracelProject;
use crate::ui::{Details, Human, Outcome, Render, Table, json_section};

#[derive(Args, Debug)]
pub struct ModelsArgs {
    #[command(subcommand)]
    pub command: ModelsCommands,
}

#[derive(Subcommand, Debug)]
pub enum ModelsCommands {
    /// List models in the selected project.
    List,
    /// Show a model or a version selected by reference.
    Get(GetArgs),
    /// List ready versions, or versions in all states.
    Versions(VersionsArgs),
    /// Download a model version.
    Pull(PullArgs),
    /// Upload a local directory of files as a new model version.
    Push(PushArgs),
    /// Promote an experiment artifact into a new model version.
    Promote(PromoteArgs),
    /// List, set, or remove model aliases.
    Alias(AliasArgs),
}

#[derive(Args, Debug)]
pub struct GetArgs {
    #[arg(value_name = "MODEL", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub model: String,
    /// Version reference: latest, 7, v7, or an alias.
    #[arg(long, value_name = "REF", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub version: Option<String>,
}

#[derive(Args, Debug)]
pub struct VersionsArgs {
    #[arg(value_name = "MODEL", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub model: String,
    /// Include pending, failed, and deleted versions.
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Debug)]
pub struct PullArgs {
    #[arg(value_name = "MODEL", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub model: String,
    /// Version reference: latest, 7, v7, or an alias.
    #[arg(long, value_name = "REF", default_value = "latest", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub version: String,
    /// Destination directory (default: ./<MODEL>-v<version>).
    #[arg(short, long, value_name = "DIR")]
    pub directory: Option<PathBuf>,
    /// Overwrite existing destination files.
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct PushArgs {
    /// Name of the model to upload a version to.
    #[arg(value_name = "MODEL", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub model_name: String,
    /// Local directory containing the files to upload.
    #[arg(short, long, value_name = "DIR")]
    pub directory: PathBuf,
    /// Create a missing model (true/false); otherwise ask when interactive.
    #[arg(long, short)]
    pub auto_create: Option<bool>,
    /// Description to use when auto-creating the model. Requires --auto-create true.
    #[arg(long, requires = "auto_create", value_name = "TEXT")]
    pub description: Option<String>,
    /// Metadata for the new version, as a JSON object.
    #[arg(long, value_name = "JSON", value_parser = parse_metadata)]
    pub metadata: Option<Value>,
}

#[derive(Args, Debug)]
pub struct PromoteArgs {
    #[arg(value_name = "MODEL", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub model: String,
    /// Project-scoped experiment number.
    #[arg(long, value_name = "NUM", value_parser = clap::value_parser!(i32).range(1..))]
    pub experiment: i32,
    /// Artifact name or id in the experiment.
    #[arg(long, value_name = "NAME|ID", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub artifact: String,
    /// Point this alias at the new version.
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub alias: Option<String>,
    /// Metadata for the new version, as a JSON object.
    #[arg(long, value_name = "JSON", value_parser = parse_metadata)]
    pub metadata: Option<Value>,
    /// Create a missing model (true/false); otherwise ask when interactive.
    #[arg(long)]
    pub auto_create: Option<bool>,
    /// Description to use when auto-creating the model. Requires --auto-create true.
    #[arg(long, requires = "auto_create", value_name = "TEXT")]
    pub description: Option<String>,
}

#[derive(Args, Debug)]
pub struct AliasArgs {
    #[command(subcommand)]
    pub command: AliasCommands,
}

#[derive(Subcommand, Debug)]
pub enum AliasCommands {
    /// List aliases for a model.
    List(AliasListArgs),
    /// Create or move an alias to a ready version.
    Set(AliasSetArgs),
    /// Remove an alias.
    Remove(AliasRemoveArgs),
}

#[derive(Args, Debug)]
pub struct AliasListArgs {
    #[arg(value_name = "MODEL", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub model: String,
}

#[derive(Args, Debug)]
pub struct AliasSetArgs {
    #[arg(value_name = "MODEL", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub model: String,
    #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub alias: String,
    #[arg(value_parser = clap::value_parser!(u32).range(1..))]
    pub version: u32,
    /// Move the alias only if it currently points at this version.
    #[arg(long, value_name = "VERSION", value_parser = clap::value_parser!(u32).range(1..))]
    pub expect: Option<u32>,
}

#[derive(Args, Debug)]
pub struct AliasRemoveArgs {
    #[arg(value_name = "MODEL", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub model: String,
    #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub alias: String,
}

/// A version made by `push` or `promote`.
#[derive(Serialize)]
#[serde(transparent)]
struct NewVersion(ModelVersionResponse);

impl Render for NewVersion {}

#[derive(Serialize)]
struct ModelPulled {
    model: String,
    version: ModelVersionResponse,
    directory: PathBuf,
    files: Vec<DownloadResult>,
    bytes: u64,
}

impl Render for ModelPulled {}

#[derive(Serialize)]
#[serde(transparent)]
struct AliasSet(ModelAliasResponse);

impl Render for AliasSet {}

#[derive(Serialize)]
struct AliasRemoved {
    model: String,
    alias: String,
    removed: bool,
}

impl Render for AliasRemoved {}

impl Render for ModelListResponse {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        Table::new(["NAME", "VERSIONS", "LATEST", "ALIASES", "CREATED AT"])
            .shrink("ALIASES")
            .rows(self.items.iter().map(|model| {
                [
                    model.name.clone(),
                    model.version_count.to_string(),
                    latest_version(model),
                    alias_summary(&model.aliases),
                    model.created_at.clone(),
                ]
            }))
            .total(self.total)
            .write(out)
    }
}

impl Render for ModelResponse {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        Details::new()
            .field("Name", &self.name)
            .field("ID", &self.id)
            .field("Project ID", self.project_id)
            .field("Display name", &self.display_name)
            .optional("Description", self.description.as_ref())
            .field("Versions", self.version_count)
            .field("Latest", latest_version(self))
            .field("Aliases", alias_summary(&self.aliases))
            .field("Created at", &self.created_at)
            .field("Created by", &self.created_by.username)
            .write(out)
    }
}

impl Render for ModelVersionResponse {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        Details::new()
            .field("Version", format!("v{}", self.version))
            .field("ID", &self.id)
            .field("State", state_name(self.state))
            .field("Source", source_name(self.source_kind))
            .optional(
                "Experiment",
                self.experiment
                    .as_ref()
                    .map(|experiment| experiment.experiment_num),
            )
            .field("Size", self.size)
            .field("Digest", &self.digest)
            .field("Aliases", self.aliases.join(", "))
            .field("Created at", &self.created_at)
            .field("Created by", &self.created_by.username)
            .optional("Failure reason", self.failure_reason.as_ref())
            .write(out)?;
        writeln!(out)?;
        Table::new(["REL PATH", "SIZE", "CHECKSUM"])
            .rows(self.manifest.files.iter().map(|file| {
                [
                    file.rel_path.clone(),
                    file.size_bytes.to_string(),
                    file.checksum.clone(),
                ]
            }))
            .write(out)?;
        writeln!(out)?;
        json_section(out, "Metadata", &self.metadata)
    }
}

impl Render for ModelVersionListResponse {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        Table::new([
            "VERSION",
            "STATE",
            "SOURCE",
            "SIZE",
            "ALIASES",
            "CREATED AT",
        ])
        .shrink("ALIASES")
        .rows(self.items.iter().map(|version| {
            let source = match &version.experiment {
                Some(experiment) => format!(
                    "{} (experiment {})",
                    source_name(version.source_kind),
                    experiment.experiment_num
                ),
                None => source_name(version.source_kind).into(),
            };
            [
                format!("v{}", version.version),
                state_name(version.state).into(),
                source,
                version.size.to_string(),
                version.aliases.join(", "),
                version.created_at.clone(),
            ]
        }))
        .total(self.total)
        .write(out)
    }
}

impl Render for ModelAliasListResponse {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        Table::new(["ALIAS", "VERSION"])
            .rows(
                self.items
                    .iter()
                    .map(|alias| [alias.alias.clone(), format!("v{}", alias.version)]),
            )
            .total(self.total)
            .write(out)
    }
}

fn latest_version(model: &ModelResponse) -> String {
    model
        .latest_version
        .map(|version| format!("v{version}"))
        .unwrap_or_default()
}

fn alias_summary(aliases: &[ModelAliasResponse]) -> String {
    aliases
        .iter()
        .map(|alias| format!("{}=v{}", alias.alias, alias.version))
        .collect::<Vec<_>>()
        .join(", ")
}

fn state_name(state: ModelVersionStateResponse) -> &'static str {
    match state {
        ModelVersionStateResponse::Pending => "pending",
        ModelVersionStateResponse::Ready => "ready",
        ModelVersionStateResponse::Failed => "failed",
        ModelVersionStateResponse::Deleted => "deleted",
    }
}

fn source_name(source: ModelVersionSourceKindResponse) -> &'static str {
    match source {
        ModelVersionSourceKindResponse::Upload => "upload",
        ModelVersionSourceKindResponse::Promotion => "promotion",
    }
}

fn validate_download_directory(directory: Option<&Path>, model: &str) -> anyhow::Result<()> {
    if let Some(directory) = directory {
        if directory.as_os_str().is_empty() || (directory.exists() && !directory.is_dir()) {
            return Err(CliError::new(
                ErrorKind::Usage,
                format!("Invalid destination directory '{}'.", directory.display()),
            )
            .into());
        }
    } else if validate_rel_path(model).is_err() || Path::new(model).components().count() != 1 {
        return Err(CliError::new(
            ErrorKind::Usage,
            format!("Model name '{model}' needs an explicit destination directory."),
        )
        .with_hint("Pass --directory <DIR>.")
        .into());
    }
    Ok(())
}

pub fn handle_command(args: ModelsArgs, context: CliContext) -> anyhow::Result<Outcome> {
    let project = resolve_namespace_project(&context)?.project;
    let (namespace, name) = (&project.owner, &project.name);
    let client = || get_client_and_login_if_needed(&context);
    match args.command {
        ModelsCommands::List => Ok(client()?.list_models(namespace, name)?.into()),
        ModelsCommands::Get(args) => {
            let client = client()?;
            match args.version {
                Some(reference) => Ok(client
                    .resolve_model_version_ref(namespace, name, &args.model, &reference)
                    .map_err(|error| {
                        map_resource_error(
                            error,
                            namespace,
                            name,
                            Resource::ModelVersionRef {
                                model: &args.model,
                                reference: &reference,
                            },
                        )
                    })?
                    .into()),
                None => Ok(client
                    .get_model(namespace, name, &args.model)
                    .map_err(|error| {
                        map_resource_error(error, namespace, name, Resource::Model(&args.model))
                    })?
                    .into()),
            }
        }
        ModelsCommands::Versions(args) => {
            let client = client()?;
            Ok(if args.all {
                client.list_model_versions_in_state(
                    namespace,
                    name,
                    &args.model,
                    ModelVersionListState::All,
                )
            } else {
                client.list_model_versions(namespace, name, &args.model)
            }
            .map_err(|error| {
                map_resource_error(error, namespace, name, Resource::Model(&args.model))
            })?
            .into())
        }
        ModelsCommands::Pull(args) => Ok(pull(args, &context, &project)?.into()),
        ModelsCommands::Push(args) => Ok(push(args, &context, &project)?.into()),
        ModelsCommands::Promote(args) => Ok(promote(args, &context, &project)?.into()),
        ModelsCommands::Alias(args) => alias(args.command, &context, &project),
    }
}

fn pull(
    args: PullArgs,
    context: &CliContext,
    project: &TracelProject,
) -> anyhow::Result<ModelPulled> {
    let (namespace, name) = (&project.owner, &project.name);
    context.terminal().command_title("Model download");
    validate_download_directory(args.directory.as_deref(), &args.model)?;
    let client = get_client_and_login_if_needed(context)?;
    let map_error = |error| {
        map_resource_error(
            error,
            namespace,
            name,
            Resource::ModelVersionRef {
                model: &args.model,
                reference: &args.version,
            },
        )
    };
    let version = client
        .resolve_model_version_ref(namespace, name, &args.model, &args.version)
        .map_err(map_error)?;
    let directory = args
        .directory
        .unwrap_or_else(|| PathBuf::from(format!("./{}-v{}", args.model, version.version)));
    let response = client
        .presign_model_download(namespace, name, &args.model, version.version)
        .map_err(map_error)?;
    let files: Vec<_> = response
        .files
        .into_iter()
        .map(|file| DownloadFile {
            rel_path: file.rel_path,
            url: file.url,
            size_bytes: Some(file.size_bytes),
            checksum: Some(file.checksum),
        })
        .collect();
    let files = download_files(&client, context.terminal(), &directory, &files, args.force)?;
    context.terminal().finalize(&format!(
        "Downloaded '{}' v{} to {}.",
        args.model,
        version.version,
        directory.display()
    ));
    Ok(ModelPulled {
        bytes: files.iter().map(|file| file.bytes).sum(),
        model: args.model,
        version,
        directory,
        files,
    })
}

fn push(
    args: PushArgs,
    context: &CliContext,
    project: &TracelProject,
) -> anyhow::Result<NewVersion> {
    let (namespace, name) = (&project.owner, &project.name);
    context.terminal().command_title("Model upload");
    validate_auto_create(args.auto_create, args.description.as_deref())?;
    let files = collect_push_files(&args.directory, context)?;
    let client = get_client_and_login_if_needed(context)?;
    context
        .terminal()
        .print(&format!("Uploading to {namespace}/{name}"));

    ensure_model_exists(
        context,
        &client,
        namespace,
        name,
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

    let file_sizes: BTreeMap<String, u64> = file_specs
        .iter()
        .map(|f| (f.rel_path.clone(), f.size_bytes))
        .collect();

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
        metadata: args.metadata,
    };
    let upload = client
        .request_model_version_upload(namespace, name, &args.model_name, upload_request)
        .map_err(|e| {
            spinner.error("Failed to request upload URLs.");
            map_resource_error(e, namespace, name, Resource::Model(&args.model_name))
        })?;
    spinner.stop(format!("Allocated model version {}.", upload.version));

    let tasks = build_part_tasks(&files, &file_sizes, &upload.files)?;
    upload_parts(&client, tasks, context.terminal())?;

    let map_error =
        |error| map_resource_error(error, namespace, name, Resource::Model(&args.model_name));
    client
        .complete_model_version_upload(namespace, name, &args.model_name, upload.version)
        .map_err(map_error)?;
    let version = client
        .get_model_version(namespace, name, &args.model_name, upload.version)
        .map_err(map_error)?;

    context.terminal().finalize(&format!(
        "Uploaded model '{}' v{} to {namespace}/{name}.",
        args.model_name, upload.version
    ));
    Ok(NewVersion(version))
}

fn collect_push_files(
    directory: &Path,
    context: &CliContext,
) -> anyhow::Result<BTreeMap<String, PathBuf>> {
    let spinner = context.terminal().spinner();
    spinner.start("Collecting files...");
    let files = collect_files(directory).map_err(|error| {
        spinner.error("Failed to collect files.");
        CliError::new(ErrorKind::Usage, format!("{error:#}"))
    })?;
    spinner.stop(format!("Found {} file(s).", files.len()));
    Ok(files)
}

fn promote(
    args: PromoteArgs,
    context: &CliContext,
    project: &TracelProject,
) -> anyhow::Result<NewVersion> {
    let (namespace, name) = (&project.owner, &project.name);
    context.terminal().command_title("Model promotion");
    validate_auto_create(args.auto_create, args.description.as_deref())?;
    let client = get_client_and_login_if_needed(context)?;
    let artifacts = client
        .list_artifacts(namespace, name, args.experiment)
        .map_err(|error| {
            map_resource_error(
                error,
                namespace,
                name,
                Resource::Experiment(args.experiment),
            )
        })?;
    let index = select_artifact(
        artifacts
            .items
            .iter()
            .map(|artifact| (artifact.id.as_str(), artifact.name.as_str())),
        &args.artifact,
        args.experiment,
    )?;
    let artifact = &artifacts.items[index];
    ensure_model_exists(
        context,
        &client,
        namespace,
        name,
        &args.model,
        args.auto_create,
        args.description,
    )?;
    let map_error =
        |error| map_resource_error(error, namespace, name, Resource::Model(&args.model));
    let mut version = client
        .promote_model_version(
            namespace,
            name,
            &args.model,
            PromoteModelVersionRequest {
                experiment_num: args.experiment,
                experiment_file_id: artifact.id.clone(),
                metadata: args.metadata,
            },
        )
        .map_err(map_error)?;
    if let Some(alias) = args.alias {
        client
            .set_model_alias(
                namespace,
                name,
                &args.model,
                &alias,
                SetModelAliasRequest {
                    version: version.version,
                    expected_current_version: None,
                },
            )
            .map_err(map_error)?;
        version = client
            .get_model_version(namespace, name, &args.model, version.version)
            .map_err(map_error)?;
    }
    context.terminal().finalize(&format!(
        "Promoted artifact '{}' to '{}' v{}.",
        artifact.name, args.model, version.version
    ));
    Ok(NewVersion(version))
}

fn alias(
    command: AliasCommands,
    context: &CliContext,
    project: &TracelProject,
) -> anyhow::Result<Outcome> {
    let (namespace, name) = (&project.owner, &project.name);
    match command {
        AliasCommands::List(args) => Ok(get_client_and_login_if_needed(context)?
            .list_model_aliases(namespace, name, &args.model)
            .map_err(|error| {
                map_resource_error(error, namespace, name, Resource::Model(&args.model))
            })?
            .into()),
        AliasCommands::Set(args) => {
            context.terminal().command_title("Model alias");
            let alias = get_client_and_login_if_needed(context)?
                .set_model_alias(
                    namespace,
                    name,
                    &args.model,
                    &args.alias,
                    SetModelAliasRequest {
                        version: args.version,
                        expected_current_version: args.expect,
                    },
                )
                .map_err(|error| {
                    map_resource_error(
                        error,
                        namespace,
                        name,
                        Resource::ModelVersion {
                            model: &args.model,
                            version: args.version,
                        },
                    )
                })?;
            context.terminal().finalize(&format!(
                "Alias '{}' of '{}' points to v{}.",
                alias.alias, args.model, alias.version
            ));
            Ok(AliasSet(alias).into())
        }
        AliasCommands::Remove(args) => {
            context.terminal().command_title("Model alias");
            get_client_and_login_if_needed(context)?
                .remove_model_alias(namespace, name, &args.model, &args.alias)
                .map_err(|error| {
                    map_resource_error(
                        error,
                        namespace,
                        name,
                        Resource::ModelAlias {
                            model: &args.model,
                            alias: &args.alias,
                        },
                    )
                })?;
            context.terminal().finalize(&format!(
                "Removed alias '{}' from '{}'.",
                args.alias, args.model
            ));
            Ok(AliasRemoved {
                model: args.model,
                alias: args.alias,
                removed: true,
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

    fn parse_command(arguments: &[&str]) -> ModelsArgs {
        let args = CliArgs::try_parse_from(arguments).unwrap();
        let Some(Commands::Models(args)) = args.command else {
            panic!("Expected models command");
        };
        args
    }

    #[test]
    fn definitions_and_read_commands_are_valid() {
        CliArgs::command().debug_assert();
        assert!(matches!(
            parse_command(&["tracel", "models", "list"]).command,
            ModelsCommands::List
        ));
        let args = parse_command(&["tracel", "models", "get", "weights"]);
        let ModelsCommands::Get(args) = args.command else {
            panic!("Expected get");
        };
        assert_eq!(args.model, "weights");
        assert!(args.version.is_none());
        for reference in ["latest", "7", "v7", "production"] {
            let args =
                parse_command(&["tracel", "models", "get", "weights", "--version", reference]);
            let ModelsCommands::Get(args) = args.command else {
                panic!("Expected get");
            };
            assert_eq!(args.version.as_deref(), Some(reference));
        }
        let args = parse_command(&["tracel", "models", "versions", "weights", "--all"]);
        let ModelsCommands::Versions(args) = args.command else {
            panic!("Expected versions");
        };
        assert!(args.all);
        let args = parse_command(&["tracel", "models", "pull", "weights"]);
        let ModelsCommands::Pull(args) = args.command else {
            panic!("Expected pull");
        };
        assert_eq!(args.version, "latest");
        assert!(args.directory.is_none());
        assert!(!args.force);
        let args = parse_command(&[
            "tracel",
            "models",
            "pull",
            "weights",
            "--version",
            "production",
            "-d",
            "./out",
            "--force",
            "-o",
            "json",
        ]);
        let ModelsCommands::Pull(args) = args.command else {
            panic!("Expected pull");
        };
        assert_eq!(args.version, "production");
        assert_eq!(args.directory.unwrap(), Path::new("./out"));
        assert!(args.force);
    }

    #[test]
    fn push_and_promote_parse_creation_and_metadata() {
        let args = parse_command(&[
            "tracel",
            "models",
            "push",
            "weights",
            "-d",
            ".",
            "-a",
            "true",
            "--description",
            "Weights",
            "--metadata",
            r#"{"format":"bin"}"#,
        ]);
        let ModelsCommands::Push(args) = args.command else {
            panic!("Expected push");
        };
        assert_eq!(args.model_name, "weights");
        assert_eq!(args.directory, Path::new("."));
        assert_eq!(args.auto_create, Some(true));
        assert_eq!(args.description.as_deref(), Some("Weights"));
        assert_eq!(args.metadata.unwrap(), json!({"format": "bin"}));
        let args = parse_command(&[
            "tracel",
            "models",
            "promote",
            "weights",
            "--experiment",
            "7",
            "--artifact",
            "artifact-id",
            "--alias",
            "production",
            "--auto-create",
            "false",
            "--metadata",
            "{}",
        ]);
        let ModelsCommands::Promote(args) = args.command else {
            panic!("Expected promote");
        };
        assert_eq!(args.experiment, 7);
        assert_eq!(args.artifact, "artifact-id");
        assert_eq!(args.alias.as_deref(), Some("production"));
        assert_eq!(args.auto_create, Some(false));
        assert_eq!(args.metadata.unwrap(), json!({}));
        for value in ["null", "[]", "1", "{invalid"] {
            assert!(
                CliArgs::try_parse_from([
                    "tracel",
                    "models",
                    "push",
                    "weights",
                    "-d",
                    ".",
                    "--metadata",
                    value
                ])
                .is_err()
            );
            assert!(
                CliArgs::try_parse_from([
                    "tracel",
                    "models",
                    "promote",
                    "weights",
                    "--experiment",
                    "7",
                    "--artifact",
                    "weights",
                    "--metadata",
                    value
                ])
                .is_err()
            );
        }
        for arguments in [
            vec!["tracel", "models", "push", "weights"],
            vec![
                "tracel",
                "models",
                "push",
                "weights",
                "-d",
                ".",
                "--description",
                "text",
            ],
            vec![
                "tracel",
                "models",
                "push",
                "weights",
                "-d",
                ".",
                "--auto-create",
                "maybe",
            ],
            vec![
                "tracel",
                "models",
                "promote",
                "weights",
                "--artifact",
                "weights",
            ],
            vec![
                "tracel",
                "models",
                "promote",
                "weights",
                "--experiment",
                "0",
                "--artifact",
                "weights",
            ],
        ] {
            assert!(CliArgs::try_parse_from(arguments).is_err());
        }
    }

    #[test]
    fn aliases_require_positive_versions_and_parse_the_guard() {
        let args = parse_command(&["tracel", "models", "alias", "list", "weights"]);
        let ModelsCommands::Alias(args) = args.command else {
            panic!("Expected alias");
        };
        assert!(
            matches!(args.command, AliasCommands::List(AliasListArgs { model }) if model == "weights")
        );
        let args = parse_command(&[
            "tracel",
            "models",
            "alias",
            "set",
            "weights",
            "production",
            "7",
            "--expect",
            "6",
        ]);
        let ModelsCommands::Alias(args) = args.command else {
            panic!("Expected alias");
        };
        let AliasCommands::Set(args) = args.command else {
            panic!("Expected set");
        };
        assert_eq!(args.model, "weights");
        assert_eq!(args.alias, "production");
        assert_eq!(args.version, 7);
        assert_eq!(args.expect, Some(6));
        let args = parse_command(&[
            "tracel",
            "models",
            "alias",
            "remove",
            "weights",
            "production",
        ]);
        let ModelsCommands::Alias(args) = args.command else {
            panic!("Expected alias");
        };
        assert!(
            matches!(args.command, AliasCommands::Remove(AliasRemoveArgs { model, alias }) if model == "weights" && alias == "production")
        );
        for version in ["0", "-1", "4294967296", "v7"] {
            assert!(
                CliArgs::try_parse_from([
                    "tracel",
                    "models",
                    "alias",
                    "set",
                    "weights",
                    "production",
                    version
                ])
                .is_err()
            );
            assert!(
                CliArgs::try_parse_from([
                    "tracel",
                    "models",
                    "alias",
                    "set",
                    "weights",
                    "production",
                    "7",
                    "--expect",
                    version
                ])
                .is_err()
            );
        }
    }

    #[test]
    fn default_pull_destination_requires_a_single_normal_model_name() {
        assert!(validate_download_directory(None, "weights").is_ok());
        for model in [
            "",
            ".",
            "..",
            "/weights",
            "../weights",
            "nested/weights",
            "C:\\weights",
        ] {
            assert_eq!(
                crate::error::classify(&validate_download_directory(None, model).unwrap_err()),
                ErrorKind::Usage
            );
            assert!(validate_download_directory(Some(Path::new("./out")), model).is_ok());
        }
    }
}
