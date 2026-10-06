use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tracel_client::console::Client;
use tracel_client::console::model::request::{
    ModelFileSpecRequest, ModelVersionListState, PromoteModelVersionRequest,
    RequestModelVersionUploadRequest, SetModelAliasRequest,
};
use tracel_client::console::model::response::{
    ModelAliasResponse, ModelResponse, ModelVersionResponse, ModelVersionSourceKindResponse,
    ModelVersionStateResponse,
};

use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::{
    DownloadFile, Resource, build_part_tasks, download_files, ensure_model_exists,
    map_resource_error, parse_metadata, resolve_namespace_project, select_artifact, upload_parts,
    validate_auto_create, validate_rel_path,
};
use crate::output::{OutputMode, write_table};
use crate::tools::fs::{build_file_specs, collect_files};
use crate::tools::tracel_config::TracelProject;

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

fn print_model(model: &ModelResponse) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "Name: {}", model.name)?;
    writeln!(stdout, "ID: {}", model.id)?;
    writeln!(stdout, "Project ID: {}", model.project_id)?;
    writeln!(stdout, "Display name: {}", model.display_name)?;
    writeln!(
        stdout,
        "Description: {}",
        model.description.as_deref().unwrap_or("")
    )?;
    writeln!(stdout, "Versions: {}", model.version_count)?;
    writeln!(
        stdout,
        "Latest: {}",
        model
            .latest_version
            .map(|version| format!("v{version}"))
            .unwrap_or_else(|| "-".into())
    )?;
    writeln!(stdout, "Aliases: {}", alias_summary(&model.aliases))?;
    writeln!(stdout, "Created at: {}", model.created_at)?;
    writeln!(stdout, "Created by: {}", model.created_by.username)?;
    Ok(())
}

fn print_version(version: &ModelVersionResponse) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "Version: v{}", version.version)?;
    writeln!(stdout, "ID: {}", version.id)?;
    writeln!(stdout, "State: {}", state_name(version.state))?;
    writeln!(stdout, "Source: {}", source_name(version.source_kind))?;
    if let Some(experiment) = &version.experiment {
        writeln!(stdout, "Experiment: {}", experiment.experiment_num)?;
    }
    writeln!(stdout, "Size: {}", version.size)?;
    writeln!(stdout, "Digest: {}", version.digest)?;
    writeln!(stdout, "Aliases: {}", version.aliases.join(", "))?;
    writeln!(stdout, "Created at: {}", version.created_at)?;
    writeln!(stdout, "Created by: {}", version.created_by.username)?;
    if let Some(reason) = &version.failure_reason {
        writeln!(stdout, "Failure reason: {reason}")?;
    }
    write_table(
        &mut stdout,
        &["REL PATH", "SIZE", "CHECKSUM"],
        version
            .manifest
            .files
            .iter()
            .map(|file| {
                vec![
                    file.rel_path.clone(),
                    file.size_bytes.to_string(),
                    file.checksum.clone(),
                ]
            })
            .collect(),
    )?;
    writeln!(stdout, "Metadata:")?;
    serde_json::to_writer_pretty(&mut stdout, &version.metadata)?;
    writeln!(stdout)?;
    Ok(())
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

pub fn handle_command(args: ModelsArgs, context: CliContext) -> anyhow::Result<Value> {
    let title = match &args.command {
        ModelsCommands::Push(_) => "Model upload",
        _ => "Models",
    };
    context.terminal().command_title(title);
    let project = resolve_namespace_project(&context)?.project;
    let files = match &args.command {
        ModelsCommands::Push(args) => {
            validate_auto_create(args.auto_create, args.description.as_deref())?;
            let spinner = context.terminal().spinner();
            spinner.start("Collecting files...");
            let files = collect_files(&args.directory).map_err(|error| {
                spinner.error("Failed to collect files.");
                CliError::new(ErrorKind::Usage, format!("{error:#}"))
            })?;
            spinner.stop(format!("Found {} file(s).", files.len()));
            Some(files)
        }
        ModelsCommands::Promote(args) => {
            validate_auto_create(args.auto_create, args.description.as_deref())?;
            None
        }
        ModelsCommands::Pull(args) => {
            validate_download_directory(args.directory.as_deref(), &args.model)?;
            None
        }
        _ => None,
    };
    let client = get_client_and_login_if_needed(&context)?;
    let human = context.output() == OutputMode::Human;
    let namespace = &project.owner;
    let name = &project.name;
    match args.command {
        ModelsCommands::List => {
            let response = client.list_models(namespace, name)?;
            if human {
                write_table(
                    &mut std::io::stdout().lock(),
                    &["NAME", "VERSIONS", "LATEST", "ALIASES", "CREATED AT"],
                    response
                        .items
                        .iter()
                        .map(|model| {
                            vec![
                                model.name.clone(),
                                model.version_count.to_string(),
                                model
                                    .latest_version
                                    .map(|version| format!("v{version}"))
                                    .unwrap_or_else(|| "-".into()),
                                alias_summary(&model.aliases),
                                model.created_at.clone(),
                            ]
                        })
                        .collect(),
                )?;
            }
            Ok(serde_json::to_value(response)?)
        }
        ModelsCommands::Get(args) => {
            if let Some(reference) = args.version {
                let version = client
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
                    })?;
                if human {
                    print_version(&version)?;
                }
                Ok(serde_json::to_value(version)?)
            } else {
                let model = client
                    .get_model(namespace, name, &args.model)
                    .map_err(|error| {
                        map_resource_error(error, namespace, name, Resource::Model(&args.model))
                    })?;
                if human {
                    print_model(&model)?;
                }
                Ok(serde_json::to_value(model)?)
            }
        }
        ModelsCommands::Versions(args) => {
            let response = if args.all {
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
            })?;
            if human {
                write_table(
                    &mut std::io::stdout().lock(),
                    &[
                        "VERSION",
                        "STATE",
                        "SOURCE",
                        "SIZE",
                        "ALIASES",
                        "CREATED AT",
                    ],
                    response
                        .items
                        .iter()
                        .map(|version| {
                            let source = match &version.experiment {
                                Some(experiment) => format!(
                                    "{} (experiment {})",
                                    source_name(version.source_kind),
                                    experiment.experiment_num
                                ),
                                None => source_name(version.source_kind).into(),
                            };
                            vec![
                                format!("v{}", version.version),
                                state_name(version.state).into(),
                                source,
                                version.size.to_string(),
                                version.aliases.join(", "),
                                version.created_at.clone(),
                            ]
                        })
                        .collect(),
                )?;
            }
            Ok(serde_json::to_value(response)?)
        }
        ModelsCommands::Pull(args) => {
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
            let files = download_files(&client, &directory, &files, args.force)?;
            let bytes: u64 = files.iter().map(|file| file.bytes).sum();
            if human {
                writeln!(
                    std::io::stdout().lock(),
                    "Downloaded '{}' v{} to {}.",
                    args.model,
                    version.version,
                    directory.display()
                )?;
            }
            Ok(
                json!({"model": args.model, "version": version, "directory": directory, "files": files, "bytes": bytes}),
            )
        }
        ModelsCommands::Push(args) => push(
            args,
            &context,
            &client,
            project,
            files.expect("Push files were collected"),
        ),
        ModelsCommands::Promote(args) => {
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
                &context,
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
            if human {
                writeln!(
                    std::io::stdout().lock(),
                    "Promoted artifact '{}' to '{}' v{}.",
                    artifact.name,
                    args.model,
                    version.version
                )?;
            }
            Ok(serde_json::to_value(version)?)
        }
        ModelsCommands::Alias(args) => match args.command {
            AliasCommands::List(args) => {
                let response = client
                    .list_model_aliases(namespace, name, &args.model)
                    .map_err(|error| {
                        map_resource_error(error, namespace, name, Resource::Model(&args.model))
                    })?;
                if human {
                    write_table(
                        &mut std::io::stdout().lock(),
                        &["ALIAS", "VERSION"],
                        response
                            .items
                            .iter()
                            .map(|alias| vec![alias.alias.clone(), format!("v{}", alias.version)])
                            .collect(),
                    )?;
                }
                Ok(serde_json::to_value(response)?)
            }
            AliasCommands::Set(args) => {
                let response = client
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
                if human {
                    writeln!(
                        std::io::stdout().lock(),
                        "{}: v{}",
                        response.alias,
                        response.version
                    )?;
                }
                Ok(serde_json::to_value(response)?)
            }
            AliasCommands::Remove(args) => {
                client
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
                if human {
                    writeln!(
                        std::io::stdout().lock(),
                        "Removed alias '{}' from '{}'.",
                        args.alias,
                        args.model
                    )?;
                }
                Ok(json!({"model": args.model, "alias": args.alias, "removed": true}))
            }
        },
    }
}

fn push(
    args: PushArgs,
    context: &CliContext,
    client: &Client,
    project: TracelProject,
    files: BTreeMap<String, PathBuf>,
) -> anyhow::Result<Value> {
    let namespace = project.owner;
    let project = project.name;
    context
        .terminal()
        .print(&format!("Uploading to {namespace}/{project}"));

    ensure_model_exists(
        context,
        client,
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
        .request_model_version_upload(&namespace, &project, &args.model_name, upload_request)
        .map_err(|e| {
            spinner.error("Failed to request upload URLs.");
            map_resource_error(e, &namespace, &project, Resource::Model(&args.model_name))
        })?;
    spinner.stop(format!("Allocated model version {}.", upload.version));

    let tasks = build_part_tasks(&files, &file_sizes, &upload.files)?;
    upload_parts(client, tasks, context.terminal())?;

    let map_error = |error| {
        map_resource_error(
            error,
            &namespace,
            &project,
            Resource::Model(&args.model_name),
        )
    };
    client
        .complete_model_version_upload(&namespace, &project, &args.model_name, upload.version)
        .map_err(map_error)?;
    let version = client
        .get_model_version(&namespace, &project, &args.model_name, upload.version)
        .map_err(map_error)?;

    context.terminal().print_success(&format!(
        "Uploaded model '{}' version {} to {}/{}.",
        args.model_name, upload.version, namespace, project
    ));
    context
        .terminal()
        .finalize("Model version uploaded successfully.");

    Ok(serde_json::to_value(version)?)
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

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
