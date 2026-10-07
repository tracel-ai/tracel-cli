use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{self, Write};

use serde::Serialize;
use serde_json::Value;
use tracel_client::console::Client;
use tracel_client::console::experiment::request::MetricSummaryQuery;
use tracel_client::console::experiment::response::MetricSummaryGroupResponse;

use super::{CompareArgs, missing_experiment};
use crate::tools::tracel_config::TracelProject;
use crate::ui::{Human, Render, Table};

/// A JSON value that differs between two experiments. A side is `None` where the path does
/// not exist, and is then left out of the JSON output.
#[derive(Debug, PartialEq, Serialize)]
pub struct Difference {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other: Option<Value>,
}

/// A metric group's summary value in both experiments, with `delta = other - base`.
#[derive(Debug, PartialEq, Serialize)]
pub struct MetricComparison {
    pub metric: String,
    pub group: String,
    pub base: Option<f64>,
    pub other: Option<f64>,
    pub delta: Option<f64>,
}

#[derive(Serialize)]
pub struct Comparison {
    pub base: i32,
    pub other: i32,
    pub config: Vec<Difference>,
    pub attributes: Vec<Difference>,
    pub metrics: Vec<MetricComparison>,
}

pub fn compare_experiments(
    client: &Client,
    project: &TracelProject,
    args: CompareArgs,
) -> anyhow::Result<Comparison> {
    let (owner, name) = (&project.owner, &project.name);
    let get_experiment = |num| {
        client
            .get_experiment(owner, name, num)
            .map_err(|error| missing_experiment(error, project, num))
    };
    let base = get_experiment(args.base)?;
    let other = get_experiment(args.other)?;

    let metrics = if args.metrics.is_empty() {
        let metric_names = |num| {
            client
                .get_metric_metadata(owner, name, num)
                .map(|metadata| metadata.metric_types)
                .map_err(|error| missing_experiment(error, project, num))
        };
        defined_metrics(metric_names(args.base)?, metric_names(args.other)?)
    } else {
        requested_metrics(args.metrics)
    };
    let summary = |num, metric: &str| {
        client
            .get_metric_summary(
                owner,
                name,
                num,
                MetricSummaryQuery {
                    metric: metric.into(),
                },
            )
            .map(|summary| summary.map(|summary| summary.groups).unwrap_or_default())
            .map_err(|error| missing_experiment(error, project, num))
    };
    let mut metric_comparisons = Vec::new();
    for metric in &metrics {
        metric_comparisons.extend(compare_metric(
            metric,
            &summary(args.base, metric)?,
            &summary(args.other, metric)?,
        ));
    }

    let attributes =
        |attributes: HashMap<String, Value>| Value::Object(attributes.into_iter().collect());
    Ok(Comparison {
        base: args.base,
        other: args.other,
        config: diff_json(&base.config, &other.config),
        attributes: diff_json(&attributes(base.attributes), &attributes(other.attributes)),
        metrics: metric_comparisons,
    })
}

/// Every path where `base` and `other` differ, depth first with object keys sorted. Objects
/// and arrays are compared member by member; any other change, including a change of type,
/// is one difference holding both values.
fn diff_json(base: &Value, other: &Value) -> Vec<Difference> {
    let mut differences = Vec::new();
    diff_at(String::new(), Some(base), Some(other), &mut differences);
    differences
}

fn diff_at(
    path: String,
    base: Option<&Value>,
    other: Option<&Value>,
    differences: &mut Vec<Difference>,
) {
    match (base, other) {
        (Some(Value::Object(base)), Some(Value::Object(other))) => {
            let keys: BTreeSet<&String> = base.keys().chain(other.keys()).collect();
            for key in keys {
                diff_at(
                    key_path(&path, key),
                    base.get(key),
                    other.get(key),
                    differences,
                );
            }
        }
        (Some(Value::Array(base)), Some(Value::Array(other))) => {
            for index in 0..base.len().max(other.len()) {
                diff_at(
                    format!("{path}[{index}]"),
                    base.get(index),
                    other.get(index),
                    differences,
                );
            }
        }
        _ if base != other => differences.push(Difference {
            path,
            base: base.cloned(),
            other: other.cloned(),
        }),
        _ => {}
    }
}

/// `parent.key`, or `parent["key"]` when the key is empty or contains `.`, `[` or `]`.
fn key_path(parent: &str, key: &str) -> String {
    if key.is_empty() || key.contains(['.', '[', ']']) {
        format!("{parent}[{}]", Value::from(key))
    } else if parent.is_empty() {
        key.into()
    } else {
        format!("{parent}.{key}")
    }
}

/// Every metric either experiment defines, sorted.
fn defined_metrics(base: Vec<String>, other: Vec<String>) -> Vec<String> {
    base.into_iter()
        .chain(other)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The requested metrics in order, without repeats.
fn requested_metrics(mut metrics: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    metrics.retain(|metric| seen.insert(metric.clone()));
    metrics
}

/// Pair two summaries of `metric` by group, sorted by group. A group summarized by one
/// experiment only has no value for the other and no delta.
fn compare_metric(
    metric: &str,
    base: &[MetricSummaryGroupResponse],
    other: &[MetricSummaryGroupResponse],
) -> Vec<MetricComparison> {
    let (base, other) = (group_values(base), group_values(other));
    base.keys()
        .chain(other.keys())
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|group| {
            let base = base.get(group).copied();
            let other = other.get(group).copied();
            MetricComparison {
                metric: metric.into(),
                group: group.into(),
                base,
                other,
                delta: base.zip(other).map(|(base, other)| other - base),
            }
        })
        .collect()
}

fn group_values(groups: &[MetricSummaryGroupResponse]) -> BTreeMap<&str, f64> {
    groups
        .iter()
        .map(|group| (group.group.as_str(), group.optimal_value))
        .collect()
}

impl Render for Comparison {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        writeln!(
            out,
            "Experiment {} (base) compared with experiment {}.",
            self.base, self.other
        )?;
        write_differences(
            out,
            "Config differences",
            "No config differences.",
            &self.config,
        )?;
        write_differences(
            out,
            "Attribute differences",
            "No attribute differences.",
            &self.attributes,
        )?;
        writeln!(out)?;
        if self.metrics.is_empty() {
            return writeln!(out, "No metric summaries to compare.");
        }
        write_title(out, "Metric summaries")?;
        let number = |value: Option<f64>| value.map(|value| value.to_string()).unwrap_or_default();
        Table::new(["METRIC", "GROUP", "BASE", "OTHER", "DELTA"])
            .rows(self.metrics.iter().map(|metric| {
                [
                    metric.metric.clone(),
                    metric.group.clone(),
                    number(metric.base),
                    number(metric.other),
                    metric
                        .delta
                        .map(|delta| format!("{delta:+}"))
                        .unwrap_or_default(),
                ]
            }))
            .write(out)
    }
}

/// A table of `differences` under `title`, or the line `empty` when there are none.
fn write_differences(
    out: &mut Human<'_>,
    title: &str,
    empty: &str,
    differences: &[Difference],
) -> io::Result<()> {
    writeln!(out)?;
    if differences.is_empty() {
        return writeln!(out, "{empty}");
    }
    write_title(out, title)?;
    let value = |value: Option<&Value>| value.map(Value::to_string).unwrap_or_default();
    Table::new(["PATH", "BASE", "OTHER"])
        .shrink("BASE")
        .shrink("OTHER")
        .rows(differences.iter().map(|difference| {
            [
                difference.path.clone(),
                value(difference.base.as_ref()),
                value(difference.other.as_ref()),
            ]
        }))
        .write(out)
}

fn write_title(out: &mut Human<'_>, title: &str) -> io::Result<()> {
    let title = out.dim(format!("{title}:"));
    writeln!(out, "{title}")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn difference(path: &str, base: Option<Value>, other: Option<Value>) -> Difference {
        Difference {
            path: path.into(),
            base,
            other,
        }
    }

    fn groups(values: &[(&str, f64)]) -> Vec<MetricSummaryGroupResponse> {
        values
            .iter()
            .map(|&(group, optimal_value)| MetricSummaryGroupResponse {
                group: group.into(),
                optimal_value,
                epoch: 1,
            })
            .collect()
    }

    #[test]
    fn equal_values_have_no_differences() {
        for value in [
            json!(null),
            json!({}),
            json!({"optimizer": {"lr": 0.01, "betas": [0.9, 0.999]}, "seed": 1}),
            json!([1, {"a": [true, null]}]),
        ] {
            assert!(diff_json(&value, &value.clone()).is_empty());
        }
    }

    #[test]
    fn nested_objects_report_changed_added_and_removed_keys() {
        let base = json!({
            "optimizer": {"name": "adam", "lr": 0.01, "betas": [0.9, 0.999]},
            "seed": 1,
            "old": true,
        });
        let other = json!({
            "optimizer": {"name": "adam", "lr": 0.001, "betas": [0.9, 0.99], "decay": null},
            "seed": 1,
            "new": {"warmup": 5},
        });
        assert_eq!(
            diff_json(&base, &other),
            [
                difference("new", None, Some(json!({"warmup": 5}))),
                difference("old", Some(json!(true)), None),
                difference("optimizer.betas[1]", Some(json!(0.999)), Some(json!(0.99))),
                difference("optimizer.decay", None, Some(json!(null))),
                difference("optimizer.lr", Some(json!(0.01)), Some(json!(0.001))),
            ]
        );
        assert_eq!(
            diff_json(&other, &base)
                .into_iter()
                .map(|difference| difference.path)
                .collect::<Vec<_>>(),
            [
                "new",
                "old",
                "optimizer.betas[1]",
                "optimizer.decay",
                "optimizer.lr"
            ]
        );
    }

    #[test]
    fn arrays_compare_by_index_and_report_extra_items() {
        assert_eq!(
            diff_json(&json!([1, 2, 3]), &json!([1, 5])),
            [
                difference("[1]", Some(json!(2)), Some(json!(5))),
                difference("[2]", Some(json!(3)), None),
            ]
        );
        assert_eq!(
            diff_json(
                &json!({"layers": [{"size": 64}, {"size": 32}]}),
                &json!({"layers": [{"size": 64}, {"size": 16}, {"size": 8}]}),
            ),
            [
                difference("layers[1].size", Some(json!(32)), Some(json!(16))),
                difference("layers[2]", None, Some(json!({"size": 8}))),
            ]
        );
    }

    #[test]
    fn type_changes_are_one_difference_with_both_values() {
        assert_eq!(
            diff_json(
                &json!({"lr": 0.1, "schedule": "cosine", "flag": null, "sizes": [1], "head": {}}),
                &json!({"lr": "0.1", "schedule": {"kind": "cosine"}, "flag": false, "sizes": {"0": 1}, "head": []}),
            ),
            [
                difference("flag", Some(json!(null)), Some(json!(false))),
                difference("head", Some(json!({})), Some(json!([]))),
                difference("lr", Some(json!(0.1)), Some(json!("0.1"))),
                difference(
                    "schedule",
                    Some(json!("cosine")),
                    Some(json!({"kind": "cosine"}))
                ),
                difference("sizes", Some(json!([1])), Some(json!({"0": 1}))),
            ]
        );
        assert_eq!(
            diff_json(&json!(null), &json!({"lr": 0.1})),
            [difference("", Some(json!(null)), Some(json!({"lr": 0.1})))]
        );
    }

    #[test]
    fn keys_that_would_be_ambiguous_are_quoted() {
        assert_eq!(
            diff_json(
                &json!({"a.b": 1, "": 1, "outer": {"x[0]": 1, "plain key": 1}}),
                &json!({"a.b": 2, "": 2, "outer": {"x[0]": 2, "plain key": 2}}),
            )
            .into_iter()
            .map(|difference| difference.path)
            .collect::<Vec<_>>(),
            [
                r#"[""]"#,
                r#"["a.b"]"#,
                "outer.plain key",
                r#"outer["x[0]"]"#
            ]
        );
    }

    #[test]
    fn a_missing_side_is_omitted_while_null_is_kept() {
        assert_eq!(
            serde_json::to_value(diff_json(
                &json!({"removed": null, "changed": null}),
                &json!({"added": null, "changed": 1}),
            ))
            .unwrap(),
            json!([
                {"path": "added", "other": null},
                {"path": "changed", "base": null, "other": 1},
                {"path": "removed", "base": null},
            ])
        );
    }

    #[test]
    fn metric_groups_pair_by_name_with_deltas_only_when_both_exist() {
        let comparisons = compare_metric(
            "loss",
            &groups(&[("valid", 0.75), ("train", 0.5)]),
            &groups(&[("valid", 0.5), ("test", 0.25)]),
        );
        assert_eq!(
            serde_json::to_value(&comparisons).unwrap(),
            json!([
                {"metric": "loss", "group": "test", "base": null, "other": 0.25, "delta": null},
                {"metric": "loss", "group": "train", "base": 0.5, "other": null, "delta": null},
                {"metric": "loss", "group": "valid", "base": 0.75, "other": 0.5, "delta": -0.25},
            ])
        );
        assert_eq!(
            compare_metric("accuracy", &groups(&[("valid", 0.5)]), &[]),
            [MetricComparison {
                metric: "accuracy".into(),
                group: "valid".into(),
                base: Some(0.5),
                other: None,
                delta: None,
            }]
        );
        assert!(compare_metric("accuracy", &[], &[]).is_empty());
    }

    #[test]
    fn comparisons_read_as_tables_with_a_line_for_each_empty_section() {
        let comparison = Comparison {
            base: 3,
            other: 5,
            config: diff_json(&json!({"lr": 0.1, "seed": 1}), &json!({"lr": 0.01})),
            attributes: Vec::new(),
            metrics: compare_metric(
                "loss",
                &groups(&[("valid", 0.5)]),
                &groups(&[("valid", 0.25), ("train", 0.75)]),
            ),
        };
        let mut text = Vec::new();
        comparison.render(&mut Human::plain(&mut text)).unwrap();
        assert_eq!(
            String::from_utf8(text).unwrap(),
            "Experiment 3 (base) compared with experiment 5.\n\
             \n\
             Config differences:\n\
             PATH  BASE  OTHER\n\
             lr    0.1   0.01\n\
             seed  1     -\n\
             \n\
             No attribute differences.\n\
             \n\
             Metric summaries:\n\
             METRIC  GROUP  BASE  OTHER  DELTA\n\
             loss    train  -     0.75   -\n\
             loss    valid  0.5   0.25   -0.25\n"
        );
    }

    #[test]
    fn metrics_default_to_all_defined_and_requested_ones_keep_their_order() {
        assert_eq!(
            defined_metrics(
                vec!["loss".into(), "accuracy".into()],
                vec!["accuracy".into(), "f1".into()],
            ),
            ["accuracy", "f1", "loss"]
        );
        assert_eq!(
            requested_metrics(vec!["loss".into(), "accuracy".into(), "loss".into()]),
            ["loss", "accuracy"]
        );
    }
}
