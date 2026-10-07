//! Project helpers for CLI operations

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::{
    context::CliContext,
    error::{CliError, ErrorKind as CliErrorKind},
    tools::{
        cargo::try_locate_manifest,
        project_context::{ErrorKind, ProjectContext, ProjectContextError},
        tracel_config::TracelProject,
        workspace::WorkspaceInfo,
    },
};
use tracel_client::console::Client;

pub fn find_manifest() -> anyhow::Result<std::path::PathBuf> {
    try_locate_manifest().ok_or_else(|| {
        CliError::new(
            CliErrorKind::Usage,
            "Could not locate a Cargo.toml manifest.",
        )
        .with_hint("Run this command inside a Burn project directory.")
        .into()
    })
}

/// Check if current directory has a linked Tracel Console project
pub fn is_tracel_project_linked() -> bool {
    let manifest_path = find_manifest();
    match manifest_path {
        Err(_) => false,
        Ok(p) => ProjectContext::load(&p).is_ok(),
    }
}

/// One error that says what went wrong, why, and what to do next.
fn explain_project_context_error(e: ProjectContextError) -> anyhow::Error {
    let (kind, hint) = match e.kind() {
        ErrorKind::ManifestNotFound => (
            CliErrorKind::Usage,
            "Run this command from a Rust project directory.",
        ),
        ErrorKind::ProjectNotLinked => (CliErrorKind::NotFound, "Run 'tracel init' to link it."),
        ErrorKind::Parsing => (
            CliErrorKind::Usage,
            "Check that Cargo.toml and tracel.toml are valid.",
        ),
        ErrorKind::ProjectInitialization => (
            CliErrorKind::Internal,
            "Re-link the project with 'tracel init --force'.",
        ),
        ErrorKind::Unexpected => (CliErrorKind::Internal, "Check your project setup."),
    };

    let mut message = e.to_string();
    let mut cause = std::error::Error::source(&e);
    while let Some(err) = cause {
        message.push_str(&format!(": {err}"));
        cause = err.source();
    }

    CliError::new(kind, format!("{message}."))
        .with_hint(hint)
        .into()
}

/// Require a linked Tracel Console project.
pub fn require_linked_project() -> anyhow::Result<ProjectContext> {
    let manifest_path = find_manifest()?;
    ProjectContext::load(&manifest_path).map_err(explain_project_context_error)
}

pub fn parse_project(value: &str) -> Result<TracelProject, CliError> {
    if let Some((namespace, project)) = value.split_once('/') {
        if !namespace.is_empty() && !project.is_empty() && !project.contains('/') {
            return Ok(TracelProject {
                owner: namespace.to_owned(),
                name: project.to_owned(),
            });
        }
    }

    Err(CliError::new(
        CliErrorKind::Usage,
        format!("Invalid --project value '{value}': expected <namespace>/<name>."),
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

#[derive(Deserialize, Default)]
struct TracelTomlConfig {
    #[serde(alias = "owner")]
    namespace: Option<String>,
    #[serde(alias = "name")]
    project: Option<String>,
}

fn project_not_found() -> CliError {
    CliError::new(
        CliErrorKind::NotFound,
        "No Tracel Console project resolved.",
    )
    .with_hint("Run 'tracel init' or pass --project <namespace>/<name>.")
}

/// Resolve project identity from flags, environment variables, or tracel.toml.
pub fn resolve_namespace_project(context: &CliContext) -> anyhow::Result<ResolvedProject> {
    resolve_project(
        context.project(),
        std::env::var("TRACEL_NAMESPACE").ok(),
        std::env::var("TRACEL_PROJECT").ok(),
        || {
            let manifest_path = try_locate_manifest().ok_or_else(project_not_found)?;
            let workspace_root = manifest_path.parent().ok_or_else(project_not_found)?;
            let path = TracelProject::path(workspace_root);
            let contents = match std::fs::read_to_string(&path) {
                Ok(contents) => contents,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(TracelTomlConfig::default());
                }
                Err(error) => {
                    return Err(CliError::new(
                        CliErrorKind::Usage,
                        format!("Failed to read '{}': {error}", path.display()),
                    )
                    .into());
                }
            };
            toml::from_str(&contents).map_err(|error| {
                CliError::new(
                    CliErrorKind::Usage,
                    format!("Failed to parse '{}': {error}", path.display()),
                )
                .into()
            })
        },
    )
}

fn resolve_project(
    flag: Option<&TracelProject>,
    namespace: Option<String>,
    project: Option<String>,
    load_config: impl FnOnce() -> anyhow::Result<TracelTomlConfig>,
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
        TracelTomlConfig::default()
    } else {
        load_config()?
    };

    Ok(ResolvedProject {
        project: TracelProject {
            owner: namespace
                .or(config.namespace)
                .ok_or_else(project_not_found)?,
            name: project.or(config.project).ok_or_else(project_not_found)?,
        },
        source,
    })
}

/// Require a Cargo workspace (with or without Tracel Console linkage)
pub fn require_cargo_workspace() -> anyhow::Result<WorkspaceInfo> {
    let manifest_path = find_manifest()?;
    ProjectContext::load_workspace_info(&manifest_path).map_err(explain_project_context_error)
}

/// Whether `tracel init` should go ahead. Fails outside a Rust project, and stops
/// without error when the project is already linked and `force` is off.
pub fn can_initialize_project(context: &CliContext, force: bool) -> anyhow::Result<bool> {
    find_manifest()?;

    if is_tracel_project_linked() && !force {
        context
            .terminal()
            .print("Project is already linked to Tracel Console.");
        context
            .terminal()
            .print("Use --force flag to reinitialize.");
        return Ok(false);
    }

    Ok(true)
}

/// Validate that the linked project exists on Tracel Console server
pub fn validate_project_exists_on_server(
    project: &ProjectContext,
    client: &Client,
) -> anyhow::Result<()> {
    let bc_project = project.get_project();

    match client.get_project(&bc_project.owner, &bc_project.name) {
        Ok(_) => Ok(()),
        Err(e) if e.is_not_found() => Err(CliError::new(
            CliErrorKind::NotFound,
            format!(
                "Project {}/{} not found on Tracel Console. It may have been deleted or renamed.",
                bc_project.owner, bc_project.name
            ),
        )
        .with_hint("Run 'tracel init --force' to link another project.")
        .into()),
        Err(e) => Err(e).context("Failed to verify project exists on server"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_flag_requires_two_nonempty_parts() {
        let project = parse_project("alice/demo").unwrap();
        assert_eq!(project.owner, "alice");
        assert_eq!(project.name, "demo");
        for value in [
            "",
            "alice",
            "/demo",
            "alice/",
            "/",
            "alice/demo/extra",
            "alice//demo",
        ] {
            let error = parse_project(value).unwrap_err();
            assert_eq!(error.kind, CliErrorKind::Usage);
            assert!(error.to_string().contains("--project"));
        }
    }

    #[test]
    fn complete_overrides_skip_file_discovery() {
        let flag = parse_project("alice/demo").unwrap();
        let resolved = resolve_project(
            Some(&flag),
            Some("other".into()),
            Some("project".into()),
            || panic!("File discovery must be skipped"),
        )
        .unwrap();
        assert_eq!(resolved.project.owner, "alice");
        assert_eq!(resolved.project.name, "demo");
        assert_eq!(resolved.source, ProjectSource::Flag);

        let resolved = resolve_project(None, Some("alice".into()), Some("demo".into()), || {
            panic!("File discovery must be skipped")
        })
        .unwrap();
        assert_eq!(resolved.project.owner, "alice");
        assert_eq!(resolved.project.name, "demo");
        assert_eq!(resolved.source, ProjectSource::Env);
    }

    #[test]
    fn environment_parts_independently_override_file() {
        for (namespace, project, expected_namespace, expected_project, source) in [
            (
                None,
                None,
                "file-owner",
                "file-project",
                ProjectSource::TracelToml,
            ),
            (
                Some("alice"),
                None,
                "alice",
                "file-project",
                ProjectSource::Env,
            ),
            (None, Some("demo"), "file-owner", "demo", ProjectSource::Env),
        ] {
            let resolved = resolve_project(
                None,
                namespace.map(str::to_owned),
                project.map(str::to_owned),
                || {
                    Ok(TracelTomlConfig {
                        namespace: Some("file-owner".into()),
                        project: Some("file-project".into()),
                    })
                },
            )
            .unwrap();
            assert_eq!(resolved.project.owner, expected_namespace);
            assert_eq!(resolved.project.name, expected_project);
            assert_eq!(resolved.source, source);
        }
    }

    #[test]
    fn partial_config_and_legacy_keys_match_sdk() {
        for (contents, namespace, project) in [
            ("project = 'demo'", Some("alice"), None),
            ("namespace = 'alice'", None, Some("demo")),
            ("owner = 'alice'\nname = 'demo'", None, None),
        ] {
            let resolved = resolve_project(
                None,
                namespace.map(str::to_owned),
                project.map(str::to_owned),
                || Ok(toml::from_str(contents).unwrap()),
            )
            .unwrap();
            assert_eq!(resolved.project.owner, "alice");
            assert_eq!(resolved.project.name, "demo");
        }
    }

    #[test]
    fn incomplete_identity_reports_not_found_and_hint() {
        for (namespace, project) in [(None, None), (Some("alice"), None), (None, Some("demo"))] {
            let error = resolve_project(
                None,
                namespace.map(str::to_owned),
                project.map(str::to_owned),
                || Ok(TracelTomlConfig::default()),
            )
            .unwrap_err();
            let report = crate::error::ErrorReport::new(&error);
            assert_eq!(crate::error::classify(&error), CliErrorKind::NotFound);
            assert_eq!(report.exit_code, 5);
            assert_eq!(
                report.hint,
                Some("Run 'tracel init' or pass --project <namespace>/<name>.")
            );
        }
        for (source, value) in [
            (ProjectSource::Flag, "flag"),
            (ProjectSource::Env, "env"),
            (ProjectSource::TracelToml, "tracel.toml"),
        ] {
            assert_eq!(serde_json::to_value(source).unwrap(), value);
        }
    }
}
