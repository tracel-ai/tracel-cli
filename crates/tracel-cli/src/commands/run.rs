mod describe;
mod input;
mod job_flags;
mod launch;
mod remote;
mod report;

use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::Context;
use clap::builder::StyledStr;
use clap::{ArgAction, ArgGroup, Args, Command};
use serde::Serialize;
use serde_json::Value;
use tracel_client::console::Env;
use tracel_job::{DefinitionsFile, JobKind, RunReport};

pub use job_flags::{job_flags, with_job_flags};

use crate::commands::experiments::{ExperimentSelector, get_experiment, parse_experiment};
use crate::commands::login::get_client_and_login_if_needed;
use crate::commands::package::PackageArgs;
use crate::context::CliContext;
use crate::error::{CliError, ErrorKind, classify};
use crate::helpers::{require_cargo_workspace, resolve_namespace_project};
use crate::tools::cargo;
use crate::tools::tracel_config::TracelProject;
use crate::ui::{Human, Outcome, Render, Table, Terminal};

use describe::{describe, find_job};
use input::Assignment;
use launch::{Event, Launch, Target, launch, select_target};
use report::{announcement, ending, ending_error};

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("packaging")
        .args(["mode", "targets", "install_targets"])
        .multiple(true)
        .requires("remote")
))]
#[command(group(ArgGroup::new("selection").args(["job", "list", "remote"]).multiple(true)))]
#[command(mut_arg("bin", |arg| arg
    .requires("selection")
    .help("Binary to run jobs from, and with --remote to upload, when there are several")))]
#[command(disable_help_flag = true)]
pub struct RunArgs {
    /// Job to run, as --list names it, followed by its own flags, which `tracel run <JOB> --help` lists
    #[arg(value_name = "JOB", conflicts_with = "forwarded")]
    pub job: Option<String>,
    /// The job's own flags, as arguments of the job's command line, such as `--epochs=5`
    #[arg(skip)]
    pub job_flags: Vec<String>,
    /// List the jobs of the workspace binary that uses the tracel crate
    #[arg(long, conflicts_with_all = ["job", "remote", "forwarded"])]
    pub list: bool,
    /// JSON file merged onto the job's input with JSON merge patch (repeatable)
    #[arg(short = 'c', long = "config", value_name = "FILE", requires = "job")]
    pub configs: Vec<PathBuf>,
    /// Set a value in the job's input, such as optimizer.lr=0.01 (repeatable)
    #[arg(
        long = "set",
        value_name = "PATH=VALUE",
        value_parser = input::parse_assignment,
        requires = "job"
    )]
    pub assignments: Vec<Assignment>,
    /// Start from the input of this Console experiment instead of the job's example input
    #[arg(long, value_name = "NUM|latest", value_parser = parse_experiment, requires = "job")]
    pub like: Option<ExperimentSelector>,
    /// Record the run on this machine instead of the Tracel Console
    #[arg(long, requires = "job", conflicts_with = "remote")]
    pub offline: bool,
    /// Queue a job on this compute provider group instead of running locally
    #[arg(long, value_name = "GROUP")]
    pub remote: Option<String>,
    #[command(flatten)]
    pub package: PackageArgs,
    /// Queue the job without asking for confirmation
    #[arg(long, short = 'y', requires = "remote")]
    pub yes: bool,
    /// Package and show the job without uploading or queuing anything
    #[arg(long, requires = "remote", conflicts_with_all = ["follow", "yes"])]
    pub dry_run: bool,
    /// Follow the job's logs until it finishes; exit 9 when it fails or is cancelled
    #[arg(long, requires = "remote")]
    pub follow: bool,
    /// Arguments passed to the program (without a JOB)
    #[arg(last = true, value_name = "ARGS")]
    pub forwarded: Vec<String>,
    /// Print help, with the job's own flags after a JOB
    #[arg(short, long, action = ArgAction::SetTrue)]
    pub help: bool,
}

pub fn handle_command(args: RunArgs, context: CliContext) -> anyhow::Result<Outcome> {
    if args.list {
        return Ok(list_jobs(&args, &context)?.into());
    }
    if let Some(job) = &args.job {
        return run_job(job, &args, &context);
    }
    match &args.remote {
        Some(group) => remote::handle_command(
            group,
            remote::job_command(&args.forwarded),
            &args.package,
            &args,
            &context,
        ),
        None => {
            run_locally(&args.forwarded, &context)?;
            Ok(Outcome::streamed())
        }
    }
}

fn list_jobs(args: &RunArgs, context: &CliContext) -> anyhow::Result<DefinitionsFile> {
    let workspace = require_cargo_workspace()?;
    Ok(describe(context.terminal(), &workspace, args.package.bin.as_deref())?.definitions)
}

impl Render for DefinitionsFile {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        Table::new(["NAME", "KIND", "DESCRIPTION"])
            .shrink("DESCRIPTION")
            .rows(self.jobs.iter().map(|job| {
                let kind = match job.kind {
                    JobKind::Experiment => "experiment",
                    JobKind::Inference => "inference",
                };
                [
                    job.name.clone(),
                    kind.to_string(),
                    job.description.clone().unwrap_or_default(),
                ]
            }))
            .write(out)
    }
}

/// The data of a local job run.
#[derive(Serialize)]
struct JobRun {
    job: String,
    input: Value,
    target: Target,
    exit_code: Option<i32>,
    report: Option<RunReport>,
}

impl Render for JobRun {}

/// Run `job` of the workspace binary with its resolved input, locally or with --remote.
fn run_job(job: &str, args: &RunArgs, context: &CliContext) -> anyhow::Result<Outcome> {
    let terminal = context.terminal();
    if let Some(group) = &args.remote {
        args.package.check(terminal)?;
        if !args.yes && !args.dry_run {
            terminal.require_confirmation(
                &format!(
                    "Queue job '{job}' on compute provider group '{group}'? It may incur costs."
                ),
                "yes",
            )?;
        }
    }

    let workspace = require_cargo_workspace()?;
    let described = describe(terminal, &workspace, args.package.bin.as_deref())?;
    let definition = find_job(&described.definitions, job)?;
    let start = match &args.like {
        Some(experiment) => recorded_input(context, experiment)?,
        None => definition.input_example.clone().unwrap_or(Value::Null),
    };
    let configs = args
        .configs
        .iter()
        .map(|path| input::read_config(path))
        .collect::<Result<Vec<_>, _>>()?;
    let input = input::resolve(
        definition,
        start,
        &configs,
        &args.job_flags,
        &args.assignments,
    )?;
    if let Some(schema) = &definition.input_schema {
        match input::violations(schema, &input) {
            Ok(violations) if violations.is_empty() => {}
            Ok(violations) => return Err(input::invalid_input(job, &violations).into()),
            Err(error) => terminal.print_warning(&format!(
                "The input of job '{job}' is not checked: its schema cannot be used ({error})."
            )),
        }
    }
    let input_json = serde_json::to_string(&input)?;

    if let Some(group) = &args.remote {
        let packaging = PackageArgs {
            bin: Some(described.binary.name.clone()),
            ..args.package.clone()
        };
        let command = remote::job_command(&[job.to_string(), input_json]);
        return remote::handle_command(group, command, &packaging, args, context);
    }

    let project = linked_project(context)?;
    let logged_in = !args.offline && project.is_some() && context.has_credentials()?;
    let (target, note) = select_target(args.offline, project.is_some(), logged_in);
    if let Some(note) = note {
        terminal.instruct(note);
    }

    let mut env = sdk_environment(context, project.as_ref());
    env.push(("TRACEL_TARGET", target.as_str().to_string()));
    let report_directory = tempfile::tempdir().context("Failed to create a temporary directory")?;
    let report_path = report_directory.path().join("report.json");
    let mut announced = false;
    let finished = launch(
        Launch {
            program: &described.program,
            job,
            input: &input_json,
            env,
            stdout: context.output().child_stdout(),
            report_path: &report_path,
        },
        |event| match event {
            Event::Report(report) => {
                if let Some(announcement) = announcement(&report.experiment) {
                    if !announced {
                        terminal.print(&announcement);
                    }
                    announced = true;
                }
            }
            Event::Stopping => terminal.print(&format!(
                "Stopping job '{job}'; it is killed if still running in 30 seconds."
            )),
            Event::Killed => terminal.print_warning(&format!("Killed job '{job}'.")),
            Event::UnreadableReport(reason) => terminal.print_warning(&format!(
                "Ignoring the run report of job '{job}': {reason}."
            )),
        },
    )?;

    let report = finished.report;
    let ended = ending(report.as_ref(), finished.exit);
    if let Some(error) = ending_error(job, &ended, report.as_ref()) {
        return Err(error.into());
    }
    terminal.print_success(&format!("Job '{job}' completed."));
    Ok(JobRun {
        job: job.to_string(),
        input,
        target,
        exit_code: finished.exit.code,
        report,
    }
    .into())
}

/// The help of `tracel run`, as clap writes it.
pub struct Help(StyledStr);

impl Serialize for Help {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

impl Render for Help {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        if out.color() {
            write!(out, "{}", self.0.ansi())
        } else {
            write!(out, "{}", self.0)
        }
    }
}

/// The help of `tracel run`, the subcommand of `tracel`: after a JOB, that of the job, with
/// its own flags from its definition.
pub fn help(args: &RunArgs, tracel: Command, terminal: &Terminal) -> anyhow::Result<Help> {
    let tracel = match &args.job {
        Some(job) => {
            let workspace = require_cargo_workspace()?;
            let described = describe(terminal, &workspace, args.package.bin.as_deref())?;
            job_flags::job_help(tracel, find_job(&described.definitions, job)?)
        }
        None => tracel,
    };
    Ok(Help(job_flags::run_command(&tracel).render_long_help()))
}

/// The project from --project, the environment or `tracel.toml`, or `None` when none is set.
fn linked_project(context: &CliContext) -> anyhow::Result<Option<TracelProject>> {
    match resolve_namespace_project(context) {
        Ok(resolved) => Ok(Some(resolved.project)),
        Err(error) if classify(&error) == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The input the Console experiment `selector` names recorded.
fn recorded_input(context: &CliContext, selector: &ExperimentSelector) -> anyhow::Result<Value> {
    let project = resolve_namespace_project(context)?.project;
    let client = get_client_and_login_if_needed(context)?;
    let experiment = get_experiment(&client, &project, selector)?;
    if experiment.config.is_null() {
        return Err(CliError::new(
            ErrorKind::NotFound,
            format!(
                "Experiment {} recorded no input.",
                experiment.experiment_num
            ),
        )
        .with_hint("Leave out --like to start from the job's example input.")
        .into());
    }
    Ok(experiment.config)
}

/// Run `cargo run` in the current directory, passing the arguments given after `--`.
///
/// `tracel run -- entrypoint` is equivalent to `cargo run -- entrypoint`: stdio is
/// inherited and the program's exit code is the command's.
fn run_locally(forwarded: &[String], context: &CliContext) -> anyhow::Result<()> {
    let mut cmd = cargo::command();
    cmd.arg("run");
    cmd.envs(sdk_environment(context, context.project()));

    if !forwarded.is_empty() {
        cmd.arg("--");
        cmd.args(forwarded);
    }

    hand_over(cmd)
}

/// The variables that name the Console and project to the SDK.
fn sdk_environment(
    context: &CliContext,
    project: Option<&TracelProject>,
) -> Vec<(&'static str, String)> {
    let mut env = vec![("TRACEL_ENV", tracel_env_value(&context.environment()))];
    if let Some(project) = project {
        env.push(("TRACEL_NAMESPACE", project.owner.clone()));
        env.push(("TRACEL_PROJECT", project.name.clone()));
    }
    env
}

/// Replace this process with `cmd`. `cargo run` does the same with the program it
/// builds, so a signal sent to `tracel` reaches the program instead of orphaning it.
#[cfg(unix)]
fn hand_over(mut cmd: std::process::Command) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;

    Err(cmd.exec()).context("Failed to run `cargo run`")
}

/// Windows cannot replace a process, so wait for the child and mirror its exit code.
#[cfg(not(unix))]
fn hand_over(mut cmd: std::process::Command) -> anyhow::Result<()> {
    let status = cmd.status().context("Failed to run `cargo run`")?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}

/// The value the SDK expects in the `TRACEL_ENV` environment variable.
///
/// The SDK parses this string with an explicit match (see `discover_env` in the
/// `tracel-core` cloud backend). This needs to match.
fn tracel_env_value(env: &Env) -> String {
    match env {
        Env::Production => "Production".to_string(),
        Env::Development => "Development".to_string(),
        Env::Staging(version) => format!("Staging({version})"),
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;
    use clap::error::ErrorKind;
    use serde_json::json;

    use super::*;
    use crate::cli::{CliArgs, Commands};
    use crate::commands::package::Mode;

    fn parse(arguments: &[&str]) -> Result<RunArgs, clap::Error> {
        let Some(Commands::Run(args)) = CliArgs::try_parse_args(arguments)?.command else {
            panic!("Expected run command");
        };
        Ok(args)
    }

    #[test]
    fn without_a_job_list_or_remote_the_run_is_plain() {
        CliArgs::command().debug_assert();
        let args = parse(&["tracel", "run", "--", "--remote", "gpu", "--yes"]).unwrap();
        assert!(args.job.is_none());
        assert!(!args.list);
        assert!(args.remote.is_none());
        assert_eq!(args.forwarded, ["--remote", "gpu", "--yes"]);
    }

    #[test]
    fn a_job_takes_its_input_flags() {
        let args = parse(&[
            "tracel",
            "run",
            "train",
            "-c",
            "base.json",
            "--config",
            "gpu.json",
            "--set",
            "epochs=5",
            "--set",
            "optimizer.lr=0.01",
            "--like",
            "42",
            "--offline",
            "--bin",
            "trainer",
        ])
        .unwrap();
        assert_eq!(args.job.as_deref(), Some("train"));
        assert_eq!(
            args.configs,
            [PathBuf::from("base.json"), PathBuf::from("gpu.json")]
        );
        assert_eq!(args.assignments.len(), 2);
        assert_eq!(args.assignments[1].value, json!(0.01));
        assert!(matches!(args.like, Some(ExperimentSelector::Number(42))));
        assert!(args.offline);
        assert_eq!(args.package.bin.as_deref(), Some("trainer"));

        let args = parse(&["tracel", "run", "train", "--like", "latest"]).unwrap();
        assert!(matches!(args.like, Some(ExperimentSelector::Latest)));
        assert!(!args.offline);
    }

    #[test]
    fn list_takes_only_bin() {
        let args = parse(&["tracel", "run", "--list", "--bin", "trainer"]).unwrap();
        assert!(args.list);
        assert_eq!(args.package.bin.as_deref(), Some("trainer"));
        for arguments in [
            &["tracel", "run", "--list", "train"][..],
            &["tracel", "run", "--list", "--remote", "gpu"],
            &["tracel", "run", "--list", "--", "train"],
        ] {
            let error = parse(arguments).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::ArgumentConflict, "{arguments:?}");
            assert_eq!(error.exit_code(), 2);
        }
    }

    #[test]
    fn a_job_and_forwarded_arguments_cannot_be_combined() {
        let error = parse(&["tracel", "run", "train", "--", "--epochs", "5"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn input_flags_require_a_job() {
        for flags in [
            &["-c", "base.json"][..],
            &["--set", "epochs=5"],
            &["--like", "42"],
            &["--offline"],
        ] {
            let prefixes = if flags == ["--offline"] {
                &[&["tracel", "run"][..]][..]
            } else {
                &[
                    &["tracel", "run"][..],
                    &["tracel", "run", "--remote", "gpu"],
                ]
            };
            for prefix in prefixes {
                let arguments = [prefix, flags].concat();
                let error = parse(&arguments).unwrap_err();
                assert_eq!(
                    error.kind(),
                    ErrorKind::MissingRequiredArgument,
                    "{arguments:?}"
                );
            }
        }
        let error = parse(&["tracel", "run", "train", "--set", "epochs"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        let error = parse(&["tracel", "run", "train", "--like", "0"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        let error = parse(&["tracel", "run", "train", "--offline", "--remote", "gpu"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn remote_accepts_packaging_flags_and_forwarded_arguments() {
        let args = parse(&[
            "tracel",
            "run",
            "--remote",
            "gpu",
            "--mode",
            "binary",
            "--target",
            "x86_64-unknown-linux-gnu",
            "--target",
            "aarch64-unknown-linux-gnu",
            "--bin",
            "trainer",
            "--install-targets",
            "-y",
            "--follow",
            "--",
            "train",
            "--epochs",
            "10",
        ])
        .unwrap();
        assert_eq!(args.remote.as_deref(), Some("gpu"));
        assert_eq!(args.package.mode, Some(Mode::Binary));
        assert_eq!(
            args.package.targets,
            ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]
        );
        assert_eq!(args.package.bin.as_deref(), Some("trainer"));
        assert!(args.package.install_targets);
        assert!(args.yes);
        assert!(!args.dry_run);
        assert!(args.follow);
        assert_eq!(args.forwarded, ["train", "--epochs", "10"]);

        let args = parse(&["tracel", "run", "--remote", "gpu", "--dry-run"]).unwrap();
        assert!(args.dry_run);
        assert!(!args.follow);
        assert!(args.package.mode.is_none());
        assert!(args.forwarded.is_empty());
    }

    #[test]
    fn remote_job_runs_take_input_and_packaging_flags() {
        let args = parse(&[
            "tracel", "run", "train", "--set", "epochs=5", "--remote", "gpu", "--mode", "source",
            "--yes", "--follow",
        ])
        .unwrap();
        assert_eq!(args.job.as_deref(), Some("train"));
        assert_eq!(args.remote.as_deref(), Some("gpu"));
        assert_eq!(args.package.mode, Some(Mode::Source));
        assert!(args.follow);
    }

    #[test]
    fn remote_flags_require_remote() {
        for flags in [
            &["--mode", "source"][..],
            &["--target", "x86_64-unknown-linux-gnu"],
            &["--install-targets"],
            &["--yes"],
            &["-y"],
            &["--dry-run"],
            &["--follow"],
        ] {
            for prefix in [&["tracel", "run"][..], &["tracel", "run", "train"]] {
                let arguments = [prefix, flags].concat();
                let error = parse(&arguments).unwrap_err();
                assert_eq!(
                    error.kind(),
                    ErrorKind::MissingRequiredArgument,
                    "{arguments:?}"
                );
                assert_eq!(error.exit_code(), 2);
            }
        }
        let error = parse(&["tracel", "run", "--remote"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn bin_requires_a_job_list_or_remote() {
        let error = parse(&["tracel", "run", "--bin", "trainer"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
        let error = parse(&["tracel", "run", "--bin", "trainer", "--", "train"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
        for arguments in [
            &["tracel", "run", "train", "--bin", "trainer"][..],
            &["tracel", "run", "--list", "--bin", "trainer"],
            &["tracel", "run", "--remote", "gpu", "--bin", "trainer"],
        ] {
            assert!(parse(arguments).is_ok(), "{arguments:?}");
        }
    }

    #[test]
    fn dry_run_conflicts_with_follow_and_yes() {
        for flag in ["--follow", "--yes", "-y"] {
            let error =
                parse(&["tracel", "run", "--remote", "gpu", "--dry-run", flag]).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::ArgumentConflict, "{flag}");
        }
        assert!(parse(&["tracel", "run", "--remote", "gpu", "--yes", "--follow"]).is_ok());
    }

    #[test]
    fn the_sdk_environment_names_the_console_and_project() {
        let (output, terminal) = crate::ui::channels(crate::ui::Format::Human, true);
        let context = CliContext::new(terminal, output, Env::Staging(2), None);
        assert_eq!(
            sdk_environment(&context, None),
            [("TRACEL_ENV", "Staging(2)".to_string())]
        );
        let project = TracelProject {
            owner: "alice".to_string(),
            name: "demo".to_string(),
        };
        assert_eq!(
            sdk_environment(&context, Some(&project)),
            [
                ("TRACEL_ENV", "Staging(2)".to_string()),
                ("TRACEL_NAMESPACE", "alice".to_string()),
                ("TRACEL_PROJECT", "demo".to_string()),
            ]
        );
    }
}
