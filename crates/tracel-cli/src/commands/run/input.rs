use std::path::Path;

use serde_json::{Map, Value};
use tracel_job::JobDefinition;

use super::job_flags;
use crate::error::{CliError, ErrorKind};

/// One step of a `--set` path: an object key, or an array index written `[i]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    Key(String),
    Index(usize),
}

/// A `--set <PATH>=<VALUE>`: the value is JSON, or a string when it isn't JSON.
#[derive(Clone, Debug, PartialEq)]
pub struct Assignment {
    pub path: Vec<Segment>,
    pub value: Value,
}

/// Parse a `--set` argument.
pub fn parse_assignment(argument: &str) -> Result<Assignment, String> {
    let (path, value) = argument
        .split_once('=')
        .ok_or("expected <PATH>=<VALUE>, such as optimizer.lr=0.01")?;
    Ok(Assignment {
        path: parse_path(path)?,
        value: serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string())),
    })
}

/// Parse a dotted path with `[i]` array indices, such as `layers[0].size`.
fn parse_path(path: &str) -> Result<Vec<Segment>, String> {
    let mut segments = Vec::new();
    let mut rest = path;
    loop {
        if !rest.starts_with('[') {
            let end = rest.find(['.', '[', ']']).unwrap_or(rest.len());
            if end == 0 {
                return Err(format!("empty key in path '{path}'"));
            }
            segments.push(Segment::Key(rest[..end].to_string()));
            rest = &rest[end..];
        }
        while let Some(after) = rest.strip_prefix('[') {
            let end = after
                .find(']')
                .ok_or_else(|| format!("missing ']' in path '{path}'"))?;
            let index = after[..end]
                .parse()
                .map_err(|_| format!("'[{}]' isn't an array index", &after[..end]))?;
            segments.push(Segment::Index(index));
            rest = &after[end + 1..];
        }
        if rest.is_empty() {
            return Ok(segments);
        }
        rest = rest
            .strip_prefix('.')
            .ok_or_else(|| format!("unexpected '{rest}' in path '{path}'"))?;
    }
}

/// Write a path as `--set` takes it; the whole input is `(root)`.
pub fn display_path(segments: &[Segment]) -> String {
    if segments.is_empty() {
        return "(root)".to_string();
    }
    let mut path = String::new();
    for segment in segments {
        match segment {
            Segment::Key(key) if path.is_empty() => path.push_str(key),
            Segment::Key(key) => {
                path.push('.');
                path.push_str(key);
            }
            Segment::Index(index) => path.push_str(&format!("[{index}]")),
        }
    }
    path
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

impl Assignment {
    /// Set the value at the path in `input`, creating missing objects and arrays on the
    /// way. An index may be one past the end of an array, which appends.
    pub fn apply(&self, input: &mut Value) -> Result<(), CliError> {
        let mut current = input;
        for (depth, segment) in self.path.iter().enumerate() {
            let failed = |reason: String| {
                CliError::new(
                    ErrorKind::Usage,
                    format!(
                        "Can't set {}: {reason}.",
                        display_path(&self.path[..=depth])
                    ),
                )
            };
            let parent = || display_path(&self.path[..depth]);
            current = match segment {
                Segment::Key(key) => {
                    if current.is_null() {
                        *current = Value::Object(Map::new());
                    }
                    match current {
                        Value::Object(map) => map.entry(key.clone()).or_insert(Value::Null),
                        other => {
                            return Err(failed(format!(
                                "{} is {}, not an object",
                                parent(),
                                type_name(other)
                            )));
                        }
                    }
                }
                Segment::Index(index) => {
                    if current.is_null() {
                        *current = Value::Array(Vec::new());
                    }
                    match current {
                        Value::Array(items) => {
                            if *index == items.len() {
                                items.push(Value::Null);
                            }
                            let length = items.len();
                            items
                                .get_mut(*index)
                                .ok_or_else(|| failed(format!("{} has {length} items", parent())))?
                        }
                        other => {
                            return Err(failed(format!(
                                "{} is {}, not an array",
                                parent(),
                                type_name(other)
                            )));
                        }
                    }
                }
            };
        }
        *current = self.value.clone();
        Ok(())
    }
}

/// Apply `patch` to `target` with JSON merge patch (RFC 7386): an object patch replaces
/// the fields it names and removes the ones it sets to `null`; any other patch replaces
/// the target.
pub fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(fields) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let Some(map) = target.as_object_mut() else {
        return;
    };
    for (key, value) in fields {
        if value.is_null() {
            map.remove(key);
        } else {
            merge_patch(map.entry(key.clone()).or_insert(Value::Null), value);
        }
    }
}

/// Read a `-c` file as JSON.
pub fn read_config(path: &Path) -> Result<Value, CliError> {
    let contents = std::fs::read_to_string(path).map_err(|error| {
        CliError::new(
            ErrorKind::Usage,
            format!("Can't read config file '{}': {error}", path.display()),
        )
    })?;
    serde_json::from_str(&contents).map_err(|error| {
        CliError::new(
            ErrorKind::Usage,
            format!("Config file '{}' isn't JSON: {error}", path.display()),
        )
    })
}

/// Resolve the input the job `definition` runs with, each step winning over the ones
/// before: `start`, then each config merge-patched onto it in order, then the job's flags
/// `job_flags` set as the job's command line sets them, then each assignment.
pub fn resolve(
    definition: &JobDefinition,
    start: Value,
    configs: &[Value],
    job_flags: &[String],
    assignments: &[Assignment],
) -> Result<Value, CliError> {
    let mut input = start;
    for config in configs {
        merge_patch(&mut input, config);
    }
    let mut input = job_flags::apply(definition, input, job_flags)?;
    for assignment in assignments {
        assignment.apply(&mut input)?;
    }
    Ok(input)
}

/// A place where an input breaks its schema.
#[derive(Debug, PartialEq, Eq)]
pub struct Violation {
    pub path: String,
    pub message: String,
}

/// Find where `input` breaks `schema`, or why `schema` can't be used. References outside
/// the schema are not fetched.
pub fn violations(schema: &Value, input: &Value) -> Result<Vec<Violation>, String> {
    let validator = jsonschema::validator_for(schema).map_err(|error| error.to_string())?;
    Ok(validator
        .iter_errors(input)
        .map(|error| {
            let path: Vec<Segment> = error
                .instance_path()
                .iter()
                .map(|segment| match segment {
                    jsonschema::paths::LocationSegment::Property(key) => {
                        Segment::Key(key.into_owned())
                    }
                    jsonschema::paths::LocationSegment::Index(index) => Segment::Index(index),
                })
                .collect();
            Violation {
                path: display_path(&path),
                message: error.to_string(),
            }
        })
        .collect())
}

/// Build the error for an input that breaks the schema of `job`.
pub fn invalid_input(job: &str, violations: &[Violation]) -> CliError {
    let details = violations
        .iter()
        .map(|violation| format!("{}: {}", violation.path, violation.message))
        .collect::<Vec<_>>()
        .join("; ");
    CliError::new(
        ErrorKind::Usage,
        format!("The input for job '{job}' is invalid: {details}."),
    )
    .with_hint(format!("See its flags with `tracel run {job} --help`."))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde_json::json;

    use super::*;
    use crate::test_support::job;

    fn key(key: &str) -> Segment {
        Segment::Key(key.to_string())
    }

    fn set(input: &mut Value, argument: &str) -> Result<(), CliError> {
        parse_assignment(argument).unwrap().apply(input)
    }

    fn train() -> JobDefinition {
        let schema = json!({
            "type": "object",
            "properties": {"epochs": {"type": "integer"}}
        });
        job("train", Some(schema), Some(json!({"epochs": 10})))
    }

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "epochs": {"type": "integer", "minimum": 0},
                "layers": {"type": "array", "items": {"type": "integer"}}
            },
            "required": ["epochs"]
        })
    }

    #[test]
    fn given_a_dotted_path_with_indices_when_parsing_an_assignment_then_each_key_and_index_is_a_step()
     {
        let assignment = parse_assignment("layers[0].sizes[2]=1").unwrap();

        assert_eq!(
            assignment.path,
            [
                key("layers"),
                Segment::Index(0),
                key("sizes"),
                Segment::Index(2)
            ]
        );
    }

    #[rstest]
    #[case::empty_key(".a=1", "empty key")]
    #[case::unclosed_index("a[=1", "missing ']'")]
    #[case::not_an_index("a[x]=1", "'[x]'")]
    #[case::text_after_an_index("a[0]b=1", "unexpected 'b'")]
    fn given_a_malformed_path_when_parsing_an_assignment_then_it_says_what_is_wrong(
        #[case] argument: &str,
        #[case] reason: &str,
    ) {
        let error = parse_assignment(argument).unwrap_err();

        assert!(error.contains(reason), "{error}");
    }

    #[test]
    fn given_no_equals_sign_when_parsing_an_assignment_then_it_shows_the_expected_form() {
        let error = parse_assignment("epochs").unwrap_err();

        assert!(error.contains("<PATH>=<VALUE>"), "{error}");
    }

    #[rstest]
    #[case::number("epochs=5", json!(5))]
    #[case::boolean("shuffle=true", json!(true))]
    #[case::array("layers=[1, 2]", json!([1, 2]))]
    #[case::quoted_string(r#"name="x""#, json!("x"))]
    fn given_a_json_value_when_parsing_an_assignment_then_it_is_that_value(
        #[case] argument: &str,
        #[case] expected: Value,
    ) {
        let assignment = parse_assignment(argument).unwrap();

        assert_eq!(assignment.value, expected);
    }

    #[rstest]
    #[case::word("name=x", "x")]
    #[case::empty("name=", "")]
    #[case::with_an_equals_sign("expression=a=b", "a=b")]
    fn given_text_that_is_not_json_when_parsing_an_assignment_then_it_is_a_string(
        #[case] argument: &str,
        #[case] expected: &str,
    ) {
        let assignment = parse_assignment(argument).unwrap();

        assert_eq!(assignment.value, json!(expected));
    }

    #[test]
    fn given_an_existing_field_when_setting_it_then_its_value_is_replaced() {
        let mut input = json!({"optimizer": {"lr": 0.001, "decay": 0.1}});

        set(&mut input, "optimizer.lr=0.01").unwrap();

        assert_eq!(input, json!({"optimizer": {"lr": 0.01, "decay": 0.1}}));
    }

    #[test]
    fn given_missing_objects_on_the_path_when_setting_a_field_then_they_are_created() {
        let mut input = json!({"epochs": 10});

        set(&mut input, "optimizer.schedule.warmup=2").unwrap();

        assert_eq!(
            input,
            json!({"epochs": 10, "optimizer": {"schedule": {"warmup": 2}}})
        );
    }

    #[test]
    fn given_an_index_one_past_the_end_when_setting_it_then_the_value_is_appended() {
        let mut input = json!({"layers": [8]});

        set(&mut input, "layers[1]=16").unwrap();

        assert_eq!(input, json!({"layers": [8, 16]}));
    }

    #[rstest]
    #[case::key_of_a_number("epochs.max=3", "not an object")]
    #[case::index_of_a_number("epochs[0]=3", "not an array")]
    #[case::index_past_the_end("layers[3]=1", "has 1 items")]
    fn given_a_path_the_input_cannot_hold_when_setting_it_then_it_is_a_usage_error_saying_why(
        #[case] argument: &str,
        #[case] reason: &str,
    ) {
        let mut input = json!({"epochs": 10, "layers": [8]});

        let error = set(&mut input, argument).unwrap_err();

        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(error.to_string().contains(reason), "{error}");
    }

    #[test]
    fn given_an_object_patch_when_merging_it_then_it_replaces_only_the_fields_it_names() {
        let mut input = json!({"epochs": 10, "optimizer": {"lr": 0.001, "decay": 0.1}});

        merge_patch(&mut input, &json!({"optimizer": {"lr": 0.01}}));

        assert_eq!(
            input,
            json!({"epochs": 10, "optimizer": {"lr": 0.01, "decay": 0.1}})
        );
    }

    #[test]
    fn given_a_null_field_in_the_patch_when_merging_it_then_the_field_is_removed() {
        let mut input = json!({"epochs": 10, "tag": "baseline"});

        merge_patch(&mut input, &json!({"tag": null}));

        assert_eq!(input, json!({"epochs": 10}));
    }

    #[test]
    fn given_a_patch_that_is_not_an_object_when_merging_it_then_it_replaces_the_input() {
        let mut input = json!({"layers": [8, 16]});

        merge_patch(&mut input, &json!([32]));

        assert_eq!(input, json!([32]));
    }

    #[test]
    fn given_a_config_when_resolving_the_input_then_it_wins_over_the_example() {
        let input = resolve(
            &train(),
            json!({"epochs": 10}),
            &[json!({"epochs": 20})],
            &[],
            &[],
        );

        assert_eq!(input.unwrap(), json!({"epochs": 20}));
    }

    #[test]
    fn given_a_job_flag_when_resolving_the_input_then_it_wins_over_the_config() {
        let flags = ["--epochs=30".to_string()];

        let input = resolve(
            &train(),
            json!({"epochs": 10}),
            &[json!({"epochs": 20})],
            &flags,
            &[],
        );

        assert_eq!(input.unwrap(), json!({"epochs": 30}));
    }

    #[test]
    fn given_an_assignment_when_resolving_the_input_then_it_wins_over_the_job_flags() {
        let flags = ["--epochs=30".to_string()];
        let assignments = [parse_assignment("epochs=40").unwrap()];

        let input = resolve(&train(), json!({"epochs": 10}), &[], &flags, &assignments);

        assert_eq!(input.unwrap(), json!({"epochs": 40}));
    }

    #[test]
    fn given_a_config_file_that_is_not_json_when_reading_it_then_it_is_a_usage_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("base.json");
        std::fs::write(&path, "epochs = 2").unwrap();

        let error = read_config(&path).unwrap_err();

        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(
            error.to_string().contains(&*path.to_string_lossy()),
            "{error}"
        );
    }

    #[test]
    fn given_an_input_that_follows_the_schema_when_checking_it_then_there_are_no_violations() {
        let input = json!({"epochs": 3, "layers": [1, 2]});

        let violations = violations(&schema(), &input).unwrap();

        assert_eq!(violations, []);
    }

    #[test]
    fn given_an_input_that_breaks_the_schema_when_checking_it_then_each_violation_names_its_path() {
        let input = json!({"epochs": -1, "layers": [1, "two"]});

        let violations = violations(&schema(), &input).unwrap();

        let paths: Vec<&str> = violations
            .iter()
            .map(|violation| violation.path.as_str())
            .collect();
        assert_eq!(paths, ["epochs", "layers[1]"]);
    }

    #[test]
    fn given_a_schema_with_a_remote_reference_when_checking_an_input_then_it_is_not_fetched() {
        let schema = json!({"$ref": "https://example.com/schema.json"});

        let checked = violations(&schema, &json!({}));

        assert!(checked.is_err());
    }

    #[test]
    fn given_violations_when_reporting_an_invalid_input_then_the_message_lists_them() {
        let violations = [Violation {
            path: "epochs".into(),
            message: "-1 is less than the minimum of 0".into(),
        }];

        let error = invalid_input("train", &violations);

        assert!(
            error
                .to_string()
                .contains("epochs: -1 is less than the minimum of 0"),
            "{error}"
        );
    }
}
