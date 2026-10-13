use std::io;

use clap::{Args, Subcommand};
use tracel_client::console::dataset::request::{QueryDatasetVersionsRequest, QueryDatasetsRequest};
use tracel_client::console::dataset::response::{
    DatasetListResponse, DatasetResponse, DatasetVersionListResponse, SourceKindResponse,
};

use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::helpers::{Resource, map_resource_error, resolve_namespace_project};
use crate::ui::{Details, Human, Outcome, Render, Table, list_summary, when};

#[derive(Args, Debug)]
pub struct DatasetsArgs {
    #[command(subcommand)]
    pub command: DatasetsCommands,
}

#[derive(Subcommand, Debug)]
pub enum DatasetsCommands {
    /// List the project's datasets
    List(ListArgs),
    /// Show a dataset and its metadata
    Get(GetArgs),
    /// List a dataset's published versions
    Versions(VersionsArgs),
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Show this page, counting from 0
    #[arg(long, value_name = "N")]
    pub page: Option<u32>,
    /// Show this many datasets per page
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub per_page: Option<u32>,
}

#[derive(Args, Debug)]
pub struct GetArgs {
    /// Dataset name
    #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub dataset: String,
}

#[derive(Args, Debug)]
pub struct VersionsArgs {
    /// Dataset name
    #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub dataset: String,
    /// Show this page, counting from 0
    #[arg(long, value_name = "N")]
    pub page: Option<u32>,
    /// Show this many versions per page
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub per_page: Option<u32>,
}

pub fn handle_command(args: DatasetsArgs, context: CliContext) -> anyhow::Result<Outcome> {
    let project = resolve_namespace_project(&context)?.project;
    let client = get_client_and_login_if_needed(&context)?;
    match args.command {
        DatasetsCommands::List(args) => Ok(client
            .query_datasets(
                &project.namespace,
                &project.name,
                QueryDatasetsRequest {
                    page: args.page,
                    per_page: args.per_page,
                },
            )?
            .into()),
        DatasetsCommands::Get(args) => Ok(client
            .get_dataset(&project.namespace, &project.name, &args.dataset)
            .map_err(|error| {
                map_resource_error(
                    error,
                    &project.namespace,
                    &project.name,
                    Resource::Dataset(&args.dataset),
                )
            })?
            .into()),
        DatasetsCommands::Versions(args) => Ok(client
            .query_dataset_versions(
                &project.namespace,
                &project.name,
                &args.dataset,
                QueryDatasetVersionsRequest {
                    page: args.page,
                    per_page: args.per_page,
                },
            )
            .map_err(|error| {
                map_resource_error(
                    error,
                    &project.namespace,
                    &project.name,
                    Resource::Dataset(&args.dataset),
                )
            })?
            .into()),
    }
}

impl Render for DatasetListResponse {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        Table::new(["NAME", "DESCRIPTION"])
            .shrink("DESCRIPTION")
            .rows(self.items.iter().map(|dataset| {
                [
                    dataset.name.clone(),
                    dataset.description.clone().unwrap_or_default(),
                ]
            }))
            .write(out)
    }

    fn summary(&self) -> Option<String> {
        Some(list_summary(
            self.items.len(),
            self.total_count,
            ["dataset", "datasets"],
            "No datasets in this project yet.",
        ))
    }
}

impl Render for DatasetResponse {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        Details::new()
            .field("Name", &self.name)
            .optional("Description", self.description.as_ref())
            .section("Metadata", &self.metadata)
            .write(out)
    }
}

impl Render for DatasetVersionListResponse {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        Table::new(["VERSION", "ITEMS", "SOURCE", "CREATED AT"])
            .rows(self.items.iter().map(|version| {
                let source = match version.source_kind {
                    SourceKindResponse::AnnotationSet => "annotation set",
                    SourceKindResponse::DirectUpload => "direct upload",
                };
                [
                    version.version.to_string(),
                    version.item_count.to_string(),
                    source.into(),
                    when(&version.created_at),
                ]
            }))
            .write(out)
    }

    fn summary(&self) -> Option<String> {
        Some(list_summary(
            self.items.len(),
            self.total_count,
            ["version", "versions"],
            "No published versions yet.",
        ))
    }
}
