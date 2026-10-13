use std::io::{self, Write};
use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use tracel_client::console::Client;
use tracel_client::console::experiment::request::{
    ExperimentLogQueryRequest, ListExperimentsQuery, LogLevelRequest, MetadataFilterRequest,
    MetricAggregatedQuery, MetricSummaryQuery,
};
use tracel_client::console::experiment::response::{
    ExperimentDetailsResponse, ExperimentLogItemResponse, ExperimentLogQueryResponse,
    ListExperimentsResponse, LogLevelResponse, MetricMetadataResponse, MetricResponse,
    MetricSummaryResponse,
};

use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::resolve_namespace_project;
use crate::helpers::{Resource, map_resource_error};
use crate::tools::tracel_config::TracelProject;
use crate::ui::{
    Details, Human, Layout, Outcome, Output, Render, Table, counted, list_summary, log_time,
    number, when,
};

#[derive(Args, Debug)]
pub struct ExperimentsArgs {
    #[command(subcommand)]
    pub command: ExperimentsCommands,
}

impl ExperimentsArgs {
    pub fn layout(&self) -> Layout {
        match &self.command {
            ExperimentsCommands::Logs(args) if args.follow => {
                Layout::flow(format!("Follow the logs of experiment {}", args.num))
            }
            _ => Layout::Read,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum ExperimentsCommands {
    /// List the project's experiments
    List(ListArgs),
    /// Show an experiment
    Get(GetArgs),
    /// List metrics, or show one metric's series
    Metrics(MetricsArgs),
    /// Show an experiment's logs
    Logs(LogsArgs),
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Show this page, counting from 0
    #[arg(long, value_name = "N")]
    pub page: Option<u32>,
    /// Show at most this many experiments
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub limit: Option<u32>,
    /// Sort by a field, asc or desc; repeat for more
    #[arg(long, value_name = "FIELD[,asc|desc]")]
    pub sort: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum ExperimentSelector {
    Number(i32),
    Latest,
}

fn parse_experiment(value: &str) -> Result<ExperimentSelector, String> {
    if value == "latest" {
        return Ok(ExperimentSelector::Latest);
    }
    match value.parse::<i32>() {
        Ok(number) if number > 0 => Ok(ExperimentSelector::Number(number)),
        _ => Err("Expected an experiment number or latest.".into()),
    }
}

#[derive(Args, Debug)]
pub struct GetArgs {
    /// Experiment number, or latest
    #[arg(value_name = "NUM|latest", value_parser = parse_experiment)]
    pub experiment: ExperimentSelector,
}

#[derive(Args, Debug)]
pub struct MetricsArgs {
    /// Experiment number
    #[arg(value_parser = clap::value_parser!(i32).range(1..))]
    pub num: i32,
    /// Show this metric's series
    #[arg(long, value_name = "NAME")]
    pub metric: Option<String>,
    /// Show the metric's summary instead of its series
    #[arg(long, requires = "metric")]
    pub summary: bool,
    /// Show at most this many points
    #[arg(long, value_name = "N", default_value_t = 100, value_parser = clap::value_parser!(i64).range(1..))]
    pub max_points: i64,
    /// Downsample the series by this factor
    #[arg(long, value_name = "N", default_value_t = 1, value_parser = clap::value_parser!(i64).range(1..))]
    pub downsampling: i64,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl From<LogLevel> for LogLevelRequest {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Trace => Self::Trace,
            LogLevel::Debug => Self::Debug,
            LogLevel::Info => Self::Info,
            LogLevel::Warn => Self::Warn,
            LogLevel::Error => Self::Error,
        }
    }
}

fn parse_metadata_pair(value: &str) -> Result<(String, String), String> {
    match value.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.into(), value.into())),
        _ => Err("Expected KEY=VALUE with a key.".into()),
    }
}

#[derive(Args, Debug)]
pub struct LogsArgs {
    /// Experiment number
    #[arg(value_parser = clap::value_parser!(i32).range(1..))]
    pub num: i32,
    /// Show at most this many entries
    #[arg(long, value_name = "N", default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..))]
    pub limit: u32,
    /// Show only this level; repeat for more
    #[arg(long = "level", value_enum, value_name = "LEVEL")]
    pub levels: Vec<LogLevel>,
    /// Show entries after this sequence number
    #[arg(long, value_name = "SEQ", conflicts_with_all = ["from", "to", "offset"])]
    pub after: Option<u64>,
    /// Show entries from this time
    #[arg(long, value_name = "TIME")]
    pub from: Option<String>,
    /// Show entries up to this time
    #[arg(long, value_name = "TIME")]
    pub to: Option<String>,
    /// Skip this many entries
    #[arg(long, value_name = "N")]
    pub offset: Option<u32>,
    /// Show entries whose message contains this text
    #[arg(long, value_name = "TEXT")]
    pub search: Option<String>,
    /// Show entries with this metadata; repeat for more
    #[arg(long, value_name = "KEY=VALUE", value_parser = parse_metadata_pair)]
    pub metadata: Vec<(String, String)>,
    /// Hide entries with this metadata; repeat for more
    #[arg(long, value_name = "KEY=VALUE", value_parser = parse_metadata_pair)]
    pub metadata_not: Vec<(String, String)>,
    /// Show entries that have this metadata key
    #[arg(long, value_name = "KEY", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub metadata_exists: Vec<String>,
    /// Keep showing new entries until the experiment ends
    #[arg(long, conflicts_with_all = ["from", "to", "offset"])]
    pub follow: bool,
}

impl LogsArgs {
    fn into_request(self) -> ExperimentLogQueryRequest {
        let metadata_filters = self
            .metadata
            .into_iter()
            .map(|(key, value)| MetadataFilterRequest::Equals { key, value })
            .chain(
                self.metadata_not
                    .into_iter()
                    .map(|(key, value)| MetadataFilterRequest::NotEquals { key, value }),
            )
            .chain(
                self.metadata_exists
                    .into_iter()
                    .map(|key| MetadataFilterRequest::Exists { key }),
            )
            .collect();
        ExperimentLogQueryRequest {
            after: if self.follow {
                Some(self.after.unwrap_or(0))
            } else {
                self.after
            },
            from: self.from,
            to: self.to,
            limit: Some(self.limit),
            offset: self.offset,
            levels: if self.levels.is_empty() {
                None
            } else {
                Some(self.levels.into_iter().map(Into::into).collect())
            },
            search: self.search,
            metadata_filters,
        }
    }
}

impl Render for ListExperimentsResponse {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        Table::new(["NUMBER", "NAME", "STATUS", "CREATED AT", "CREATED BY"])
            .shrink("NAME")
            .rows(self.items.iter().map(|experiment| {
                [
                    experiment.experiment_num.to_string(),
                    experiment.name.clone().unwrap_or_default(),
                    status_name(&experiment.status).to_string(),
                    when(&experiment.created_at),
                    experiment.created_by.username.clone(),
                ]
            }))
            .write(out)
    }

    fn summary(&self) -> Option<String> {
        Some(list_summary(
            self.items.len(),
            self.total,
            ["experiment", "experiments"],
            "No experiments in this project yet.",
        ))
    }
}

impl Render for ExperimentDetailsResponse {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        Details::new()
            .optional("Name", self.name.as_ref())
            .field("Description", &self.description)
            .field("Created at", when(&self.created_at))
            .field("Created by", &self.created_by.username)
            .section("Config", &self.config)
            .section("Configurations", &self.configurations)
            .section("Attributes", &self.attributes)
            .write(out)
    }

    fn summary(&self) -> Option<String> {
        let number = self.experiment_num;
        Some(match self.status.as_str() {
            "completed" => format!("Experiment {number} completed."),
            "failed" => format!("Experiment {number} failed."),
            status => format!("Experiment {number} is {}.", status_name(status)),
        })
    }
}

impl Render for MetricMetadataResponse {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        Table::new(["METRIC"])
            .empty("No metrics recorded.")
            .rows(self.metric_types.iter().map(|name| [name.clone()]))
            .write(out)?;
        writeln!(out)?;
        Table::new(["GROUP"])
            .empty("No metric groups.")
            .rows(self.groups.iter().map(|name| [name.clone()]))
            .write(out)
    }

    fn summary(&self) -> Option<String> {
        Some(format!(
            "{} in {}.",
            counted(self.metric_types.len() as u64, ["metric", "metrics"]),
            counted(self.groups.len() as u64, ["group", "groups"])
        ))
    }
}

impl Render for Option<MetricResponse> {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        let Some(metrics) = self else {
            let none = out.dim("No metric series available.");
            return writeln!(out, "{none}");
        };
        Table::new(["GROUP", "EPOCH", "ITERATION", "VALUE", "LOW", "HIGH"])
            .empty("No points in this series.")
            .rows(metrics.groups.iter().flat_map(|group| {
                group.entries.iter().map(|entry| {
                    [
                        group.name.clone(),
                        entry.epoch.to_string(),
                        entry.iteration.to_string(),
                        number(entry.value),
                        number(entry.low),
                        number(entry.high),
                    ]
                })
            }))
            .write(out)
    }
}

impl Render for Option<MetricSummaryResponse> {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        let Some(summary) = self else {
            let none = out.dim("No metric summary available.");
            return writeln!(out, "{none}");
        };
        Table::new(["GROUP", "OPTIMAL VALUE", "EPOCH"])
            .empty("No metric groups.")
            .rows(summary.groups.iter().map(|group| {
                [
                    group.group.clone(),
                    number(group.optimal_value),
                    group.epoch.to_string(),
                ]
            }))
            .write(out)
    }
}

impl Render for ExperimentLogQueryResponse {
    /// Write the entries oldest first, as `--follow` does.
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        let mut items: Vec<_> = self.items.iter().collect();
        items.sort_by_key(|item| item.seq);
        items.into_iter().try_for_each(|item| write_log(out, item))
    }

    fn summary(&self) -> Option<String> {
        Some(list_summary(
            self.items.len(),
            self.total,
            ["log entry", "log entries"],
            "No log entries.",
        ))
    }
}

/// Name an experiment status in a word: "stopping" for the server's `cancel_requested`.
fn status_name(status: &str) -> &str {
    match status {
        "cancel_requested" => "stopping",
        status => status,
    }
}

fn write_log(out: &mut Human<'_>, item: &ExperimentLogItemResponse) -> io::Result<()> {
    let level = match item.log_level {
        LogLevelResponse::Trace => "trace",
        LogLevelResponse::Debug => "debug",
        LogLevelResponse::Info => "info",
        LogLevelResponse::Warn => "warn",
        LogLevelResponse::Error => "error",
    };
    let message = item.message.replace(['\r', '\n'], " ");
    writeln!(out, "{} {level} {message}", log_time(&item.timestamp))
}

/// An event of `logs --follow`. As JSON, a log item gains `"type": "log"`.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LogEvent<'a> {
    Log(&'a ExperimentLogItemResponse),
    End { running: bool },
}

impl Render for LogEvent<'_> {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        match self {
            Self::Log(item) => write_log(out, item),
            Self::End { .. } => Ok(()),
        }
    }
}

fn follow_logs(
    client: &Client,
    project: &TracelProject,
    num: i32,
    mut request: ExperimentLogQueryRequest,
    output: &Output,
) -> anyhow::Result<()> {
    loop {
        let response = client
            .query_experiment_logs(&project.namespace, &project.name, num, request.clone())
            .map_err(|error| {
                map_resource_error(
                    error,
                    &project.namespace,
                    &project.name,
                    Resource::Experiment(num),
                )
            })?;
        for item in &response.items {
            output.event(&LogEvent::Log(item))?;
        }
        let previous = request.after.unwrap_or(0);
        let cursor = response
            .items
            .iter()
            .map(|item| item.seq)
            .max()
            .unwrap_or(previous)
            .max(previous);
        request.after = Some(cursor);
        if !response.running && !response.has_more {
            return output.event(&LogEvent::End { running: false });
        }
        if response.has_more && cursor == previous {
            anyhow::bail!("The Tracel Console sent more log entries without a new sequence.");
        }
        // Drain a backlog right away; wait only once caught up.
        if !response.has_more {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
}

pub fn handle_command(args: ExperimentsArgs, context: CliContext) -> anyhow::Result<Outcome> {
    let client = get_client_and_login_if_needed(&context)?;
    let project = resolve_namespace_project(&context)?.project;
    let (namespace, name) = (&project.namespace, &project.name);
    let missing =
        |num| move |error| map_resource_error(error, namespace, name, Resource::Experiment(num));
    match args.command {
        ExperimentsCommands::List(args) => Ok(client
            .get_project_experiments(
                namespace,
                name,
                ListExperimentsQuery {
                    page: args.page,
                    limit: args.limit,
                    sort: args.sort,
                },
            )?
            .into()),
        ExperimentsCommands::Get(args) => {
            let experiment = match args.experiment {
                ExperimentSelector::Number(num) => client
                    .get_experiment(namespace, name, num)
                    .map_err(missing(num))?,
                ExperimentSelector::Latest => client
                    .get_project_latest_experiment(namespace, name)?
                    .ok_or_else(|| {
                        CliError::new(ErrorKind::NotFound, "No experiments in this project yet.")
                    })?,
            };
            Ok(experiment.into())
        }
        ExperimentsCommands::Metrics(args) => {
            let Some(metric) = args.metric else {
                return Ok(client
                    .get_metric_metadata(namespace, name, args.num)
                    .map_err(missing(args.num))?
                    .into());
            };
            if args.summary {
                Ok(client
                    .get_metric_summary(namespace, name, args.num, MetricSummaryQuery { metric })
                    .map_err(missing(args.num))?
                    .into())
            } else {
                Ok(client
                    .get_metrics(
                        namespace,
                        name,
                        args.num,
                        MetricAggregatedQuery {
                            metric,
                            max_points: args.max_points,
                            downsampling_factor: args.downsampling,
                        },
                    )
                    .map_err(missing(args.num))?
                    .into())
            }
        }
        ExperimentsCommands::Logs(args) => {
            let num = args.num;
            let follow = args.follow;
            let request = args.into_request();
            if follow {
                follow_logs(&client, &project, num, request, context.output())?;
                context
                    .terminal()
                    .outro(&format!("Experiment {num} has finished."));
                return Ok(Outcome::streamed());
            }
            Ok(client
                .query_experiment_logs(namespace, name, num, request)
                .map_err(missing(num))?
                .into())
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::cli::Commands;
    use crate::test_support::{command, plain};

    fn logs_args(args: &[&str]) -> LogsArgs {
        let mut argv = vec!["experiments", "logs", "42"];
        argv.extend(args);
        match command(&argv) {
            Commands::Experiments(ExperimentsArgs {
                command: ExperimentsCommands::Logs(args),
            }) => args,
            other => panic!("{argv:?} parse as {other:?}"),
        }
    }

    fn log(seq: u64, message: &str) -> ExperimentLogItemResponse {
        ExperimentLogItemResponse {
            seq,
            log_level: LogLevelResponse::Warn,
            timestamp: "2026-10-07T00:00:00Z".into(),
            message: message.into(),
            metadata: json!({"epoch": 3}),
        }
    }

    #[test]
    fn an_experiment_is_a_positive_number_or_latest() {
        for (value, expected) in [
            ("latest", Some("latest")),
            ("42", Some("42")),
            ("0", None),
            ("-1", None),
            ("v7", None),
        ] {
            let parsed = parse_experiment(value).ok().map(|selector| match selector {
                ExperimentSelector::Latest => "latest".to_string(),
                ExperimentSelector::Number(number) => number.to_string(),
            });
            assert_eq!(parsed.as_deref(), expected, "{value}");
        }
    }

    #[test]
    fn log_filters_become_the_client_request() {
        let args = logs_args(&[
            "--limit",
            "25",
            "--level",
            "info",
            "--level",
            "error",
            "--from",
            "2026-10-06T00:00:00Z",
            "--to",
            "2026-10-07T00:00:00Z",
            "--offset",
            "5",
            "--search",
            "loss",
            "--metadata",
            "worker=gpu=0",
            "--metadata-not",
            "stage=init",
            "--metadata-exists",
            "epoch",
        ]);

        let request = args.into_request();

        assert_eq!(request.limit, Some(25));
        assert_eq!(
            request.levels,
            Some(vec![LogLevelRequest::Info, LogLevelRequest::Error])
        );
        assert_eq!(request.from.as_deref(), Some("2026-10-06T00:00:00Z"));
        assert_eq!(request.to.as_deref(), Some("2026-10-07T00:00:00Z"));
        assert_eq!(request.offset, Some(5));
        assert_eq!(request.search.as_deref(), Some("loss"));
        assert_eq!(request.after, None);
        assert_eq!(
            serde_json::to_value(request.metadata_filters).unwrap(),
            json!([
                {"type":"equals", "key":"worker", "value":"gpu=0"},
                {"type":"not_equals", "key":"stage", "value":"init"},
                {"type":"exists", "key":"epoch"},
            ])
        );
    }

    #[test]
    fn following_logs_reads_from_a_sequence_cursor() {
        for (args, after) in [
            (&["--follow"][..], Some(0)),
            (&["--follow", "--after", "12"], Some(12)),
            (&["--after", "12"], Some(12)),
            (&[], None),
        ] {
            assert_eq!(logs_args(args).into_request().after, after, "{args:?}");
        }
    }

    #[test]
    fn logs_read_oldest_first() {
        let logs = ExperimentLogQueryResponse {
            running: false,
            items: vec![log(9, "second"), log(8, "first")],
            has_more: false,
            next_cursor: None,
            total: 2,
        };

        let text = plain(&logs);

        let messages: Vec<&str> = text
            .lines()
            .map(|line| line.rsplit(' ').next().unwrap())
            .collect();
        assert_eq!(messages, ["first", "second"]);
    }

    #[test]
    fn a_followed_log_is_an_event_with_its_type() {
        let item = log(7, "loss\nspiked");
        for (event, expected) in [
            (
                LogEvent::Log(&item),
                json!({
                    "type": "log",
                    "seq": 7,
                    "log_level": "warn",
                    "timestamp": "2026-10-07T00:00:00Z",
                    "message": "loss\nspiked",
                    "metadata": {"epoch": 3},
                }),
            ),
            (
                LogEvent::End { running: false },
                json!({"type": "end", "running": false}),
            ),
        ] {
            assert_eq!(serde_json::to_value(&event).unwrap(), expected);
        }
    }

    #[test]
    fn a_log_line_shows_its_local_time_level_and_one_line_message() {
        let item = log(7, "loss\nspiked");

        let text = plain(&LogEvent::Log(&item)) + &plain(&LogEvent::End { running: false });

        assert_eq!(
            text,
            format!("{} warn loss spiked\n", log_time("2026-10-07T00:00:00Z"))
        );
    }
}
