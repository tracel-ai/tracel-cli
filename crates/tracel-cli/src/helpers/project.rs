//! Finding the workspace and the project a command works on.

use std::path::Path;

use anyhow::Context;
use serde::Serialize;

use crate::{
    context::CliContext,
    error::{CliError, ErrorKind as CliErrorKind},
    tools::{
        cargo::try_locate_manifest,
        project_context::{ErrorKind, ProjectContext, ProjectContextError},
        tracel_config::{TracelProject, TracelToml},
        workspace::WorkspaceInfo,
    },
};
use tracel_client::console::Client;

fn find_manifest() -> anyhow::Result<std::path::PathBuf> {
    try_locate_manifest().ok_or_else(|| {
        CliError::new(CliErrorKind::Usage, "Not in a Cargo workspace.")
            .with_hint("Run it in your project's directory, or pass -C <DIR>.")
            .into()
    })
}

fn is_tracel_project_linked() -> bool {
    let manifest_path = find_manifest();
    match manifest_path {
        Err(_) => false,
        Ok(p) => ProjectContext::load(&p).is_ok(),
    }
}

/// Turn a workspace error into one that says what to do next.
fn explain_project_context_error(e: ProjectContextError) -> anyhow::Error {
    let (kind, hint) = match e.kind() {
        ErrorKind::ManifestNotFound => (
            CliErrorKind::Usage,
            Some("Run it in your project's directory, or pass -C <DIR>."),
        ),
        ErrorKind::ProjectNotLinked => (
            CliErrorKind::NotFound,
            Some("Run `tracel init` to link it."),
        ),
        ErrorKind::Parsing => (CliErrorKind::Usage, None),
        ErrorKind::ProjectInitialization => (
            CliErrorKind::Internal,
            Some("Link it again with `tracel init --force`."),
        ),
        ErrorKind::Unexpected => return e.into(),
    };

    let mut message = e.to_string();
    let mut cause = std::error::Error::source(&e);
    while let Some(err) = cause {
        message.push_str(&format!(": {err}"));
        cause = err.source();
    }

    let error = CliError::new(kind, message);
    match hint {
        Some(hint) => error.with_hint(hint).into(),
        None => error.into(),
    }
}

/// Load the workspace and the project it is linked to.
pub fn require_linked_project() -> anyhow::Result<ProjectContext> {
    let manifest_path = find_manifest()?;
    ProjectContext::load(&manifest_path).map_err(explain_project_context_error)
}

pub fn parse_project(value: &str) -> Result<TracelProject, CliError> {
    if let Some((namespace, project)) = value.split_once('/') {
        if !namespace.is_empty() && !project.is_empty() && !project.contains('/') {
            return Ok(TracelProject {
                namespace: namespace.to_owned(),
                name: project.to_owned(),
            });
        }
    }

    Err(CliError::new(
        CliErrorKind::Usage,
        format!("--project needs <namespace>/<name>, not '{value}'."),
    ))
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub enum ProjectSource {
    #[serde(rename = "flag")]
    Flag,
    #[serde(rename = "env")]
    Env,
    #[serde(rename = "tracel.toml")]
    TracelToml,
}

impl std::fmt::Display for ProjectSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Flag => "--project",
            Self::Env => "TRACEL_NAMESPACE or TRACEL_PROJECT",
            Self::TracelToml => "tracel.toml",
        })
    }
}

#[derive(Debug)]
pub struct ResolvedProject {
    pub project: TracelProject,
    pub source: ProjectSource,
}

fn project_not_found() -> CliError {
    CliError::new(CliErrorKind::NotFound, "No project here.")
        .with_hint("Run `tracel init`, or pass --project <namespace>/<name>.")
}

/// Build the error for a project the Tracel Console doesn't have, with a hint that fits
/// how it was selected.
pub fn missing_project(project: &TracelProject, source: &ProjectSource) -> CliError {
    let error = CliError::new(
        CliErrorKind::NotFound,
        format!(
            "No project {}/{} on the Tracel Console.",
            project.namespace, project.name
        ),
    );
    match source {
        ProjectSource::TracelToml => {
            error.with_hint("Run `tracel init --force` to link another project.")
        }
        ProjectSource::Flag | ProjectSource::Env => {
            error.with_hint("List your projects with `tracel project list`.")
        }
    }
}

/// Find the project a command works on.
pub fn resolve_namespace_project(context: &CliContext) -> anyhow::Result<ResolvedProject> {
    resolve_project(context.project())
}

/// Find the project `flag` names, else the one TRACEL_NAMESPACE and TRACEL_PROJECT
/// name, else `tracel.toml`'s. The variables each override their key.
pub fn resolve_project(flag: Option<&TracelProject>) -> anyhow::Result<ResolvedProject> {
    resolve_project_from(
        flag,
        std::env::var("TRACEL_NAMESPACE").ok(),
        std::env::var("TRACEL_PROJECT").ok(),
        || {
            let manifest_path = try_locate_manifest().ok_or_else(project_not_found)?;
            let workspace_root = manifest_path.parent().ok_or_else(project_not_found)?;
            Ok(read_tracel_toml(workspace_root)?)
        },
    )
}

/// Read the `tracel.toml` of `workspace_root`, empty when there is none.
fn read_tracel_toml(workspace_root: &Path) -> Result<TracelToml, CliError> {
    TracelToml::load(workspace_root)
        .map(Option::unwrap_or_default)
        .map_err(|error| {
            CliError::new(
                CliErrorKind::Usage,
                format!(
                    "Can't read '{}': {error}",
                    TracelProject::path(workspace_root).display()
                ),
            )
        })
}

fn resolve_project_from(
    flag: Option<&TracelProject>,
    namespace: Option<String>,
    project: Option<String>,
    load_config: impl FnOnce() -> anyhow::Result<TracelToml>,
) -> anyhow::Result<ResolvedProject> {
    if let Some(project) = flag {
        return Ok(ResolvedProject {
            project: project.clone(),
            source: ProjectSource::Flag,
        });
    }

    let source = if namespace.is_some() || project.is_some() {
        ProjectSource::Env
    } else {
        ProjectSource::TracelToml
    };
    let config = if namespace.is_some() && project.is_some() {
        TracelToml::default()
    } else {
        load_config()?
    };

    Ok(ResolvedProject {
        project: TracelProject {
            namespace: namespace
                .or(config.namespace)
                .ok_or_else(project_not_found)?,
            name: project.or(config.project).ok_or_else(project_not_found)?,
        },
        source,
    })
}

/// Load the Cargo workspace around the current directory, linked or not.
pub fn require_cargo_workspace() -> anyhow::Result<WorkspaceInfo> {
    let manifest_path = find_manifest()?;
    ProjectContext::load_workspace_info(&manifest_path).map_err(explain_project_context_error)
}

/// Load the Cargo workspace, with the project a command works on and how it was
/// selected.
pub fn require_workspace_project(
    context: &CliContext,
) -> anyhow::Result<(ProjectContext, ProjectSource)> {
    let workspace_info = require_cargo_workspace()?;
    let resolved = resolve_namespace_project(context)?;
    let project = ProjectContext {
        workspace_info,
        project: resolved.project,
    };
    Ok((project, resolved.source))
}

/// Decide whether `tracel init` links: not when the workspace is linked and `force` is
/// off. Fails outside a workspace.
pub fn can_initialize_project(force: bool) -> anyhow::Result<bool> {
    find_manifest()?;
    Ok(force || !is_tracel_project_linked())
}

/// Fail unless the Tracel Console has `project`, selected by `source`.
pub fn validate_project_exists_on_server(
    project: &TracelProject,
    source: &ProjectSource,
    client: &Client,
) -> anyhow::Result<()> {
    match client.get_project(&project.namespace, &project.name) {
        Ok(_) => Ok(()),
        Err(e) if e.is_not_found() => Err(missing_project(project, source).into()),
        Err(e) => {
            Err(e).with_context(|| format!("Can't look up {}/{}", project.namespace, project.name))
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::error::{ErrorReport, classify};

    fn tracel_toml(namespace: Option<&str>, project: Option<&str>) -> TracelToml {
        TracelToml {
            namespace: namespace.map(str::to_string),
            project: project.map(str::to_string),
        }
    }

    fn unread() -> anyhow::Result<TracelToml> {
        panic!("tracel.toml was read")
    }

    fn full_name(project: &TracelProject) -> String {
        format!("{}/{}", project.namespace, project.name)
    }

    #[test]
    fn given_a_namespace_and_a_name_when_parsing_a_project_then_it_has_both() {
        let project = parse_project("alice/demo").unwrap();

        assert_eq!(project.namespace, "alice");
        assert_eq!(project.name, "demo");
    }

    #[rstest]
    #[case::empty("")]
    #[case::no_slash("alice")]
    #[case::no_namespace("/demo")]
    #[case::no_name("alice/")]
    #[case::extra_part("alice/demo/extra")]
    fn given_a_value_that_is_not_namespace_slash_name_when_parsing_a_project_then_it_is_a_usage_error(
        #[case] value: &str,
    ) {
        let error = parse_project(value).unwrap_err();

        assert_eq!(error.kind, CliErrorKind::Usage);
        assert!(error.to_string().contains(&format!("'{value}'")), "{error}");
    }

    #[test]
    fn given_a_project_flag_when_resolving_the_project_then_the_flag_wins() {
        let flag = parse_project("alice/demo").unwrap();

        let resolved =
            resolve_project_from(Some(&flag), Some("env".into()), Some("env".into()), unread)
                .unwrap();

        assert_eq!(full_name(&resolved.project), "alice/demo");
        assert_eq!(resolved.source, ProjectSource::Flag);
    }

    #[test]
    fn given_both_variables_when_resolving_the_project_then_they_name_it_without_tracel_toml() {
        let resolved =
            resolve_project_from(None, Some("alice".into()), Some("demo".into()), unread).unwrap();

        assert_eq!(full_name(&resolved.project), "alice/demo");
        assert_eq!(resolved.source, ProjectSource::Env);
    }

    #[test]
    fn given_no_flag_or_variable_when_resolving_the_project_then_tracel_toml_names_it() {
        let resolved = resolve_project_from(None, None, None, || {
            Ok(tracel_toml(Some("alice"), Some("demo")))
        })
        .unwrap();

        assert_eq!(full_name(&resolved.project), "alice/demo");
        assert_eq!(resolved.source, ProjectSource::TracelToml);
    }

    #[test]
    fn given_only_tracel_namespace_when_resolving_the_project_then_it_overrides_tracel_toml_namespace()
     {
        let resolved = resolve_project_from(None, Some("team".into()), None, || {
            Ok(tracel_toml(Some("alice"), Some("demo")))
        })
        .unwrap();

        assert_eq!(full_name(&resolved.project), "team/demo");
        assert_eq!(resolved.source, ProjectSource::Env);
    }

    #[test]
    fn given_no_project_anywhere_when_resolving_the_project_then_it_is_not_found() {
        let error =
            resolve_project_from(None, None, None, || Ok(tracel_toml(None, None))).unwrap_err();

        assert_eq!(classify(&error), CliErrorKind::NotFound);
        assert!(
            ErrorReport::new(&error)
                .hint
                .unwrap()
                .contains("tracel init")
        );
    }

    #[rstest]
    #[case::flag(ProjectSource::Flag, "tracel project list")]
    #[case::env(ProjectSource::Env, "tracel project list")]
    #[case::tracel_toml(ProjectSource::TracelToml, "tracel init --force")]
    fn given_how_a_project_was_selected_when_the_console_lacks_it_then_the_hint_suggests_the_fitting_command(
        #[case] source: ProjectSource,
        #[case] command: &str,
    ) {
        let project = parse_project("alice/demo").unwrap();

        let error = anyhow::Error::new(missing_project(&project, &source));

        assert!(ErrorReport::new(&error).hint.unwrap().contains(command));
    }
}
