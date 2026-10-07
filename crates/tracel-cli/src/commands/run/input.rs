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

/// A `--set <PATH>=<VALUE>`: the value is JSON, or a string when it does not parse as JSON.
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
                .map_err(|_| format!("'[{}]' is not an array index", &after[..end]))?;
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

/// A path as `--set` takes it; the whole input is `(root)`.
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
    /// Set the value at the path in `input`, creating missing objects and arrays on the way.
    /// An index may be one past the end of an array, which appends.
    pub fn apply(&self, input: &mut Value) -> Result<(), CliError> {
        let mut current = input;
        for (depth, segment) in self.path.iter().enumerate() {
            let failed = |reason: String| {
                CliError::new(
                    ErrorKind::Usage,
                    format!(
                        "Invalid --set {}: {reason}.",
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

/// Apply `patch` to `target` with JSON merge patch (RFC 7386): an object patch replaces the
/// fields it names and removes the ones it sets to `null`; any other patch replaces the target.
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
            format!("Cannot read config file '{}': {error}", path.display()),
        )
    })?;
    serde_json::from_str(&contents).map_err(|error| {
        CliError::new(
            ErrorKind::Usage,
            format!(
                "Config file '{}' is not valid JSON: {error}",
                path.display()
            ),
        )
    })
}

/// The input the job `definition` runs with, each step winning over the ones before: `start`,
/// then each config merge-patched onto it in order, then the job's flags `job_flags` set as the
/// job's command line sets them, then each assignment set.
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

/// Where `input` breaks `schema`, or why `schema` cannot be used. References outside the
/// schema are not fetched.
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

/// The error for an input that breaks the schema of `job`.
pub fn invalid_input(job: &str, violations: &[Violation]) -> CliError {
    let details = violations
        .iter()
        .map(|violation| format!("{}: {}", violation.path, violation.message))
        .collect::<Vec<_>>()
        .join("; ");
    CliError::new(
        ErrorKind::Usage,
        format!("Invalid input for job '{job}': {details}."),
    )
    .with_hint("Change it with --set <PATH>=<VALUE>; `tracel run --list --json` shows the job's input schema.")
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracel_job::JobKind;

    use super::*;

    fn key(key: &str) -> Segment {
        Segment::Key(key.to_string())
    }

    fn set(input: &mut Value, argument: &str) -> Result<(), CliError> {
        parse_assignment(argument).unwrap().apply(input)
    }

    #[test]
    fn paths_are_dotted_keys_with_array_indices() {
        assert_eq!(parse_path("epochs").unwrap(), [key("epochs")]);
        assert_eq!(
            parse_path("optimizer.lr").unwrap(),
            [key("optimizer"), key("lr")]
        );
        assert_eq!(
            parse_path("layers[0].sizes[2][1]").unwrap(),
            [
                key("layers"),
                Segment::Index(0),
                key("sizes"),
                Segment::Index(2),
                Segment::Index(1)
            ]
        );
        assert_eq!(
            parse_path("[1].name").unwrap(),
            [Segment::Index(1), key("name")]
        );
        assert_eq!(parse_path("a-b_c d").unwrap(), [key("a-b_c d")]);
        for path in [
            "", ".a", "a.", "a..b", "a[", "a[x]", "a[-1]", "a]b", "a[0]b",
        ] {
            assert!(parse_path(path).is_err(), "{path}");
        }
    }

    #[test]
    fn displayed_paths_parse_back() {
        for path in [
            "epochs",
            "optimizer.lr",
            "layers[0].sizes[2][1]",
            "[1].name",
        ] {
            assert_eq!(display_path(&parse_path(path).unwrap()), path);
        }
        assert_eq!(display_path(&[]), "(root)");
    }

    #[test]
    fn values_are_json_or_else_strings() {
        for (argument, value) in [
            ("epochs=5", json!(5)),
            ("lr=0.01", json!(0.01)),
            ("shuffle=true", json!(true)),
            ("tag=null", Value::Null),
            ("layers=[1, 2]", json!([1, 2])),
            (r#"name="x""#, json!("x")),
            ("name=x", json!("x")),
            ("name=", json!("")),
            ("expression=a=b", json!("a=b")),
            ("name={oops", json!("{oops")),
        ] {
            assert_eq!(
                parse_assignment(argument).unwrap().value,
                value,
                "{argument}"
            );
        }
        assert!(parse_assignment("epochs").is_err());
        assert!(parse_assignment("=5").is_err());
    }

    #[test]
    fn assignments_replace_values_and_create_missing_ones() {
        let mut input = json!({"epochs": 10, "optimizer": {"lr": 0.001}, "layers": [8]});
        set(&mut input, "epochs=5").unwrap();
        set(&mut input, "optimizer.lr=0.01").unwrap();
        set(&mut input, "optimizer.schedule.warmup=2").unwrap();
        set(&mut input, "layers[0]=16").unwrap();
        set(&mut input, "layers[1]=32").unwrap();
        set(&mut input, "heads[0].size=4").unwrap();
        assert_eq!(
            input,
            json!({
                "epochs": 5,
                "optimizer": {"lr": 0.01, "schedule": {"warmup": 2}},
                "layers": [16, 32],
                "heads": [{"size": 4}]
            })
        );

        let mut input = Value::Null;
        set(&mut input, "a.b=1").unwrap();
        assert_eq!(input, json!({"a": {"b": 1}}));
    }

    #[test]
    fn assignments_through_other_values_name_the_path() {
        let mut input = json!({"epochs": 10, "layers": [8]});
        for (argument, message) in [
            (
                "epochs.max=3",
                "Invalid --set epochs.max: epochs is a number, not an object.",
            ),
            (
                "layers.size=3",
                "Invalid --set layers.size: layers is an array, not an object.",
            ),
            (
                "epochs[0]=3",
                "Invalid --set epochs[0]: epochs is a number, not an array.",
            ),
            (
                "layers[3]=1",
                "Invalid --set layers[3]: layers has 1 items.",
            ),
        ] {
            let error = set(&mut input, argument).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Usage);
            assert_eq!(error.to_string(), message);
        }
        assert_eq!(input, json!({"epochs": 10, "layers": [8]}));
    }

    #[test]
    fn merge_patch_follows_rfc_7386() {
        // The examples of RFC 7386, appendix A.
        for (target, patch, result) in [
            (json!({"a": "b"}), json!({"a": "c"}), json!({"a": "c"})),
            (
                json!({"a": "b"}),
                json!({"b": "c"}),
                json!({"a": "b", "b": "c"}),
            ),
            (json!({"a": "b"}), json!({"a": null}), json!({})),
            (
                json!({"a": "b", "b": "c"}),
                json!({"a": null}),
                json!({"b": "c"}),
            ),
            (json!({"a": ["b"]}), json!({"a": "c"}), json!({"a": "c"})),
            (json!({"a": "c"}), json!({"a": ["b"]}), json!({"a": ["b"]})),
            (
                json!({"a": {"b": "c"}}),
                json!({"a": {"b": "d", "c": null}}),
                json!({"a": {"b": "d"}}),
            ),
            (
                json!({"a": [{"b": "c"}]}),
                json!({"a": [1]}),
                json!({"a": [1]}),
            ),
            (json!(["a", "b"]), json!(["c", "d"]), json!(["c", "d"])),
            (json!({"a": "b"}), json!(["c"]), json!(["c"])),
            (json!({"a": "foo"}), Value::Null, Value::Null),
            (json!({"a": "foo"}), json!("bar"), json!("bar")),
            (
                json!({"e": null}),
                json!({"a": 1}),
                json!({"e": null, "a": 1}),
            ),
            (
                json!([1, 2]),
                json!({"a": "b", "c": null}),
                json!({"a": "b"}),
            ),
            (
                json!({}),
                json!({"a": {"bb": {"ccc": null}}}),
                json!({"a": {"bb": {}}}),
            ),
        ] {
            let mut merged = target.clone();
            merge_patch(&mut merged, &patch);
            assert_eq!(merged, result, "{target} + {patch}");
        }
    }

    /// A job whose example input is `example`, with no schema.
    fn job(example: Value) -> JobDefinition {
        JobDefinition {
            name: "train".to_string(),
            kind: JobKind::Experiment,
            description: None,
            input_schema: None,
            input_example: Some(example),
        }
    }

    fn assignments(arguments: &[&str]) -> Vec<Assignment> {
        arguments
            .iter()
            .map(|argument| parse_assignment(argument).unwrap())
            .collect()
    }

    fn flags(arguments: &[&str]) -> Vec<String> {
        arguments
            .iter()
            .map(|argument| argument.to_string())
            .collect()
    }

    #[test]
    fn each_step_wins_over_the_ones_before() {
        let example = json!({
            "epochs": 10,
            "batch_size": 8,
            "tag": "example",
            "optimizer": {"lr": 0.001, "decay": 0.1}
        });
        let input = resolve(
            &job(example.clone()),
            example,
            &[
                json!({"epochs": 20, "batch_size": 16, "optimizer": {"decay": null}}),
                json!({"epochs": 30, "tag": "config"}),
            ],
            &flags(&["--epochs=40", "--batch-size=32", "--optimizer.lr=0.01"]),
            &assignments(&["epochs=50", "optimizer.lr=0.1"]),
        )
        .unwrap();
        assert_eq!(
            input,
            json!({"epochs": 50, "batch_size": 32, "tag": "config", "optimizer": {"lr": 0.1}})
        );
        assert_eq!(
            resolve(&job(json!({})), Value::Null, &[], &[], &[]).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn job_flags_set_what_the_same_assignments_set() {
        let example = json!({"epochs": 10, "shuffle": false, "optimizer": {"lr": 0.001}});
        let job = job(example.clone());
        let with_flags = resolve(
            &job,
            example.clone(),
            &[],
            &flags(&["--epochs=5", "--optimizer.lr=0.01", "--shuffle"]),
            &[],
        )
        .unwrap();
        let with_assignments = resolve(
            &job,
            example,
            &[],
            &[],
            &assignments(&["epochs=5", "optimizer.lr=0.01", "shuffle=true"]),
        )
        .unwrap();
        assert_eq!(with_flags, with_assignments);
        assert_eq!(
            with_flags,
            json!({"epochs": 5, "shuffle": true, "optimizer": {"lr": 0.01}})
        );
    }

    #[test]
    fn config_files_must_be_json() {
        let dir = tempfile::tempdir().unwrap();
        let valid = dir.path().join("valid.json");
        let invalid = dir.path().join("invalid.json");
        std::fs::write(&valid, r#"{"epochs": 2}"#).unwrap();
        std::fs::write(&invalid, "epochs = 2").unwrap();

        assert_eq!(read_config(&valid).unwrap(), json!({"epochs": 2}));
        for path in [invalid, dir.path().join("missing.json")] {
            let error = read_config(&path).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Usage);
            assert!(error.to_string().contains(&*path.to_string_lossy()));
        }
    }

    fn schema() -> Value {
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "epochs": {"type": "integer", "format": "uint", "minimum": 0},
                "optimizer": {"$ref": "#/$defs/Optimizer"},
                "layers": {"type": "array", "items": {"type": "integer"}}
            },
            "required": ["epochs", "optimizer"],
            "$defs": {
                "Optimizer": {
                    "type": "object",
                    "properties": {"lr": {"type": "number"}},
                    "required": ["lr"]
                }
            }
        })
    }

    #[test]
    fn a_valid_input_has_no_violations() {
        let input = json!({"epochs": 3, "optimizer": {"lr": 0.1}, "layers": [1, 2]});
        assert_eq!(violations(&schema(), &input).unwrap(), []);
    }

    #[test]
    fn violations_name_the_path_and_the_reason() {
        let input = json!({"epochs": "x", "optimizer": {}, "layers": [1, "two"]});
        let found = violations(&schema(), &input).unwrap();
        let paths: Vec<&str> = found
            .iter()
            .map(|violation| violation.path.as_str())
            .collect();
        assert_eq!(found.len(), 3, "{found:?}");
        for path in ["epochs", "optimizer", "layers[1]"] {
            assert!(paths.contains(&path), "{found:?}");
        }
        let epochs = found
            .iter()
            .find(|violation| violation.path == "epochs")
            .unwrap();
        assert_eq!(epochs.message, r#""x" is not of type "integer""#);

        let root = violations(&schema(), &json!([])).unwrap();
        assert_eq!(root[0].path, "(root)");

        let error = invalid_input("train", &found[..1]);
        assert_eq!(error.kind, ErrorKind::Usage);
        assert_eq!(
            error.to_string(),
            format!(
                "Invalid input for job 'train': {}: {}.",
                found[0].path, found[0].message
            )
        );
    }

    #[test]
    fn a_schema_with_a_remote_reference_is_not_fetched() {
        let schema = json!({"$ref": "https://example.com/schema.json"});
        assert!(violations(&schema, &json!({})).is_err());
        assert!(violations(&json!({"type": 5}), &json!({})).is_err());
    }
}
