use std::io::Write;
use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::{Value, json};
use tracel_client::console::Client;
use tracel_client::console::experiment::request::{
    ExperimentLogQueryRequest, ListExperimentsQuery, LogLevelRequest, MetadataFilterRequest,
    MetricAggregatedQuery, MetricSummaryQuery,
};
use tracel_client::console::experiment::response::{
    ExperimentDetailsResponse, ExperimentLogItemResponse, ListExperimentsResponse,
    LogLevelResponse, MetricMetadataResponse, MetricResponse, MetricSummaryResponse,
};

use crate::commands::login::get_client_and_login_if_needed;
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind};
use crate::helpers::project::resolve_namespace_project;
use crate::output::{self, OutputMode};
use crate::tools::tracel_config::TracelProject;

#[derive(Args, Debug)]
pub struct ExperimentsArgs {
    #[command(subcommand)]
    pub command: ExperimentsCommands,
}

impl ExperimentsArgs {
    pub fn streams_output(&self) -> bool {
        matches!(
            &self.command,
            ExperimentsCommands::Logs(LogsArgs { follow: true, .. })
        )
    }
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

fn write_table(
    stdout: &mut impl Write,
    headers: &[&str],
    rows: Vec<Vec<String>>,
) -> std::io::Result<()> {
    let rows: Vec<Vec<String>> =
        std::iter::once(headers.iter().map(|header| (*header).into()).collect())
            .chain(rows)
            .map(|row: Vec<String>| {
                row.into_iter()
                    .map(|cell| cell.split_whitespace().collect::<Vec<_>>().join(" "))
                    .collect()
            })
            .collect();
    let widths: Vec<_> = (0..headers.len())
        .map(|column| {
            rows.iter()
                .map(|row| console::measure_text_width(&row[column]))
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in rows {
        for (column, cell) in row.iter().enumerate() {
            write!(stdout, "{cell}")?;
            if column + 1 < headers.len() {
                write!(
                    stdout,
                    "{}  ",
                    " ".repeat(widths[column].saturating_sub(console::measure_text_width(cell)))
                )?;
            }
        }
        writeln!(stdout)?;
    }
    Ok(())
}

fn print_list(response: &ListExperimentsResponse) -> anyhow::Result<()> {
    let rows = response
        .items
        .iter()
        .map(|experiment| {
            vec![
                experiment.experiment_num.to_string(),
                experiment.name.clone().unwrap_or_default(),
                experiment.status.clone(),
                experiment.created_at.clone(),
                experiment.created_by.username.clone(),
            ]
        })
        .collect();
    let mut stdout = std::io::stdout().lock();
    write_table(
        &mut stdout,
        &["NUMBER", "NAME", "STATUS", "CREATED AT", "CREATED BY"],
        rows,
    )?;
    let shown = response.items.len() as u64;
    let end = u64::from(response.page)
        .saturating_mul(u64::from(response.per_page))
        .saturating_add(shown);
    if end < response.total {
        writeln!(stdout, "Showing {shown} of {}", response.total)?;
    }
    Ok(())
}

fn print_experiment(experiment: &ExperimentDetailsResponse) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "Number: {}", experiment.experiment_num)?;
    writeln!(stdout, "ID: {}", experiment.id)?;
    writeln!(stdout, "Project ID: {}", experiment.project_id)?;
    writeln!(stdout, "Name: {}", experiment.name.as_deref().unwrap_or(""))?;
    writeln!(stdout, "Status: {}", experiment.status)?;
    writeln!(stdout, "Description: {}", experiment.description)?;
    writeln!(stdout, "Created at: {}", experiment.created_at)?;
    writeln!(stdout, "Created by: {}", experiment.created_by.username)?;
    writeln!(stdout, "Config:")?;
    serde_json::to_writer_pretty(&mut stdout, &experiment.config)?;
    writeln!(stdout, "\nConfigurations:")?;
    serde_json::to_writer_pretty(&mut stdout, &experiment.configurations)?;
    writeln!(stdout, "\nAttributes:")?;
    serde_json::to_writer_pretty(&mut stdout, &experiment.attributes)?;
    writeln!(stdout)?;
    Ok(())
}

fn print_metric_metadata(metadata: &MetricMetadataResponse) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    write_table(
        &mut stdout,
        &["METRIC"],
        metadata
            .metric_types
            .iter()
            .map(|name| vec![name.clone()])
            .collect(),
    )?;
    writeln!(stdout)?;
    write_table(
        &mut stdout,
        &["GROUP"],
        metadata
            .groups
            .iter()
            .map(|name| vec![name.clone()])
            .collect(),
    )?;
    Ok(())
}

fn print_metrics(response: &MetricResponse) -> anyhow::Result<()> {
    let rows = response
        .groups
        .iter()
        .flat_map(|group| {
            group.entries.iter().map(|entry| {
                vec![
                    group.name.clone(),
                    entry.epoch.to_string(),
                    entry.iteration.to_string(),
                    entry.value.to_string(),
                    entry.low.to_string(),
                    entry.high.to_string(),
                ]
            })
        })
        .collect();
    write_table(
        &mut std::io::stdout().lock(),
        &["GROUP", "EPOCH", "ITERATION", "VALUE", "LOW", "HIGH"],
        rows,
    )?;
    Ok(())
}

fn print_metric_summary(response: &MetricSummaryResponse) -> anyhow::Result<()> {
    let rows = response
        .groups
        .iter()
        .map(|group| {
            vec![
                group.group.clone(),
                group.optimal_value.to_string(),
                group.epoch.to_string(),
            ]
        })
        .collect();
    write_table(
        &mut std::io::stdout().lock(),
        &["GROUP", "OPTIMAL VALUE", "EPOCH"],
        rows,
    )?;
    Ok(())
}

fn print_logs(items: &[ExperimentLogItemResponse]) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    for item in items {
        let level = match item.log_level {
            LogLevelResponse::Trace => "trace",
            LogLevelResponse::Debug => "debug",
            LogLevelResponse::Info => "info",
            LogLevelResponse::Warn => "warn",
            LogLevelResponse::Error => "error",
        };
        let message = item.message.replace(['\r', '\n'], " ");
        writeln!(stdout, "{} {level} {message}", item.timestamp)?;
    }
    stdout.flush()?;
    Ok(())
}

#[derive(Serialize)]
struct LogEvent<'a> {
    #[serde(rename = "type")]
    event_type: &'static str,
    #[serde(flatten)]
    item: &'a ExperimentLogItemResponse,
}

fn follow_logs(
    client: &Client,
    project: &TracelProject,
    num: i32,
    mut request: ExperimentLogQueryRequest,
    mode: OutputMode,
) -> anyhow::Result<Value> {
    loop {
        let response =
            client.query_experiment_logs(&project.owner, &project.name, num, request.clone())?;
        if mode == OutputMode::Json {
            for item in &response.items {
                output::write_stream_item(&LogEvent {
                    event_type: "log",
                    item,
                })?;
            }
        } else {
            print_logs(&response.items)?;
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
            let end = json!({"type": "end", "running": false});
            if mode == OutputMode::Json {
                output::write_stream_item(&end)?;
            }
            return Ok(end);
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

pub fn handle_command(args: ExperimentsArgs, context: CliContext) -> anyhow::Result<Value> {
    context.terminal().command_title("Experiments");
    let client = get_client_and_login_if_needed(&context)?;
    let project = resolve_namespace_project(&context)?.project;
    let human = context.output() == OutputMode::Human;
    match args.command {
        ExperimentsCommands::List(args) => {
            let response = client.get_project_experiments(
                &project.owner,
                &project.name,
                ListExperimentsQuery {
                    page: args.page,
                    limit: args.limit,
                    sort: args.sort,
                },
            )?;
            if human {
                print_list(&response)?;
            }
            Ok(serde_json::to_value(response)?)
        }
        ExperimentsCommands::Get(args) => {
            let experiment = match args.experiment {
                ExperimentSelector::Number(num) => {
                    client.get_experiment(&project.owner, &project.name, num)?
                }
                ExperimentSelector::Latest => client
                    .get_project_latest_experiment(&project.owner, &project.name)?
                    .ok_or_else(|| {
                        CliError::new(ErrorKind::NotFound, "No experiments found in this project.")
                    })?,
            };
            if human {
                print_experiment(&experiment)?;
            }
            Ok(serde_json::to_value(experiment)?)
        }
        ExperimentsCommands::Metrics(args) => {
            let Some(metric) = args.metric else {
                let metadata =
                    client.get_metric_metadata(&project.owner, &project.name, args.num)?;
                if human {
                    print_metric_metadata(&metadata)?;
                }
                return Ok(serde_json::to_value(metadata)?);
            };
            if args.summary {
                let response = client.get_metric_summary(
                    &project.owner,
                    &project.name,
                    args.num,
                    MetricSummaryQuery { metric },
                )?;
                if human {
                    if let Some(response) = &response {
                        print_metric_summary(response)?;
                    } else {
                        context.terminal().print("No metric summary available.");
                    }
                }
                Ok(serde_json::to_value(response)?)
            } else {
                let response = client.get_metrics(
                    &project.owner,
                    &project.name,
                    args.num,
                    MetricAggregatedQuery {
                        metric,
                        max_points: args.max_points,
                        downsampling_factor: args.downsampling,
                    },
                )?;
                if human {
                    if let Some(response) = &response {
                        print_metrics(response)?;
                    } else {
                        context.terminal().print("No metric series available.");
                    }
                }
                Ok(serde_json::to_value(response)?)
            }
        }
        ExperimentsCommands::Logs(args) => {
            let num = args.num;
            let follow = args.follow;
            let request = args.into_request();
            if follow {
                return follow_logs(&client, &project, num, request, context.output());
            }
            let response =
                client.query_experiment_logs(&project.owner, &project.name, num, request)?;
            if human {
                print_logs(&response.items)?;
            }
            Ok(serde_json::to_value(response)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

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
        assert!(!args.streams_output());
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
    fn follow_uses_a_sequence_cursor_and_streams_output() {
        for after in [None, Some("12")] {
            let mut arguments = vec!["tracel", "exp", "logs", "42", "--follow", "--json"];
            if let Some(after) = after {
                arguments.extend(["--after", after]);
            }
            let args = parse_command(&arguments);
            assert!(args.streams_output());
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
}
