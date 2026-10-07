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
    Job(i32),
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
        (ClientError::NotFoundWithCode(ApiErrorCode::ComputeProviderJob), Resource::Job(job)) => (
            format!("No job {job} in {namespace}/{project}."),
            "List jobs with `tracel jobs list`.".to_string(),
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
    use rstest::rstest;

    use super::*;
    use crate::error::{ErrorReport, classify};

    #[rstest]
    #[case::model(
        ApiErrorCode::Model,
        Resource::Model("weights"),
        "'weights'",
        "tracel models list"
    )]
    #[case::model_of_a_version(
        ApiErrorCode::Model,
        Resource::ModelVersion { model: "weights", version: 7 },
        "'weights'",
        "tracel models list"
    )]
    #[case::version(
        ApiErrorCode::ModelVersion,
        Resource::ModelVersion { model: "weights", version: 7 },
        "version 7",
        "tracel models versions weights"
    )]
    #[case::alias(
        ApiErrorCode::ModelAlias,
        Resource::ModelAlias { model: "weights", alias: "production" },
        "'production'",
        "tracel models alias list weights"
    )]
    #[case::version_or_alias(
        ApiErrorCode::ModelAlias,
        Resource::ModelVersionRef { model: "weights", reference: "production" },
        "'production'",
        "tracel models versions weights"
    )]
    #[case::dataset(
        ApiErrorCode::Dataset,
        Resource::Dataset("images"),
        "'images'",
        "tracel datasets list"
    )]
    fn given_the_console_says_what_is_missing_when_mapping_the_error_then_it_names_it_and_how_to_list_them(
        #[case] code: ApiErrorCode,
        #[case] resource: Resource<'static>,
        #[case] named: &str,
        #[case] command: &str,
    ) {
        let error = map_resource_error(
            ClientError::NotFoundWithCode(code),
            "alice",
            "demo",
            resource,
        );

        assert!(error.to_string().contains(named), "{error}");
        assert!(ErrorReport::new(&error).hint.unwrap().contains(command));
    }

    #[test]
    fn given_a_plain_not_found_for_an_experiment_when_mapping_the_error_then_it_names_the_experiment()
     {
        let error = map_resource_error(
            ClientError::NotFound,
            "alice",
            "demo",
            Resource::Experiment(9),
        );

        assert!(error.to_string().contains("experiment 9"), "{error}");
        assert!(
            ErrorReport::new(&error)
                .hint
                .unwrap()
                .contains("tracel experiments list")
        );
    }

    #[rstest]
    #[case::unauthenticated(ClientError::Unauthenticated)]
    #[case::server_error(ClientError::InternalServerError)]
    #[case::another_resource(ClientError::NotFoundWithCode(ApiErrorCode::Dataset))]
    fn given_another_error_when_mapping_it_then_it_stays_the_client_error(
        #[case] client_error: ClientError,
    ) {
        let error = map_resource_error(client_error, "alice", "demo", Resource::Model("weights"));

        assert!(error.is::<ClientError>(), "{error:?}");
    }

    const ARTIFACTS: [(&str, &str); 3] = [("a1", "weights"), ("a2", "weights"), ("a3", "logs")];

    #[rstest]
    #[case::by_id("a2", 1)]
    #[case::by_unique_name("logs", 2)]
    fn given_an_id_or_a_unique_name_when_selecting_an_artifact_then_it_is_found(
        #[case] value: &str,
        #[case] index: usize,
    ) {
        let selected = select_artifact(ARTIFACTS, value, 7).unwrap();

        assert_eq!(selected, index);
    }

    #[test]
    fn given_a_name_several_artifacts_share_when_selecting_an_artifact_then_their_ids_are_offered()
    {
        let error = anyhow::Error::new(select_artifact(ARTIFACTS, "weights", 7).unwrap_err());

        assert_eq!(classify(&error), ErrorKind::Conflict);
        assert!(ErrorReport::new(&error).hint.unwrap().contains("a1, a2"));
    }

    #[test]
    fn given_an_unknown_artifact_when_selecting_it_then_it_is_not_found() {
        let error = select_artifact(ARTIFACTS, "missing", 7).unwrap_err();

        assert_eq!(error.kind, ErrorKind::NotFound);
    }

    #[test]
    fn given_a_json_object_when_parsing_metadata_then_it_is_the_metadata() {
        let metadata = parse_metadata(r#"{"format": "bin"}"#).unwrap();

        assert_eq!(metadata, serde_json::json!({"format": "bin"}));
    }

    #[rstest]
    #[case::array("[]")]
    #[case::number("1")]
    #[case::null("null")]
    fn given_json_that_is_not_an_object_when_parsing_metadata_then_it_is_a_usage_error(
        #[case] value: &str,
    ) {
        let error = parse_metadata(value).unwrap_err();

        assert_eq!(error.kind, ErrorKind::Usage);
    }
}
