use std::io::Write;

use clap::{Args, Subcommand};
use serde_json::Value;
use tracel_client::console::dataset::request::{QueryDatasetVersionsRequest, QueryDatasetsRequest};
use tracel_client::console::dataset::response::SourceKindResponse;

use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::helpers::resolve_namespace_project;
use crate::output::{OutputMode, write_table};

#[derive(Args, Debug)]
pub struct DatasetsArgs {
    #[command(subcommand)]
    pub command: DatasetsCommands,
}

#[derive(Subcommand, Debug)]
pub enum DatasetsCommands {
    /// List datasets in the selected project.
    List(ListArgs),
    /// Show a dataset and its metadata.
    Get(GetArgs),
    /// List published dataset versions.
    Versions(VersionsArgs),
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Page number (zero-based).
    #[arg(long, value_name = "N")]
    pub page: Option<u32>,
    /// Maximum datasets per page.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub per_page: Option<u32>,
}

#[derive(Args, Debug)]
pub struct GetArgs {
    #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub dataset: String,
}

#[derive(Args, Debug)]
pub struct VersionsArgs {
    #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub dataset: String,
    /// Page number (zero-based).
    #[arg(long, value_name = "N")]
    pub page: Option<u32>,
    /// Maximum versions per page.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub per_page: Option<u32>,
}

pub fn handle_command(args: DatasetsArgs, context: CliContext) -> anyhow::Result<Value> {
    context.terminal().command_title("Datasets");
    let project = resolve_namespace_project(&context)?.project;
    let client = get_client_and_login_if_needed(&context)?;
    let human = context.output() == OutputMode::Human;
    match args.command {
        DatasetsCommands::List(args) => {
            let response = client.query_datasets(
                &project.owner,
                &project.name,
                QueryDatasetsRequest {
                    page: args.page,
                    per_page: args.per_page,
                },
            )?;
            if human {
                let mut stdout = std::io::stdout().lock();
                write_table(
                    &mut stdout,
                    &["NAME", "DESCRIPTION", "ID"],
                    response
                        .items
                        .iter()
                        .map(|dataset| {
                            vec![
                                dataset.name.clone(),
                                dataset.description.clone().unwrap_or_default(),
                                dataset.id.clone(),
                            ]
                        })
                        .collect(),
                )?;
                let shown = response.items.len() as u64;
                if shown < response.total_count {
                    writeln!(stdout, "Showing {shown} of {}", response.total_count)?;
                }
            }
            Ok(serde_json::to_value(response)?)
        }
        DatasetsCommands::Get(args) => {
            let dataset = client.get_dataset(&project.owner, &project.name, &args.dataset)?;
            if human {
                let mut stdout = std::io::stdout().lock();
                writeln!(stdout, "Name: {}", dataset.name)?;
                writeln!(stdout, "ID: {}", dataset.id)?;
                writeln!(
                    stdout,
                    "Description: {}",
                    dataset.description.as_deref().unwrap_or("")
                )?;
                writeln!(stdout, "Metadata:")?;
                serde_json::to_writer_pretty(&mut stdout, &dataset.metadata)?;
                writeln!(stdout)?;
            }
            Ok(serde_json::to_value(dataset)?)
        }
        DatasetsCommands::Versions(args) => {
            let response = client.query_dataset_versions(
                &project.owner,
                &project.name,
                &args.dataset,
                QueryDatasetVersionsRequest {
                    page: args.page,
                    per_page: args.per_page,
                },
            )?;
            if human {
                write_table(
                    &mut std::io::stdout().lock(),
                    &["VERSION", "ITEMS", "SOURCE", "CREATED AT"],
                    response
                        .items
                        .iter()
                        .map(|version| {
                            let source = match version.source_kind {
                                SourceKindResponse::AnnotationSet => "annotation_set",
                                SourceKindResponse::DirectUpload => "direct_upload",
                            };
                            vec![
                                version.version.to_string(),
                                version.item_count.to_string(),
                                source.into(),
                                version.created_at.clone(),
                            ]
                        })
                        .collect(),
                )?;
            }
            Ok(serde_json::to_value(response)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;
    use crate::cli::{CliArgs, Commands};

    fn parse_command(arguments: &[&str]) -> DatasetsArgs {
        let args = CliArgs::try_parse_from(arguments).unwrap();
        let Some(Commands::Datasets(args)) = args.command else {
            panic!("Expected datasets command");
        };
        args
    }

    #[test]
    fn definitions_and_pagination_are_valid() {
        CliArgs::command().debug_assert();
        let args = parse_command(&[
            "tracel",
            "datasets",
            "list",
            "--page",
            "0",
            "--per-page",
            "25",
        ]);
        let DatasetsCommands::List(args) = args.command else {
            panic!("Expected list");
        };
        assert_eq!(args.page, Some(0));
        assert_eq!(args.per_page, Some(25));
        let args = parse_command(&["tracel", "datasets", "get", "images"]);
        let DatasetsCommands::Get(args) = args.command else {
            panic!("Expected get");
        };
        assert_eq!(args.dataset, "images");
        let args = parse_command(&[
            "tracel",
            "datasets",
            "versions",
            "images",
            "--page",
            "2",
            "--per-page",
            "10",
        ]);
        let DatasetsCommands::Versions(args) = args.command else {
            panic!("Expected versions");
        };
        assert_eq!(args.dataset, "images");
        assert_eq!(args.page, Some(2));
        assert_eq!(args.per_page, Some(10));
        let args = parse_command(&["tracel", "datasets", "list"]);
        let DatasetsCommands::List(args) = args.command else {
            panic!("Expected list");
        };
        assert!(args.page.is_none());
        assert!(args.per_page.is_none());
        for arguments in [
            vec!["tracel", "datasets", "list", "--page", "-1"],
            vec!["tracel", "datasets", "list", "--per-page", "0"],
            vec![
                "tracel",
                "datasets",
                "versions",
                "images",
                "--per-page",
                "0",
            ],
        ] {
            assert!(CliArgs::try_parse_from(arguments).is_err());
        }
    }
}
