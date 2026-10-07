use std::io::{self, Write};

use anyhow::Context;
use clap::{Args, Subcommand};
use serde::Serialize;
use tracel_client::console::project::request::Visibility;
use tracel_client::console::project::response::ProjectResponse;

use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::project::ProjectSource;
use crate::helpers::resolve_namespace_project;
use crate::output::{Details, Outcome, Render, Table};

#[derive(Args, Debug)]
pub struct ProjectArgs {
    #[command(subcommand)]
    pub command: Option<ProjectCommands>,
}

#[derive(Subcommand, Debug)]
pub enum ProjectCommands {
    /// List your projects and your organizations' projects.
    List(ListArgs),
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Your namespace or one of your organizations; omit to list all.
    #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub namespace: Option<String>,
}

#[derive(Serialize)]
#[serde(transparent)]
struct Projects(Vec<ProjectResponse>);

impl Render for Projects {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        Table::new(["PROJECT", "VISIBILITY", "DESCRIPTION", "CREATED AT"])
            .rows(self.0.iter().map(|project| {
                [
                    format!("{}/{}", project.namespace_name, project.project_name),
                    visibility_name(&project.visibility).into(),
                    project.description.clone(),
                    project.created_at.clone(),
                ]
            }))
            .write(out)
    }
}

#[derive(Serialize)]
struct ProjectInfo {
    namespace: String,
    name: String,
    description: String,
    created_by: String,
    visibility: Visibility,
    source: ProjectSource,
}

impl Render for ProjectInfo {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        Details::new()
            .field("Project", &self.name)
            .field("Namespace", &self.namespace)
            .field("Description", &self.description)
            .field("Visibility", visibility_name(&self.visibility))
            .field("Created by", &self.created_by)
            .field("Selected by", &self.source)
            .write(out)
    }
}

fn visibility_name(visibility: &Visibility) -> &'static str {
    match visibility {
        Visibility::Private => "private",
        Visibility::Public => "public",
    }
}

pub fn handle_command(args: ProjectArgs, context: CliContext) -> anyhow::Result<Outcome> {
    match args.command {
        Some(ProjectCommands::List(args)) => Ok(list_projects(args, context)?.into()),
        None => Ok(show_project(context)?.into()),
    }
}

fn list_projects(args: ListArgs, context: CliContext) -> anyhow::Result<Projects> {
    let client = get_client_and_login_if_needed(&context)?;
    let user_namespace = &client.user().namespace;
    let mut projects = Vec::new();
    if args
        .namespace
        .as_deref()
        .is_none_or(|namespace| namespace == user_namespace)
    {
        projects.extend(client.list_user_projects(user_namespace)?);
        if args.namespace.is_some() {
            return Ok(Projects(projects));
        }
    }
    let organizations = client.get_user_organizations()?;
    if let Some(namespace) = args.namespace {
        if !organizations
            .organizations
            .iter()
            .any(|organization| organization.namespace == namespace)
        {
            let available = std::iter::once(user_namespace.as_str())
                .chain(
                    organizations
                        .organizations
                        .iter()
                        .map(|organization| organization.namespace.as_str()),
                )
                .collect::<Vec<_>>()
                .join(", ");
            return Err(CliError::new(
                ErrorKind::NotFound,
                format!("Namespace '{namespace}' is not available."),
            )
            .with_hint(format!(
                "Available namespaces: {available}. Use `tracel project list <NAMESPACE>`."
            ))
            .into());
        }
        projects.extend(client.list_organization_projects(&namespace)?);
    } else {
        for organization in organizations.organizations {
            projects.extend(client.list_organization_projects(&organization.namespace)?);
        }
    }
    Ok(Projects(projects))
}

fn show_project(context: CliContext) -> anyhow::Result<ProjectInfo> {
    let resolved = resolve_namespace_project(&context)?;
    let client = get_client_and_login_if_needed(&context)?;

    let bc_project = &resolved.project;
    let project = match client.get_project(&bc_project.owner, &bc_project.name) {
        Ok(project) => project,
        Err(e) if e.is_not_found() => {
            return Err(CliError::new(
                ErrorKind::NotFound,
                format!(
                    "Project {}/{} not found on Tracel Console.",
                    bc_project.owner, bc_project.name
                ),
            )
            .with_hint("Run 'tracel init --force' to link another project.")
            .into());
        }
        Err(e) => {
            return Err(e).with_context(|| {
                format!(
                    "Failed to retrieve project {}/{}",
                    bc_project.owner, bc_project.name
                )
            });
        }
    };

    Ok(ProjectInfo {
        namespace: project.namespace_name,
        name: project.project_name,
        description: project.description,
        created_by: project.created_by,
        visibility: project.visibility,
        source: resolved.source,
    })
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;
    use crate::cli::{CliArgs, Commands};

    #[test]
    fn bare_project_and_list_parse_without_a_selected_project() {
        CliArgs::command().debug_assert();
        let args = CliArgs::try_parse_from(["tracel", "project"]).unwrap();
        let Some(Commands::Project(args)) = args.command else {
            panic!("Expected project");
        };
        assert!(args.command.is_none());
        for namespace in [None, Some("alice"), Some("team")] {
            let mut arguments = vec!["tracel", "project", "list"];
            arguments.extend(namespace);
            let args = CliArgs::try_parse_from(arguments).unwrap();
            assert!(args.project.is_none());
            let Some(Commands::Project(args)) = args.command else {
                panic!("Expected project");
            };
            let Some(ProjectCommands::List(args)) = args.command else {
                panic!("Expected list");
            };
            assert_eq!(args.namespace.as_deref(), namespace);
        }
    }
}
