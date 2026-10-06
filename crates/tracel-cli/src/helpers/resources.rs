use serde_json::Value;

use crate::error::{CliError, ErrorKind};

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
