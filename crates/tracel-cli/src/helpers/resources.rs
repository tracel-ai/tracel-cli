use serde_json::Value;
use tracel_client::{ApiErrorCode, ClientError};

use crate::error::{CliError, ErrorKind};

/// What a command looked for on the Tracel Console, to name it when it is missing.
#[derive(Clone, Copy)]
pub enum Resource<'a> {
    Model(&'a str),
    ModelVersionRef { model: &'a str, reference: &'a str },
    ModelVersion { model: &'a str, version: u32 },
    ModelAlias { model: &'a str, alias: &'a str },
    Experiment(i32),
    Dataset(&'a str),
}

/// Turn the Tracel Console's "not found" for `resource` into an error that names it and
/// how to list what exists. Other errors stay as they are.
pub fn map_resource_error(
    error: ClientError,
    namespace: &str,
    project: &str,
    resource: Resource<'_>,
) -> anyhow::Error {
    let (message, hint) = match (&error, resource) {
        (
            ClientError::NotFoundWithCode(ApiErrorCode::Model),
            Resource::Model(model)
            | Resource::ModelVersionRef { model, .. }
            | Resource::ModelVersion { model, .. }
            | Resource::ModelAlias { model, .. },
        )
        | (
            ClientError::NotFound | ClientError::NotFoundWithCode(ApiErrorCode::Unknown),
            Resource::Model(model),
        ) => (
            format!("No model '{model}' in {namespace}/{project}."),
            "List models with `tracel models list`.".to_string(),
        ),
        (
            ClientError::NotFoundWithCode(ApiErrorCode::ModelVersion | ApiErrorCode::ModelAlias),
            Resource::ModelVersionRef { model, reference },
        ) => (
            format!("No version or alias '{reference}' on model '{model}'."),
            format!(
                "List versions with `tracel models versions {model}` and aliases with `tracel models alias list {model}`."
            ),
        ),
        (
            ClientError::NotFoundWithCode(ApiErrorCode::ModelVersion),
            Resource::ModelVersion { model, version },
        ) => (
            format!("Model '{model}' has no version {version}."),
            format!("List versions with `tracel models versions {model}`."),
        ),
        (
            ClientError::NotFoundWithCode(ApiErrorCode::ModelAlias),
            Resource::ModelAlias { model, alias },
        ) => (
            format!("Model '{model}' has no alias '{alias}'."),
            format!("List aliases with `tracel models alias list {model}`."),
        ),
        (
            ClientError::NotFound | ClientError::NotFoundWithCode(ApiErrorCode::Unknown),
            Resource::Experiment(experiment),
        ) => (
            format!("No experiment {experiment} in {namespace}/{project}."),
            "List experiments with `tracel experiments list`.".to_string(),
        ),
        (ClientError::NotFoundWithCode(ApiErrorCode::Dataset), Resource::Dataset(dataset)) => (
            format!("No dataset '{dataset}' in {namespace}/{project}."),
            "List datasets with `tracel datasets list`.".to_string(),
        ),
        _ => return error.into(),
    };
    CliError::new(ErrorKind::NotFound, message)
        .with_hint(hint)
        .into()
}

/// Read `--metadata`, which must be a JSON object.
pub fn parse_metadata(value: &str) -> Result<Value, CliError> {
    let metadata: Value = serde_json::from_str(value).map_err(|error| {
        CliError::new(ErrorKind::Usage, format!("--metadata isn't JSON: {error}"))
    })?;
    if !metadata.is_object() {
        return Err(CliError::new(
            ErrorKind::Usage,
            "--metadata must be a JSON object.",
        ));
    }
    Ok(metadata)
}

/// Fail when `--description` comes without `--auto-create true`.
pub fn validate_auto_create(
    auto_create: Option<bool>,
    description: Option<&str>,
) -> Result<(), CliError> {
    if description.is_some() && auto_create != Some(true) {
        return Err(CliError::new(
            ErrorKind::Usage,
            "--description needs --auto-create true.",
        ));
    }
    Ok(())
}

/// Find the artifact `value` names among `(id, name)` pairs: by id first, else by a
/// name only one artifact has.
pub fn select_artifact<'a>(
    items: impl IntoIterator<Item = (&'a str, &'a str)>,
    value: &str,
    experiment: i32,
) -> Result<usize, CliError> {
    let mut matches = Vec::new();
    for (index, (id, name)) in items.into_iter().enumerate() {
        if id == value {
            return Ok(index);
        }
        if name == value {
            matches.push((index, id));
        }
    }
    match matches.as_slice() {
        [] => Err(CliError::new(
            ErrorKind::NotFound,
            format!("No artifact '{value}' in experiment {experiment}."),
        )
        .with_hint(format!(
            "List artifacts with `tracel artifacts list {experiment}`."
        ))),
        [(index, _)] => Ok(*index),
        _ => Err(CliError::new(
            ErrorKind::Conflict,
            format!("Several artifacts in experiment {experiment} are named '{value}'."),
        )
        .with_hint(format!(
            "Pass one of their ids: {}.",
            matches
                .iter()
                .map(|(_, id)| *id)
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::report;

    fn models() -> [Resource<'static>; 4] {
        [
            Resource::Model("weights"),
            Resource::ModelVersionRef {
                model: "weights",
                reference: "production",
            },
            Resource::ModelVersion {
                model: "weights",
                version: 7,
            },
            Resource::ModelAlias {
                model: "weights",
                alias: "production",
            },
        ]
    }

    #[test]
    fn a_missing_resource_names_itself_with_a_listing_hint() {
        let mut cases: Vec<(ClientError, Resource<'static>, &str)> = models()
            .into_iter()
            .map(|resource| {
                (
                    ClientError::NotFoundWithCode(ApiErrorCode::Model),
                    resource,
                    "'weights'",
                )
            })
            .collect();
        cases.extend([
            (
                ClientError::NotFound,
                Resource::Model("weights"),
                "'weights'",
            ),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::Unknown),
                Resource::Model("weights"),
                "'weights'",
            ),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::ModelVersion),
                Resource::ModelVersionRef {
                    model: "weights",
                    reference: "v7",
                },
                "'v7'",
            ),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::ModelAlias),
                Resource::ModelVersionRef {
                    model: "weights",
                    reference: "production",
                },
                "'production'",
            ),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::ModelVersion),
                Resource::ModelVersion {
                    model: "weights",
                    version: 7,
                },
                "version 7",
            ),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::ModelAlias),
                Resource::ModelAlias {
                    model: "weights",
                    alias: "production",
                },
                "'production'",
            ),
            (
                ClientError::NotFound,
                Resource::Experiment(99),
                "experiment 99",
            ),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::Unknown),
                Resource::Experiment(99),
                "experiment 99",
            ),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::Dataset),
                Resource::Dataset("images"),
                "'images'",
            ),
        ]);
        for (client_error, resource, named) in cases {
            let case = format!("{client_error:?}");
            let error = map_resource_error(client_error, "alice", "demo", resource);
            assert_eq!(report(&error), (ErrorKind::NotFound, 5, true), "{case}");
            assert!(error.to_string().contains(named), "{case}: {error}");
        }
    }

    #[test]
    fn other_errors_pass_through_unchanged() {
        let unrelated = || {
            [
                ClientError::Unauthenticated,
                ClientError::CredentialNotAllowed,
                ClientError::InternalServerError,
                ClientError::UnknownError("connection refused".into()),
                ClientError::NotFoundWithCode(ApiErrorCode::DatasetVersion),
                ClientError::NotFoundWithCode(ApiErrorCode::ModelVersionNotReady),
                ClientError::ApiError {
                    status: 404.try_into().unwrap(),
                    body: tracel_client::error::ApiErrorBody {
                        code: ApiErrorCode::Model,
                        message: "original message".into(),
                    },
                },
            ]
        };
        let mut cases: Vec<(ClientError, Resource<'static>)> = models()
            .into_iter()
            .chain([Resource::Experiment(99), Resource::Dataset("images")])
            .flat_map(|resource| unrelated().into_iter().map(move |error| (error, resource)))
            .collect();
        cases.extend([
            (
                ClientError::NotFoundWithCode(ApiErrorCode::Unknown),
                Resource::Dataset("images"),
            ),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::Model),
                Resource::Experiment(99),
            ),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::ModelVersion),
                Resource::Model("weights"),
            ),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::ModelAlias),
                Resource::ModelVersion {
                    model: "weights",
                    version: 7,
                },
            ),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::ModelVersion),
                Resource::ModelAlias {
                    model: "weights",
                    alias: "production",
                },
            ),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::Dataset),
                Resource::Model("weights"),
            ),
        ]);
        for (client_error, resource) in cases {
            let original = format!("{client_error:?}");
            let error = map_resource_error(client_error, "alice", "demo", resource);
            assert_eq!(
                format!("{:?}", error.downcast_ref::<ClientError>().unwrap()),
                original
            );
            assert!(!report(&error).2, "{original}");
        }
    }

    #[test]
    fn an_artifact_is_found_by_id_then_by_a_unique_name() {
        let items = [
            ("a", "weights"),
            ("b", "weights"),
            ("c", "a"),
            ("d", "logs"),
        ];
        for (value, expected) in [
            ("a", Ok(0)),
            ("logs", Ok(3)),
            ("weights", Err((ErrorKind::Conflict, 6, true))),
            ("missing", Err((ErrorKind::NotFound, 5, true))),
        ] {
            let selected = select_artifact(items, value, 7).map_err(|error| {
                let error = anyhow::Error::new(error);
                assert!(error.to_string().contains(&format!("'{value}'")), "{value}");
                report(&error)
            });
            assert_eq!(selected, expected, "{value}");
        }
    }

    #[test]
    fn metadata_is_a_json_object() {
        for (value, valid) in [
            (r#"{"format":"bin"}"#, true),
            ("{}", true),
            ("", false),
            ("{", false),
            ("null", false),
            ("[]", false),
            ("1", false),
            (r#""text""#, false),
        ] {
            assert_eq!(parse_metadata(value).is_ok(), valid, "{value:?}");
            if let Err(error) = parse_metadata(value) {
                assert_eq!(error.kind, ErrorKind::Usage, "{value:?}");
            }
        }
    }

    #[test]
    fn a_description_needs_auto_create_true() {
        for (auto_create, description, valid) in [
            (Some(true), Some("text"), true),
            (None, None, true),
            (Some(false), None, true),
            (None, Some("text"), false),
            (Some(false), Some("text"), false),
        ] {
            assert_eq!(
                validate_auto_create(auto_create, description).is_ok(),
                valid,
                "{auto_create:?} {description:?}"
            );
        }
    }
}
