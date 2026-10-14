use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{self, Write};

use serde::Serialize;
use serde_json::Value;
use tracel_client::console::Client;
use tracel_client::console::experiment::request::MetricSummaryQuery;
use tracel_client::console::experiment::response::MetricSummaryGroupResponse;

use super::{CompareArgs, missing_experiment};
use crate::tools::tracel_config::TracelProject;
use crate::ui::{Human, Render, Table, counted, number};

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
    let (namespace, name) = (&project.namespace, &project.name);
    let get_experiment = |num| {
        client
            .get_experiment(namespace, name, num)
            .map_err(|error| missing_experiment(error, project, num))
    };
    let base = get_experiment(args.base)?;
    let other = get_experiment(args.other)?;

    let metrics = if args.metrics.is_empty() {
        let metric_names = |num| {
            client
                .get_metric_metadata(namespace, name, num)
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
                namespace,
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

/// List every path where `base` and `other` differ, depth first with object keys sorted.
/// Objects and arrays are compared member by member; any other change, including a change
/// of type, is one difference holding both values.
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

/// Write the path of `key` under `parent`: `parent.key`, or `parent["key"]` when the key is
/// empty or holds `.`, `[` or `]`.
fn key_path(parent: &str, key: &str) -> String {
    if key.is_empty() || key.contains(['.', '[', ']']) {
        format!("{parent}[{}]", Value::from(key))
    } else if parent.is_empty() {
        key.into()
    } else {
        format!("{parent}.{key}")
    }
}

/// List every metric either experiment defines, sorted.
fn defined_metrics(base: Vec<String>, other: Vec<String>) -> Vec<String> {
    base.into_iter()
        .chain(other)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Keep the requested metrics in order, without repeats.
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
    /// Write each part under its heading.
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        for (title, empty, differences) in [
            ("Config differences", "No config differences.", &self.config),
            (
                "Attribute differences",
                "No attribute differences.",
                &self.attributes,
            ),
        ] {
            out.heading(title)?;
            differences_table(differences).empty(empty).write(out)?;
            writeln!(out)?;
        }
        out.heading("Metric summaries")?;
        self.metric_table()
            .empty("No metric summaries to compare.")
            .write(out)
    }

    fn summary(&self) -> Option<String> {
        let differences = (self.config.len() + self.attributes.len()) as u64;
        Some(format!(
            "{} between experiments {} and {}.",
            counted(differences, ["difference", "differences"]),
            self.base,
            self.other
        ))
    }
}

impl Comparison {
    fn metric_table(&self) -> Table<5> {
        let value = |value: Option<f64>| value.map(number).unwrap_or_default();
        Table::new(["METRIC", "GROUP", "BASE", "OTHER", "DELTA"]).rows(self.metrics.iter().map(
            |metric| {
                [
                    metric.metric.clone(),
                    metric.group.clone(),
                    value(metric.base),
                    value(metric.other),
                    metric
                        .delta
                        .map(|delta| {
                            let sign = if delta > 0.0 { "+" } else { "" };
                            format!("{sign}{}", number(delta))
                        })
                        .unwrap_or_default(),
                ]
            },
        ))
    }
}

fn differences_table(differences: &[Difference]) -> Table<3> {
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

    fn summary(group: &str, optimal_value: f64) -> MetricSummaryGroupResponse {
        MetricSummaryGroupResponse {
            group: group.into(),
            optimal_value,
            epoch: 1,
        }
    }

    #[test]
    fn given_equal_configs_when_comparing_them_then_there_are_no_differences() {
        let config = json!({"optimizer": {"lr": 0.01, "betas": [0.9, 0.999]}, "seed": 1});

        let differences = diff_json(&config, &config.clone());

        assert_eq!(differences, []);
    }

    #[test]
    fn given_a_changed_nested_field_when_comparing_configs_then_its_dotted_path_has_both_values() {
        let base = json!({"optimizer": {"name": "adam", "lr": 0.01}});
        let other = json!({"optimizer": {"name": "adam", "lr": 0.001}});

        let differences = diff_json(&base, &other);

        assert_eq!(
            differences,
            [difference(
                "optimizer.lr",
                Some(json!(0.01)),
                Some(json!(0.001))
            )]
        );
    }

    #[test]
    fn given_a_field_only_one_config_has_when_comparing_them_then_the_other_side_is_missing() {
        let base = json!({"seed": 1, "old": true});
        let other = json!({"seed": 1, "new": null});

        let differences = diff_json(&base, &other);

        assert_eq!(
            differences,
            [
                difference("new", None, Some(json!(null))),
                difference("old", Some(json!(true)), None),
            ]
        );
    }

    #[test]
    fn given_arrays_when_comparing_configs_then_items_compare_by_index() {
        let base = json!({"layers": [64, 32, 16]});
        let other = json!({"layers": [64, 8]});

        let differences = diff_json(&base, &other);

        assert_eq!(
            differences,
            [
                difference("layers[1]", Some(json!(32)), Some(json!(8))),
                difference("layers[2]", Some(json!(16)), None),
            ]
        );
    }

    #[test]
    fn given_a_field_whose_type_changed_when_comparing_configs_then_it_is_one_difference() {
        let base = json!({"schedule": "cosine"});
        let other = json!({"schedule": {"kind": "cosine"}});

        let differences = diff_json(&base, &other);

        assert_eq!(
            differences,
            [difference(
                "schedule",
                Some(json!("cosine")),
                Some(json!({"kind": "cosine"}))
            )]
        );
    }

    #[test]
    fn given_a_key_a_dotted_path_would_misread_when_comparing_configs_then_it_is_quoted() {
        let base = json!({"outer": {"a.b": 1}});
        let other = json!({"outer": {"a.b": 2}});

        let differences = diff_json(&base, &other);

        assert_eq!(differences[0].path, r#"outer["a.b"]"#);
    }

    #[test]
    fn given_a_group_both_experiments_summarize_when_comparing_a_metric_then_the_delta_is_other_minus_base()
     {
        let comparisons =
            compare_metric("loss", &[summary("valid", 0.75)], &[summary("valid", 0.5)]);

        assert_eq!(comparisons[0].delta, Some(-0.25));
    }

    #[test]
    fn given_a_group_one_experiment_summarizes_when_comparing_a_metric_then_there_is_no_delta() {
        let comparisons = compare_metric("loss", &[summary("train", 0.5)], &[]);

        assert_eq!(
            comparisons,
            [MetricComparison {
                metric: "loss".into(),
                group: "train".into(),
                base: Some(0.5),
                other: None,
                delta: None,
            }]
        );
    }
}
