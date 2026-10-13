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
        CliError::new(CliErrorKind::Usage, "Not in a Cargo workspace.")
            .with_hint("Run it in your project's directory, or pass -C <DIR>.")
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
        ErrorKind::Unexpected => (CliErrorKind::Internal, None),
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

#[derive(Deserialize, Default)]
struct TracelTomlConfig {
    #[serde(alias = "owner")]
    namespace: Option<String>,
    #[serde(alias = "name")]
    project: Option<String>,
}

fn project_not_found() -> CliError {
    CliError::new(CliErrorKind::NotFound, "No project here.")
        .with_hint("Run `tracel init`, or pass --project <namespace>/<name>.")
}

/// The error for a project the Tracel Console doesn't have, with a hint that fits how
/// it was selected.
pub fn missing_project(project: &TracelProject, source: &ProjectSource) -> CliError {
    let error = CliError::new(
        CliErrorKind::NotFound,
        format!(
            "No project {}/{} on the Tracel Console.",
            project.owner, project.name
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

/// Resolve project identity from flags, environment variables, or tracel.toml.
pub fn resolve_namespace_project(context: &CliContext) -> anyhow::Result<ResolvedProject> {
    resolve_project(context.project())
}

/// The project `flag` names, or else the one the environment or `tracel.toml` names.
pub fn resolve_project(flag: Option<&TracelProject>) -> anyhow::Result<ResolvedProject> {
    resolve_project_from(
        flag,
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
                        format!("Can't read '{}': {error}", path.display()),
                    )
                    .into());
                }
            };
            toml::from_str(&contents).map_err(|error| {
                CliError::new(
                    CliErrorKind::Usage,
                    format!("Can't parse '{}': {error}", path.display()),
                )
                .into()
            })
        },
    )
}

fn resolve_project_from(
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

/// Whether `tracel init` should go ahead: fails outside a Rust project, and is
/// false when the project is already linked and `force` is off.
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
    match client.get_project(&project.owner, &project.name) {
        Ok(_) => Ok(()),
        Err(e) if e.is_not_found() => Err(missing_project(project, source).into()),
        Err(e) => {
            Err(e).with_context(|| format!("Can't look up {}/{}", project.owner, project.name))
        }
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
        let resolved = resolve_project_from(
            Some(&flag),
            Some("other".into()),
            Some("project".into()),
            || panic!("File discovery must be skipped"),
        )
        .unwrap();
        assert_eq!(resolved.project.owner, "alice");
        assert_eq!(resolved.project.name, "demo");
        assert_eq!(resolved.source, ProjectSource::Flag);

        let resolved =
            resolve_project_from(None, Some("alice".into()), Some("demo".into()), || {
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
            let resolved = resolve_project_from(
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
            let resolved = resolve_project_from(
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
            let error = resolve_project_from(
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
                Some("Run `tracel init`, or pass --project <namespace>/<name>.")
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
