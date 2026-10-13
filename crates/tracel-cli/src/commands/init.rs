use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::{can_initialize_project, require_cargo_workspace, require_linked_project};
use crate::tools::git;
use crate::tools::project_context::ProjectContext;
use crate::tools::tracel_config::TracelProject;
use crate::ui::{Declined, Outcome, Render, Terminal, Unanswered};
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
    /// Commit all current changes first
    #[arg(long, conflicts_with = "allow_dirty")]
    pub commit: bool,
    /// Link with uncommitted changes
    #[arg(long)]
    pub allow_dirty: bool,
}

/// The project the workspace is linked to. `url` is null when it already was.
#[derive(Serialize)]
struct ProjectLinked {
    namespace: String,
    name: String,
    created: bool,
    url: Option<String>,
}

impl Render for ProjectLinked {}

const NOTHING_LINKED: &str = "Nothing was linked.";

pub fn handle_command(args: InitArgs, context: CliContext) -> anyhow::Result<Outcome> {
    if let Some(name) = &args.name {
        validate_project_name(name).map_err(|message| CliError::new(ErrorKind::Usage, message))?;
    }
    if !can_initialize_project(args.force)? {
        let project = require_linked_project()?;
        let linked = project.get_project();
        let other_owner = args.owner.as_deref().is_some_and(|o| o != linked.owner);
        let other_name = args.name.as_deref().is_some_and(|n| n != linked.name);
        if other_owner || other_name {
            return Err(CliError::new(
                ErrorKind::Conflict,
                format!(
                    "This workspace is already linked to {}/{}.",
                    linked.owner, linked.name
                ),
            )
            .with_hint("Pass --force to link another project.")
            .into());
        }
        context
            .terminal()
            .print("Pass --force to link another project.");
        context.terminal().outro(&format!(
            "Already linked to {}/{}.",
            linked.owner, linked.name
        ));
        return Ok(ProjectLinked {
            namespace: linked.owner.clone(),
            name: linked.name.clone(),
            created: false,
            url: None,
        }
        .into());
    }

    let client = super::login::get_client_and_login_if_needed(&context)?;
    Ok(prompt_init(args, &context, &client)?.into())
}

fn prompt_init(
    args: InitArgs,
    context: &CliContext,
    client: &Client,
) -> anyhow::Result<ProjectLinked> {
    let user = client.get_current_user()?;
    let workspace_info = require_cargo_workspace()?;

    let terminal = context.terminal();

    ensure_git_repo_initialized(&workspace_info.get_ws_root(), terminal)?;
    ensure_git_repo_clean(terminal, args.commit, args.allow_dirty)?;

    git::get_first_commit_hash().context("Can't read the first commit")?;

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
                owner_name,
                &project_name,
                args.description,
                terminal,
            )?,
            true,
        ),
        Err(e) => {
            return Err(e).with_context(|| format!("Can't look up {owner_name}/{project_name}"));
        }
    };

    ProjectContext::init(project_info.clone(), &workspace_info.get_manifest_path())?;

    let frontend_url = project_url(context, &project_owner, owner_name, &project_name)?;
    let project = format!("{}/{}", project_info.owner, project_info.name);
    let url = terminal.format_url(&frontend_url);
    terminal.outro(&if created {
        format!("Created and linked {project} at {url}.")
    } else {
        format!("Linked to {project} at {url}.")
    });

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
    let mut namespaces = vec![(ProjectKind::User, format!("{user_name} (you)"), "")];
    namespaces.extend(organizations.organizations.iter().map(|org| {
        (
            ProjectKind::Organization(org.namespace.clone()),
            org.name.clone(),
            "",
        )
    }));
    terminal.select(
        "Owner",
        &namespaces,
        Some(ProjectKind::User),
        Unanswered::new(
            "Linking needs an owner.",
            format!("Pass --owner with {}.", valid_values.join(" or ")),
        ),
    )
}

fn validate_owner(owner: &str, valid_values: &[&str]) -> Result<(), CliError> {
    if valid_values.contains(&owner) {
        Ok(())
    } else {
        Err(CliError::new(
            ErrorKind::Usage,
            format!(
                "Unknown owner '{owner}'. Owners: {}.",
                valid_values.join(", ")
            ),
        ))
    }
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

pub fn prompt_project_name(workspace_name: &str, terminal: &Terminal) -> anyhow::Result<String> {
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

    if confirmed {
        Ok(TracelProject {
            owner: project.namespace_name.clone(),
            name: project.project_name.clone(),
        })
    } else {
        Err(Declined::new(NOTHING_LINKED).into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProjectKind {
    User,
    Organization(String),
}

fn project_url(
    context: &CliContext,
    owner: &ProjectKind,
    namespace: &str,
    name: &str,
) -> anyhow::Result<url::Url> {
    let path = match owner {
        ProjectKind::User => format!("/users/{namespace}/projects/{name}"),
        ProjectKind::Organization(_) => format!("/orgs/{namespace}/projects/{name}"),
    };
    Ok(context.get_frontend_endpoint().join(&path)?)
}

fn create_new_project(
    client: &Client,
    project_kind: ProjectKind,
    namespace: &str,
    name: &str,
    description: Option<String>,
    terminal: &Terminal,
) -> anyhow::Result<TracelProject> {
    let description = match description {
        Some(description) => description,
        None if terminal.is_interactive() => terminal.input(
            "Description (optional)",
            Unanswered::new(
                format!("Creating {namespace}/{name} needs a description."),
                "Pass --description <text>.",
            ),
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

    let project =
        created_project_path.with_context(|| format!("Can't create project {namespace}/{name}"))?;
    Ok(TracelProject {
        owner: project.namespace_name,
        name: project.project_name,
    })
}

pub fn ensure_git_repo_initialized(
    ws_root: &std::path::Path,
    terminal: &Terminal,
) -> anyhow::Result<()> {
    if !git::is_repo_initialized() {
        git::init_repo(ws_root)?;
        terminal.step("Created a git repository");
    }
    Ok(())
}

pub fn ensure_git_repo_clean(
    terminal: &Terminal,
    commit: bool,
    allow_dirty: bool,
) -> anyhow::Result<()> {
    let has_commit = git::get_last_commit_hash().is_ok();
    let dirty = match git::is_repo_dirty() {
        Ok(dirty) => dirty,
        Err(_) if !has_commit => true,
        Err(error) => return Err(error).context("Can't read the git status"),
    };
    if !dirty && has_commit {
        return Ok(());
    }
    if commit {
        return commit_all(terminal);
    }
    if allow_dirty && has_commit {
        return Ok(());
    }
    if allow_dirty {
        return Err(no_commit()
            .with_hint("Pass --commit to make the first one.")
            .into());
    }
    if !terminal.is_interactive() {
        return Err(commit_required().into());
    }
    terminal.print_warning(if has_commit {
        "The workspace has uncommitted changes."
    } else {
        "The workspace has no commit yet."
    });
    if !terminal.confirm(
        "Commit all files?",
        false,
        Unanswered::new("Linking needs a commit.", "Pass --commit or --allow-dirty."),
    )? {
        return Err(Declined::new(NOTHING_LINKED).into());
    }
    commit_all(terminal)
}

fn no_commit() -> CliError {
    CliError::new(ErrorKind::Usage, "The workspace has no commit yet.")
}

fn commit_required() -> CliError {
    CliError::new(ErrorKind::Usage, "The workspace has uncommitted changes.")
        .with_hint("Commit them, or pass --commit or --allow-dirty.")
}

/// Commit everything in the working tree, making the first commit if there is none.
pub fn commit_all(terminal: &Terminal) -> anyhow::Result<()> {
    let commit_message = "Automatic commit by Tracel Console CLI";
    let status = std::process::Command::new("git")
        .args(["add", "--all"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .context("Can't run `git add --all`")?;
    if !status.success() {
        anyhow::bail!("`git add --all` failed.");
    }
    let mut command = std::process::Command::new("git");
    command.args(["commit", "-m", commit_message]);
    if git::get_last_commit_hash().is_err() {
        command.arg("--allow-empty");
    }
    let status = command
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .context("Can't run `git commit`")?;
    if !status.success() {
        anyhow::bail!("`git commit` failed.");
    }
    terminal.step("Committed all files");
    Ok(())
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
                .contains("'_' and '-'")
        );
    }
}
