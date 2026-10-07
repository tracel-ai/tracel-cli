use serde_json::Value;
use tracel_client::{ApiErrorCode, ClientError};

use crate::error::{CliError, ErrorKind};

#[derive(Clone, Copy)]
pub enum Resource<'a> {
    Model(&'a str),
    ModelVersionRef { model: &'a str, reference: &'a str },
    ModelVersion { model: &'a str, version: u32 },
    ModelAlias { model: &'a str, alias: &'a str },
    Experiment(i32),
    Dataset(&'a str),
}

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
        ) => (
            format!("No model '{model}' in {namespace}/{project}."),
            "List models with `tracel models list`.".to_string(),
        ),
        (
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

pub fn parse_metadata(value: &str) -> Result<Value, CliError> {
    let metadata: Value = serde_json::from_str(value).map_err(|error| {
        CliError::new(
            ErrorKind::Usage,
            format!("Invalid --metadata JSON: {error}"),
        )
    })?;
    if !metadata.is_object() {
        return Err(CliError::new(
            ErrorKind::Usage,
            "--metadata must be a JSON object.",
        ));
    }
    Ok(metadata)
}

pub fn validate_auto_create(
    auto_create: Option<bool>,
    description: Option<&str>,
) -> Result<(), CliError> {
    if description.is_some() && auto_create != Some(true) {
        return Err(CliError::new(
            ErrorKind::Usage,
            "--description can only be used together with --auto-create true.",
        ));
    }
    Ok(())
}

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
            format!(
                "Several artifacts named '{value}' in experiment {experiment}: {}.",
                matches
                    .iter()
                    .map(|(_, id)| *id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
        .with_hint("Pass the artifact id instead.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorReport;

    #[test]
    fn missing_models_use_the_resolved_project_in_every_model_context() {
        for resource in [
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
        ] {
            let error = map_resource_error(
                ClientError::NotFoundWithCode(ApiErrorCode::Model),
                "alice",
                "demo",
                resource,
            );
            assert_eq!(
                error.downcast_ref::<CliError>().unwrap().kind,
                ErrorKind::NotFound
            );
            let report = ErrorReport::new(&error);
            assert_eq!(report.message, "No model 'weights' in alice/demo.");
            assert_eq!(report.hint, Some("List models with `tracel models list`."));
            assert_eq!(report.exit_code, 5);
        }
        for client_error in [
            ClientError::NotFound,
            ClientError::NotFoundWithCode(ApiErrorCode::Unknown),
        ] {
            let error =
                map_resource_error(client_error, "alice", "demo", Resource::Model("weights"));
            let report = ErrorReport::new(&error);
            assert_eq!(report.message, "No model 'weights' in alice/demo.");
            assert_eq!(report.exit_code, 5);
        }
    }

    #[test]
    fn missing_resources_have_specific_messages_and_listing_hints() {
        for (client_error, resource, message, hint) in [
            (
                ClientError::NotFoundWithCode(ApiErrorCode::ModelVersion),
                Resource::ModelVersionRef {
                    model: "weights",
                    reference: "v7",
                },
                "No version or alias 'v7' on model 'weights'.",
                "List versions with `tracel models versions weights` and aliases with `tracel models alias list weights`.",
            ),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::ModelAlias),
                Resource::ModelVersionRef {
                    model: "weights",
                    reference: "production",
                },
                "No version or alias 'production' on model 'weights'.",
                "List versions with `tracel models versions weights` and aliases with `tracel models alias list weights`.",
            ),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::ModelVersion),
                Resource::ModelVersion {
                    model: "weights",
                    version: 7,
                },
                "Model 'weights' has no version 7.",
                "List versions with `tracel models versions weights`.",
            ),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::ModelAlias),
                Resource::ModelAlias {
                    model: "weights",
                    alias: "production",
                },
                "Model 'weights' has no alias 'production'.",
                "List aliases with `tracel models alias list weights`.",
            ),
            (
                ClientError::NotFound,
                Resource::Experiment(99),
                "No experiment 99 in alice/demo.",
                "List experiments with `tracel experiments list`.",
            ),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::Unknown),
                Resource::Experiment(99),
                "No experiment 99 in alice/demo.",
                "List experiments with `tracel experiments list`.",
            ),
            (
                ClientError::NotFoundWithCode(ApiErrorCode::Dataset),
                Resource::Dataset("images"),
                "No dataset 'images' in alice/demo.",
                "List datasets with `tracel datasets list`.",
            ),
        ] {
            let error = map_resource_error(client_error, "alice", "demo", resource);
            assert_eq!(
                error.downcast_ref::<CliError>().unwrap().kind,
                ErrorKind::NotFound
            );
            let report = ErrorReport::new(&error);
            assert_eq!(report.message, message);
            assert_eq!(report.hint, Some(hint));
            assert_eq!(report.exit_code, 5);
        }
    }

    #[test]
    fn unrelated_errors_and_codes_keep_the_original_client_error() {
        for resource in [
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
            Resource::Experiment(99),
            Resource::Dataset("images"),
        ] {
            for client_error in [
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
            ] {
                let original = format!("{client_error:?}");
                let error = map_resource_error(client_error, "alice", "demo", resource);
                assert_eq!(
                    format!("{:?}", error.downcast_ref::<ClientError>().unwrap()),
                    original
                );
                assert_eq!(ErrorReport::new(&error).hint, None);
            }
        }
        for (client_error, resource) in [
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
        ] {
            let original = format!("{client_error:?}");
            let error = map_resource_error(client_error, "alice", "demo", resource);
            assert_eq!(
                format!("{:?}", error.downcast_ref::<ClientError>().unwrap()),
                original
            );
            assert_eq!(ErrorReport::new(&error).hint, None);
        }
    }

    #[test]
    fn artifact_selection_prefers_id_and_reports_missing_or_ambiguous_names() {
        let items = [
            ("a", "weights"),
            ("b", "weights"),
            ("c", "a"),
            ("d", "logs"),
        ];
        assert_eq!(select_artifact(items, "a", 7).unwrap(), 0);
        assert_eq!(select_artifact(items, "logs", 7).unwrap(), 3);
        let error = select_artifact(items, "weights", 7).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Conflict);
        assert!(error.to_string().contains("a, b"));
        let error = select_artifact(items, "missing", 7).unwrap_err();
        assert_eq!(error.kind, ErrorKind::NotFound);
        assert_eq!(error.to_string(), "No artifact 'missing' in experiment 7.");
        assert!(select_artifact([], "missing", 7).is_err());
        assert_eq!(
            select_artifact([("a", "b"), ("b", "weights")], "b", 7).unwrap(),
            1
        );
    }

    #[test]
    fn metadata_requires_a_json_object_and_description_requires_creation() {
        assert_eq!(
            parse_metadata(r#"{"format":"bin"}"#).unwrap()["format"],
            "bin"
        );
        for value in ["", "{", "null", "[]", "1", "true", r#""text""#] {
            assert_eq!(parse_metadata(value).unwrap_err().kind, ErrorKind::Usage);
        }
        for auto_create in [None, Some(false)] {
            assert_eq!(
                validate_auto_create(auto_create, Some("text"))
                    .unwrap_err()
                    .kind,
                ErrorKind::Usage
            );
        }
        assert!(validate_auto_create(Some(true), Some("text")).is_ok());
        assert!(validate_auto_create(None, None).is_ok());
    }
}
