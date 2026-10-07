use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::{can_initialize_project, require_cargo_workspace, require_linked_project};
use crate::tools::project_context::ProjectContext;
use crate::tools::tracel_config::TracelProject;
use crate::ui::{Outcome, Render, Terminal};
use anyhow::Context;
use clap::Args;
use serde::Serialize;
use tracel_client::console::Client;
use tracel_client::console::project::request::Visibility;
use tracel_client::console::project::response::ProjectResponse;

#[derive(Args, Debug)]
pub struct InitArgs {
    /// Force reinitialization of the project
    #[arg(long, short = 'f')]
    pub force: bool,
    /// Namespace of your user or one of your organizations
    #[arg(long, value_name = "NAMESPACE")]
    pub owner: Option<String>,
    /// Project name (alphanumeric characters, underscores, and hyphens)
    #[arg(long, value_name = "PROJECT")]
    pub name: Option<String>,
    /// Description for a new project (use an empty string for none)
    #[arg(long, value_name = "TEXT")]
    pub description: Option<String>,
    /// Link an existing project without asking for confirmation
    #[arg(long, short = 'y')]
    pub yes: bool,
}

/// The project the repository is linked to. `url` is null when it already was.
#[derive(Serialize)]
struct ProjectLinked {
    namespace: String,
    name: String,
    created: bool,
    url: Option<String>,
}

impl Render for ProjectLinked {}

pub fn handle_command(args: InitArgs, context: CliContext) -> anyhow::Result<Outcome> {
    if let Some(name) = &args.name {
        validate_project_name(name).map_err(|message| {
            CliError::new(ErrorKind::Usage, message).with_hint("Pass --name <project>.")
        })?;
    }
    if !can_initialize_project(&context, args.force)? {
        let project = require_linked_project()?;
        let linked = project.get_project();
        let other_owner = args.owner.as_deref().is_some_and(|o| o != linked.owner);
        let other_name = args.name.as_deref().is_some_and(|n| n != linked.name);
        if other_owner || other_name {
            return Err(CliError::new(
                ErrorKind::Conflict,
                format!(
                    "This repository is already linked to {}/{}.",
                    linked.owner, linked.name
                ),
            )
            .with_hint("Pass --force to link it to another project.")
            .into());
        }
        return Ok(ProjectLinked {
            namespace: linked.owner.clone(),
            name: linked.name.clone(),
            created: false,
            url: None,
        }
        .into());
    }

    let client = super::login::get_client_and_login_if_needed(&context)?;
    let linked =
        prompt_init(args, &context, &client).context("Failed to initialize the project")?;
    Ok(linked.into())
}

fn prompt_init(
    args: InitArgs,
    context: &CliContext,
    client: &Client,
) -> anyhow::Result<ProjectLinked> {
    let user = client.get_current_user()?;
    let workspace_info = require_cargo_workspace()?;

    context.terminal().command_title("Project Initialization");

    let terminal = context.terminal();

    let project_owner = prompt_owner_name(
        &user.username,
        &user.namespace,
        args.owner.as_deref(),
        client,
        terminal,
    )?;
    let project_name = match args.name {
        Some(name) => name,
        None => prompt_project_name(&workspace_info.workspace_name, terminal)?,
    };

    let owner_name = match &project_owner {
        ProjectKind::User => user.namespace.as_str(),
        ProjectKind::Organization(org_name) => org_name.as_str(),
    };
    let (project_info, created) = match client.get_project(owner_name, &project_name) {
        Ok(project) => (
            handle_existing_project(&project, terminal, args.yes)?,
            false,
        ),
        Err(e) if e.is_not_found() => (
            create_new_project(
                client,
                project_owner.clone(),
                &project_name,
                args.description,
                terminal,
            )?,
            true,
        ),
        Err(e) => return Err(e).context("Failed to check for an existing project"),
    };

    ProjectContext::init(project_info.clone(), &workspace_info.get_manifest_path())
        .context("Failed to initialize project metadata")?;
    terminal.print("Created project metadata");

    let frontend_url = project_url(context, &user.username, &project_owner, &project_name)?;

    terminal.finalize(&format!(
        "Project initialized successfully! You can check out your project at {}",
        context.terminal().format_url(&frontend_url)
    ));

    Ok(ProjectLinked {
        namespace: project_info.owner,
        name: project_info.name,
        created,
        url: Some(frontend_url.to_string()),
    })
}

fn prompt_owner_name(
    user_name: &str,
    user_namespace: &str,
    owner: Option<&str>,
    client: &Client,
    terminal: &Terminal,
) -> anyhow::Result<ProjectKind> {
    let organizations = client.get_user_organizations()?;
    let mut valid_values = vec![user_namespace];
    valid_values.extend(
        organizations
            .organizations
            .iter()
            .map(|org| org.namespace.as_str()),
    );
    if let Some(owner) = owner {
        validate_owner(owner, &valid_values)?;
        return Ok(if owner == user_namespace {
            ProjectKind::User
        } else {
            ProjectKind::Organization(owner.to_string())
        });
    }
    let mut namespaces = vec![(ProjectKind::User, format!("[user] {user_name}"), "")];
    namespaces.extend(organizations.organizations.iter().map(|org| {
        (
            ProjectKind::Organization(org.namespace.clone()),
            format!("[org] {}", org.name),
            "",
        )
    }));
    terminal.select(
        "Select the owner of the project",
        "owner",
        &namespaces,
        Some(ProjectKind::User),
        &valid_values,
    )
}

fn validate_owner(owner: &str, valid_values: &[&str]) -> Result<(), CliError> {
    if valid_values.contains(&owner) {
        Ok(())
    } else {
        Err(CliError::new(
            ErrorKind::Usage,
            format!(
                "Invalid --owner '{owner}'. Valid values: {}.",
                valid_values.join(", ")
            ),
        )
        .with_hint("Pass --owner <namespace> using one of the valid values."))
    }
}

fn validate_project_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        Err("Project name cannot be empty.".to_string())
    } else if name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        Ok(())
    } else {
        Err(
            "Project name must contain only alphanumeric characters, underscores, or hyphens."
                .to_string(),
        )
    }
}

pub fn prompt_project_name(workspace_name: &str, terminal: &Terminal) -> anyhow::Result<String> {
    let input = terminal.input_validated(
        &format!(
            "Enter the project name (default: {}) ",
            console::style(workspace_name).bold()
        ),
        "name",
        workspace_name,
        |input| {
            if input.is_empty() {
                Ok(())
            } else {
                validate_project_name(input)
            }
        },
    )?;

    let input = if input.is_empty() {
        workspace_name.to_string()
    } else {
        input
    };

    validate_project_name(&input).map_err(|message| CliError::new(ErrorKind::Usage, message))?;
    Ok(input)
}

fn handle_existing_project(
    project: &ProjectResponse,
    terminal: &Terminal,
    yes: bool,
) -> anyhow::Result<TracelProject> {
    let confirmed = yes
        || terminal.confirm(
            &format!(
                "Project \"{}\" already exists under owner \"{}\". Do you want to link it?",
                project.project_name, project.namespace_name
            ),
            "yes",
            false,
        )?;

    if confirmed {
        Ok(TracelProject {
            owner: project.namespace_name.clone(),
            name: project.project_name.clone(),
        })
    } else {
        Err(anyhow::anyhow!("Project initialization cancelled by user"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProjectKind {
    User,
    Organization(String),
}

fn project_url(
    context: &CliContext,
    username: &str,
    owner: &ProjectKind,
    name: &str,
) -> anyhow::Result<url::Url> {
    let path = match owner {
        ProjectKind::User => format!("/users/{username}/projects/{name}"),
        ProjectKind::Organization(namespace) => format!("/orgs/{namespace}/projects/{name}"),
    };
    Ok(context.get_frontend_endpoint().join(&path)?)
}

fn create_new_project(
    client: &Client,
    project_kind: ProjectKind,
    name: &str,
    description: Option<String>,
    terminal: &Terminal,
) -> anyhow::Result<TracelProject> {
    let description = match description {
        Some(description) => description,
        None if terminal.is_interactive() => terminal.input(
            "Enter the project description (default empty)",
            "description",
        )?,
        None => String::new(),
    };
    let desc = if description.is_empty() {
        None
    } else {
        Some(description)
    };

    let created_project_path = match project_kind {
        ProjectKind::User => client.create_user_project(name, desc.as_deref(), Visibility::Private),
        ProjectKind::Organization(org_name) => client.create_organization_project(
            &org_name,
            name,
            desc.as_deref(),
            Visibility::Private,
        ),
    };

    let project = created_project_path.context("Failed to create project")?;
    Ok(TracelProject {
        owner: project.namespace_name,
        name: project.project_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_must_match_a_namespace() {
        let valid = ["user-namespace", "org-namespace"];
        for owner in valid {
            assert!(validate_owner(owner, &valid).is_ok());
        }
        let error = validate_owner("display-name", &valid).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(error.to_string().contains("user-namespace, org-namespace"));
    }

    #[test]
    fn project_name_allows_alphanumeric_underscores_and_hyphens() {
        for name in ["project", "Project123", "my_project-1", "modèle"] {
            assert!(validate_project_name(name).is_ok());
        }
        for name in ["", "my project", "project.name", "project/name"] {
            assert!(validate_project_name(name).is_err());
        }
        assert!(
            validate_project_name("project.name")
                .unwrap_err()
                .contains("hyphens")
        );
    }
}
