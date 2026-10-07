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
use crate::helpers::project::resolve_namespace_project;
use crate::output::{Details, Outcome, Output, Render, Table, json_section};
use crate::tools::tracel_config::TracelProject;

#[derive(Args, Debug)]
pub struct ExperimentsArgs {
    #[command(subcommand)]
    pub command: ExperimentsCommands,
}

#[derive(Subcommand, Debug)]
pub enum ExperimentsCommands {
    /// List experiments in the selected project.
    List(ListArgs),
    /// Show an experiment by its project-scoped number, or latest.
    Get(GetArgs),
    /// List metric definitions, or read a metric series or summary.
    Metrics(MetricsArgs),
    /// Read experiment logs, optionally following until the experiment finishes.
    Logs(LogsArgs),
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Page number (zero-based).
    #[arg(long)]
    pub page: Option<u32>,
    /// Maximum experiments per page.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub limit: Option<u32>,
    /// Server sort: field, field,asc, or field,desc; repeat for multiple fields.
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
        _ => Err("Expected a positive project-scoped experiment number or 'latest'.".into()),
    }
}

#[derive(Args, Debug)]
pub struct GetArgs {
    /// Project-scoped experiment number, or latest.
    #[arg(value_name = "NUM|latest", value_parser = parse_experiment)]
    pub experiment: ExperimentSelector,
}

#[derive(Args, Debug)]
pub struct MetricsArgs {
    /// Project-scoped experiment number.
    #[arg(value_parser = clap::value_parser!(i32).range(1..))]
    pub num: i32,
    /// Metric name; omit to list metric definitions.
    #[arg(long)]
    pub metric: Option<String>,
    /// Return a summary instead of the series.
    #[arg(long, requires = "metric")]
    pub summary: bool,
    /// Maximum number of series points.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(i64).range(1..))]
    pub max_points: i64,
    /// Series downsampling factor.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(i64).range(1..))]
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
        _ => Err("Expected KEY=VALUE with a nonempty key.".into()),
    }
}

#[derive(Args, Debug)]
pub struct LogsArgs {
    /// Project-scoped experiment number.
    #[arg(value_parser = clap::value_parser!(i32).range(1..))]
    pub num: i32,
    /// Maximum entries per request.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..))]
    pub limit: u32,
    /// Include this log level; repeat to include multiple levels.
    #[arg(long = "level", value_enum, value_name = "LEVEL")]
    pub levels: Vec<LogLevel>,
    /// Return entries after this sequence, oldest first.
    #[arg(long, conflicts_with_all = ["from", "to", "offset"])]
    pub after: Option<u64>,
    /// Start of the time range, as a timestamp string.
    #[arg(long)]
    pub from: Option<String>,
    /// End of the time range, as a timestamp string.
    #[arg(long)]
    pub to: Option<String>,
    /// Offset within the time range.
    #[arg(long)]
    pub offset: Option<u32>,
    /// Filter messages by search text.
    #[arg(long)]
    pub search: Option<String>,
    /// Require a metadata value; repeat for multiple filters.
    #[arg(long, value_name = "KEY=VALUE", value_parser = parse_metadata_pair)]
    pub metadata: Vec<(String, String)>,
    /// Exclude a metadata value; repeat for multiple filters.
    #[arg(long, value_name = "KEY=VALUE", value_parser = parse_metadata_pair)]
    pub metadata_not: Vec<(String, String)>,
    /// Require a metadata key; repeat for multiple filters.
    #[arg(long, value_name = "KEY", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub metadata_exists: Vec<String>,
    /// Poll every 2 seconds until finished; JSON output is NDJSON log and end events.
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
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        Table::new(["NUMBER", "NAME", "STATUS", "CREATED AT", "CREATED BY"])
            .rows(self.items.iter().map(|experiment| {
                [
                    experiment.experiment_num.to_string(),
                    experiment.name.clone().unwrap_or_default(),
                    experiment.status.clone(),
                    experiment.created_at.clone(),
                    experiment.created_by.username.clone(),
                ]
            }))
            .total(self.total)
            .write(out)
    }
}

impl Render for ExperimentDetailsResponse {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        Details::new()
            .field("Number", self.experiment_num)
            .field("ID", self.id)
            .field("Project ID", self.project_id)
            .optional("Name", self.name.as_ref())
            .field("Status", &self.status)
            .field("Description", &self.description)
            .field("Created at", &self.created_at)
            .field("Created by", &self.created_by.username)
            .write(out)?;
        json_section(out, "Config", &self.config)?;
        json_section(out, "Configurations", &self.configurations)?;
        json_section(out, "Attributes", &self.attributes)
    }
}

impl Render for MetricMetadataResponse {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        Table::new(["METRIC"])
            .rows(self.metric_types.iter().map(|name| [name.clone()]))
            .write(out)?;
        writeln!(out)?;
        Table::new(["GROUP"])
            .rows(self.groups.iter().map(|name| [name.clone()]))
            .write(out)
    }
}

impl Render for Option<MetricResponse> {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        let Some(metrics) = self else {
            return writeln!(out, "No metric series available.");
        };
        Table::new(["GROUP", "EPOCH", "ITERATION", "VALUE", "LOW", "HIGH"])
            .rows(metrics.groups.iter().flat_map(|group| {
                group.entries.iter().map(|entry| {
                    [
                        group.name.clone(),
                        entry.epoch.to_string(),
                        entry.iteration.to_string(),
                        entry.value.to_string(),
                        entry.low.to_string(),
                        entry.high.to_string(),
                    ]
                })
            }))
            .write(out)
    }
}

impl Render for Option<MetricSummaryResponse> {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        let Some(summary) = self else {
            return writeln!(out, "No metric summary available.");
        };
        Table::new(["GROUP", "OPTIMAL VALUE", "EPOCH"])
            .rows(summary.groups.iter().map(|group| {
                [
                    group.group.clone(),
                    group.optimal_value.to_string(),
                    group.epoch.to_string(),
                ]
            }))
            .write(out)
    }
}

impl Render for ExperimentLogQueryResponse {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        self.items.iter().try_for_each(|item| write_log(out, item))
    }
}

fn write_log(out: &mut dyn Write, item: &ExperimentLogItemResponse) -> io::Result<()> {
    let level = match item.log_level {
        LogLevelResponse::Trace => "trace",
        LogLevelResponse::Debug => "debug",
        LogLevelResponse::Info => "info",
        LogLevelResponse::Warn => "warn",
        LogLevelResponse::Error => "error",
    };
    let message = item.message.replace(['\r', '\n'], " ");
    writeln!(out, "{} {level} {message}", item.timestamp)
}

/// An event of `logs --follow`. As JSON, a log item gains `"type": "log"`.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LogEvent<'a> {
    Log(&'a ExperimentLogItemResponse),
    End { running: bool },
}

impl Render for LogEvent<'_> {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
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
        let response =
            client.query_experiment_logs(&project.owner, &project.name, num, request.clone())?;
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
            return Err(CliError::new(
                ErrorKind::Internal,
                "Log response has more entries but no advancing sequence.",
            )
            .into());
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
    let (owner, name) = (&project.owner, &project.name);
    match args.command {
        ExperimentsCommands::List(args) => Ok(client
            .get_project_experiments(
                owner,
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
                ExperimentSelector::Number(num) => client.get_experiment(owner, name, num)?,
                ExperimentSelector::Latest => client
                    .get_project_latest_experiment(owner, name)?
                    .ok_or_else(|| {
                        CliError::new(ErrorKind::NotFound, "No experiments found in this project.")
                    })?,
            };
            Ok(experiment.into())
        }
        ExperimentsCommands::Metrics(args) => {
            let Some(metric) = args.metric else {
                return Ok(client.get_metric_metadata(owner, name, args.num)?.into());
            };
            if args.summary {
                Ok(client
                    .get_metric_summary(owner, name, args.num, MetricSummaryQuery { metric })?
                    .into())
            } else {
                Ok(client
                    .get_metrics(
                        owner,
                        name,
                        args.num,
                        MetricAggregatedQuery {
                            metric,
                            max_points: args.max_points,
                            downsampling_factor: args.downsampling,
                        },
                    )?
                    .into())
            }
        }
        ExperimentsCommands::Logs(args) => {
            let num = args.num;
            let follow = args.follow;
            let request = args.into_request();
            if follow {
                follow_logs(&client, &project, num, request, context.output())?;
                return Ok(Outcome::streamed());
            }
            Ok(client
                .query_experiment_logs(owner, name, num, request)?
                .into())
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};
    use serde_json::json;

    use super::*;
    use crate::cli::{CliArgs, Commands};

    fn parse_command(arguments: &[&str]) -> ExperimentsArgs {
        let args = CliArgs::try_parse_from(arguments).unwrap();
        let Some(Commands::Experiments(args)) = args.command else {
            panic!("Expected experiments command");
        };
        args
    }

    #[test]
    fn command_definitions_are_valid_and_alias_preserves_sort() {
        CliArgs::command().debug_assert();
        let args = parse_command(&[
            "tracel",
            "exp",
            "list",
            "--page",
            "0",
            "--limit",
            "25",
            "--sort",
            "created_at,desc",
            "--sort",
            "name,asc",
        ]);
        let ExperimentsCommands::List(args) = args.command else {
            panic!("Expected list command");
        };
        assert_eq!(args.page, Some(0));
        assert_eq!(args.limit, Some(25));
        assert_eq!(args.sort, ["created_at,desc", "name,asc"]);
    }

    #[test]
    fn experiment_selectors_and_metric_defaults_are_validated() {
        for (selector, number) in [("latest", None), ("42", Some(42))] {
            let args = parse_command(&["tracel", "experiments", "get", selector]);
            let ExperimentsCommands::Get(args) = args.command else {
                panic!("Expected get command");
            };
            match args.experiment {
                ExperimentSelector::Latest => assert!(number.is_none()),
                ExperimentSelector::Number(value) => assert_eq!(Some(value), number),
            }
        }
        let args = parse_command(&["tracel", "exp", "metrics", "42", "--metric", "loss"]);
        let ExperimentsCommands::Metrics(args) = args.command else {
            panic!("Expected metrics command");
        };
        assert_eq!(args.num, 42);
        assert_eq!(args.metric.as_deref(), Some("loss"));
        assert_eq!(args.max_points, 100);
        assert_eq!(args.downsampling, 1);
        for arguments in [
            vec!["tracel", "exp", "get", "invalid"],
            vec!["tracel", "exp", "get", "0"],
            vec!["tracel", "exp", "metrics", "42", "--summary"],
            vec!["tracel", "exp", "metrics", "42", "--max-points", "0"],
            vec!["tracel", "exp", "metrics", "42", "--downsampling", "0"],
            vec!["tracel", "exp", "logs", "42", "--limit", "0"],
            vec!["tracel", "exp", "list", "--limit", "0"],
        ] {
            assert!(CliArgs::try_parse_from(arguments).is_err());
        }
    }

    #[test]
    fn log_filters_map_to_the_client_request() {
        let args = parse_command(&[
            "tracel",
            "exp",
            "logs",
            "42",
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
        let ExperimentsCommands::Logs(args) = args.command else {
            panic!("Expected logs command");
        };
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
    fn follow_starts_from_a_sequence_cursor() {
        for after in [None, Some("12")] {
            let mut arguments = vec!["tracel", "exp", "logs", "42", "--follow", "--json"];
            if let Some(after) = after {
                arguments.extend(["--after", after]);
            }
            let args = parse_command(&arguments);
            let ExperimentsCommands::Logs(args) = args.command else {
                panic!("Expected logs command");
            };
            let request = args.into_request();
            assert_eq!(
                request.after,
                Some(after.map_or(0, |value| value.parse().unwrap()))
            );
            assert_eq!(request.limit, Some(100));
            assert!(request.levels.is_none());
        }
        for flag in ["--follow", "--after"] {
            for (filter, value) in [("--from", "start"), ("--to", "end"), ("--offset", "5")] {
                let mut arguments = vec!["tracel", "exp", "logs", "42", flag];
                if flag == "--after" {
                    arguments.push("12");
                }
                arguments.extend([filter, value]);
                assert_eq!(
                    CliArgs::try_parse_from(arguments).unwrap_err().kind(),
                    clap::error::ErrorKind::ArgumentConflict
                );
            }
        }
    }

    #[test]
    fn followed_logs_are_typed_events() {
        let item = ExperimentLogItemResponse {
            seq: 7,
            log_level: LogLevelResponse::Warn,
            timestamp: "2026-10-07T00:00:00Z".into(),
            message: "loss\nspiked".into(),
            metadata: json!({"epoch": 3}),
        };
        assert_eq!(
            serde_json::to_value(LogEvent::Log(&item)).unwrap(),
            json!({
                "type": "log",
                "seq": 7,
                "log_level": "warn",
                "timestamp": "2026-10-07T00:00:00Z",
                "message": "loss\nspiked",
                "metadata": {"epoch": 3},
            })
        );
        assert_eq!(
            serde_json::to_value(LogEvent::End { running: false }).unwrap(),
            json!({"type": "end", "running": false})
        );
        let mut text = Vec::new();
        LogEvent::Log(&item).render(&mut text).unwrap();
        LogEvent::End { running: false }.render(&mut text).unwrap();
        assert_eq!(
            String::from_utf8(text).unwrap(),
            "2026-10-07T00:00:00Z warn loss spiked\n"
        );
    }
}
