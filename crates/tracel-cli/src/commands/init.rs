use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::{can_initialize_project, require_cargo_workspace, require_linked_project};
use crate::tools::project_context::ProjectContext;
use crate::tools::tracel_config::TracelProject;
use crate::ui::{Declined, Outcome, Terminal, Unanswered};
use anyhow::Context;
use clap::Args;
use serde::Serialize;
use tracel_client::console::Client;
use tracel_client::console::project::request::Visibility;
use tracel_client::console::project::response::ProjectResponse;

#[derive(Args, Debug)]
pub struct InitArgs {
    /// Relink a workspace that is already linked
    #[arg(long, short = 'f')]
    pub force: bool,
    /// Own the project as yourself or an organization
    #[arg(long, value_name = "NAMESPACE")]
    pub owner: Option<String>,
    /// Use this project name
    #[arg(long, value_name = "PROJECT")]
    pub name: Option<String>,
    /// Describe a new project
    #[arg(long, value_name = "TEXT")]
    pub description: Option<String>,
    /// Link an existing project without asking
    #[arg(long, short = 'y')]
    pub yes: bool,
}

/// The project the workspace is linked to. `url` is null when it already was.
#[derive(Serialize)]
struct ProjectLinked {
    namespace: String,
    name: String,
    created: bool,
    url: Option<String>,
}

const NOTHING_LINKED: &str = "Nothing was linked.";

pub fn handle_command(args: InitArgs, context: CliContext) -> anyhow::Result<Outcome> {
    if let Some(name) = &args.name {
        validate_project_name(name).map_err(|message| CliError::new(ErrorKind::Usage, message))?;
    }
    if !can_initialize_project(args.force)? {
        return already_linked(&args, &context);
    }
    let client = super::login::get_client_and_login_if_needed(&context)?;
    Ok(Outcome::data(link(args, &context, &client)?))
}

/// End on the project the workspace is linked to, unless the flags ask for another.
fn already_linked(args: &InitArgs, context: &CliContext) -> anyhow::Result<Outcome> {
    let project = require_linked_project()?;
    let linked = project.get_project();
    let other_namespace = args.owner.as_deref().is_some_and(|o| o != linked.namespace);
    let other_name = args.name.as_deref().is_some_and(|n| n != linked.name);
    let name = format!("{}/{}", linked.namespace, linked.name);
    if other_namespace || other_name {
        return Err(CliError::new(
            ErrorKind::Conflict,
            format!("This workspace is already linked to {name}."),
        )
        .with_hint("Pass --force to link another project.")
        .into());
    }
    context
        .terminal()
        .info("Pass --force to link another project.");
    context
        .terminal()
        .outro(&format!("Already linked to {name}."));
    Ok(Outcome::data(ProjectLinked {
        namespace: linked.namespace.clone(),
        name: linked.name.clone(),
        created: false,
        url: None,
    }))
}

/// Link the workspace to the project the flags or prompts choose, creating it when the
/// Tracel Console has none by that name.
fn link(args: InitArgs, context: &CliContext, client: &Client) -> anyhow::Result<ProjectLinked> {
    let user = client.get_current_user()?;
    let workspace_info = require_cargo_workspace()?;
    let terminal = context.terminal();

    let namespace = choose_namespace(
        &user.username,
        &user.namespace,
        args.owner.as_deref(),
        client,
        terminal,
    )?;
    let name = match args.name {
        Some(name) => name,
        None => prompt_project_name(&workspace_info.workspace_name, terminal)?,
    };

    let (project, created) = match client.get_project(namespace.name(), &name) {
        Ok(project) => (
            confirm_existing_project(&project, terminal, args.yes)?,
            false,
        ),
        Err(e) if e.is_not_found() => (
            create_project(client, &namespace, &name, args.description, terminal)?,
            true,
        ),
        Err(e) => {
            return Err(e).with_context(|| format!("Can't look up {}/{name}", namespace.name()));
        }
    };

    ProjectContext::init(project.clone(), &workspace_info.get_manifest_path())?;

    let url = context.web_console().join(&namespace.project_path(&name))?;
    let shown = format!("{}/{}", project.namespace, project.name);
    let link = terminal.format_url(&url);
    let outcome = if created {
        format!("Created and linked {shown} at {link}.")
    } else {
        format!("Linked to {shown} at {link}.")
    };
    terminal.next_steps(&outcome, &["tracel run --list".to_string()]);

    Ok(ProjectLinked {
        namespace: project.namespace,
        name: project.name,
        created,
        url: Some(url.to_string()),
    })
}

/// A namespace that can own a project: the user's own or one of their organizations'.
#[derive(Clone, PartialEq, Eq)]
enum Namespace {
    User(String),
    Organization(String),
}

impl Namespace {
    fn name(&self) -> &str {
        match self {
            Self::User(name) | Self::Organization(name) => name,
        }
    }

    /// Build the web Console path of project `name`.
    fn project_path(&self, name: &str) -> String {
        match self {
            Self::User(namespace) => format!("/users/{namespace}/projects/{name}"),
            Self::Organization(namespace) => format!("/orgs/{namespace}/projects/{name}"),
        }
    }
}

/// Choose the namespace `--owner` names, else ask the user.
fn choose_namespace(
    username: &str,
    user_namespace: &str,
    owner: Option<&str>,
    client: &Client,
    terminal: &Terminal,
) -> anyhow::Result<Namespace> {
    let organizations = client.get_user_organizations()?.organizations;
    let mut namespaces = vec![(
        Namespace::User(user_namespace.to_string()),
        format!("{username} (you)"),
        "",
    )];
    namespaces.extend(organizations.iter().map(|organization| {
        (
            Namespace::Organization(organization.namespace.clone()),
            organization.name.clone(),
            "",
        )
    }));
    let names: Vec<&str> = namespaces
        .iter()
        .map(|(namespace, _, _)| namespace.name())
        .collect();
    if let Some(owner) = owner {
        return namespaces
            .iter()
            .find(|(namespace, _, _)| namespace.name() == owner)
            .map(|(namespace, _, _)| namespace.clone())
            .ok_or_else(|| unknown_owner(owner, &names).into());
    }
    terminal.select(
        "Owner",
        &namespaces,
        None,
        Unanswered::new(
            "Linking needs an owner.",
            format!("Pass --owner with {}.", names.join(" or ")),
        ),
    )
}

fn unknown_owner(owner: &str, namespaces: &[&str]) -> CliError {
    CliError::new(
        ErrorKind::Usage,
        format!(
            "Unknown owner '{owner}'. Owners: {}.",
            namespaces.join(", ")
        ),
    )
}

fn validate_project_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        Err("A project name can't be empty.".to_string())
    } else if name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        Ok(())
    } else {
        Err("Project names use only letters, digits, '_' and '-'.".to_string())
    }
}

/// Ask for a project name, `workspace_name` by default.
fn prompt_project_name(workspace_name: &str, terminal: &Terminal) -> anyhow::Result<String> {
    let input = terminal.input_validated(
        "Project name",
        workspace_name,
        Unanswered::new("Linking needs a project name.", "Pass --name <project>."),
        |input| {
            if input.is_empty() {
                Ok(())
            } else {
                validate_project_name(input)
            }
        },
    )?;
    let name = if input.is_empty() {
        workspace_name.to_string()
    } else {
        input
    };
    validate_project_name(&name).map_err(|message| CliError::new(ErrorKind::Usage, message))?;
    Ok(name)
}

/// Link `project`, which already exists, once the user agrees or `yes` says so.
fn confirm_existing_project(
    project: &ProjectResponse,
    terminal: &Terminal,
    yes: bool,
) -> anyhow::Result<TracelProject> {
    let name = format!("{}/{}", project.namespace_name, project.project_name);
    let confirmed = yes
        || terminal.confirm(
            &format!("Link existing project {name}?"),
            false,
            Unanswered::new(
                format!("Linking {name} needs confirmation."),
                "Pass --yes to link it without asking.",
            ),
        )?;
    if !confirmed {
        return Err(Declined::new(NOTHING_LINKED).into());
    }
    Ok(TracelProject {
        namespace: project.namespace_name.clone(),
        name: project.project_name.clone(),
    })
}

fn create_project(
    client: &Client,
    namespace: &Namespace,
    name: &str,
    description: Option<String>,
    terminal: &Terminal,
) -> anyhow::Result<TracelProject> {
    let description = match description {
        Some(description) => description,
        None if terminal.is_interactive() => terminal.input(
            "Description (optional)",
            Unanswered::new(
                format!("Creating {}/{name} needs a description.", namespace.name()),
                "Pass --description <text>.",
            ),
        )?,
        None => String::new(),
    };
    let description = (!description.is_empty()).then_some(description);

    let created = match namespace {
        Namespace::User(_) => {
            client.create_user_project(name, description.as_deref(), Visibility::Private)
        }
        Namespace::Organization(organization) => client.create_organization_project(
            organization,
            name,
            description.as_deref(),
            Visibility::Private,
        ),
    };
    let project =
        created.with_context(|| format!("Can't create project {}/{name}", namespace.name()))?;
    Ok(TracelProject {
        namespace: project.namespace_name,
        name: project.project_name,
    })
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::letters_and_digits("Project123")]
    #[case::underscore_and_hyphen("my_project-1")]
    #[case::accented_letter("modèle")]
    fn given_letters_digits_underscores_or_hyphens_when_validating_a_project_name_then_it_is_valid(
        #[case] name: &str,
    ) {
        let validated = validate_project_name(name);

        assert_eq!(validated, Ok(()));
    }

    #[rstest]
    #[case::empty("")]
    #[case::space("my project")]
    #[case::dot("project.name")]
    #[case::slash("project/name")]
    fn given_another_character_when_validating_a_project_name_then_it_is_invalid(
        #[case] name: &str,
    ) {
        let validated = validate_project_name(name);

        assert!(validated.is_err());
    }
}
