mod describe;
mod input;
mod job_flags;
mod launch;
mod remote;

use std::io;
use std::path::PathBuf;

use anyhow::Context;
use clap::{ArgAction, ArgGroup, Args, Command};
use serde::Serialize;
use serde_json::Value;
use tracel_client::console::Env;
use tracel_job::{DefinitionsFile, JobDefinition};

pub use job_flags::{job_flags, with_job_flags};

use crate::commands::experiments::{ExperimentSelector, get_experiment, parse_experiment};
use crate::commands::login::get_client_and_login_if_needed;
use crate::commands::package::PackageArgs;
use crate::context::CliContext;
use crate::environment;
use crate::error::{CliError, ErrorKind, classify};
use crate::helpers::{job_command, require_cargo_workspace, resolve_namespace_project};
use crate::tools::cargo;
use crate::tools::tracel_config::TracelProject;
use crate::ui::{Human, Layout, Outcome, Render, Table, Terminal, list_summary};

use describe::{describe, find_job};
use input::Assignment;
use launch::{Launch, Target, exit_error, launch, select_target};
use remote::RemoteRun;

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("packaging")
        .args(["mode", "targets", "install_targets"])
        .multiple(true)
        .requires("remote")
))]
#[command(group(ArgGroup::new("selection").args(["job", "list", "remote"]).multiple(true)))]
#[command(mut_arg("bin", |arg| arg.requires("selection")))]
#[command(disable_help_flag = true)]
pub struct RunArgs {
    /// Job to run, then its own flags
    #[arg(value_name = "JOB", conflicts_with = "forwarded")]
    pub job: Option<String>,
    /// The job's own flags, as arguments of the job's command line, such as `--epochs=5`.
    #[arg(skip)]
    pub job_flags: Vec<String>,
    /// List the jobs your program defines
    #[arg(long, conflicts_with_all = ["job", "remote", "forwarded"])]
    pub list: bool,
    /// Merge this JSON file onto the input; repeat for more
    #[arg(short = 'c', long = "config", value_name = "FILE", requires = "job")]
    pub configs: Vec<PathBuf>,
    /// Set an input value, such as optimizer.lr=0.01; repeat for more
    #[arg(
        long = "set",
        value_name = "PATH=VALUE",
        value_parser = input::parse_assignment,
        requires = "job"
    )]
    pub assignments: Vec<Assignment>,
    /// Start from the input this experiment recorded
    #[arg(long, value_name = "NUM|latest", value_parser = parse_experiment, requires = "job")]
    pub like: Option<ExperimentSelector>,
    /// Record the run on this machine
    #[arg(long, requires = "job", conflicts_with = "remote")]
    pub offline: bool,
    /// Queue the job on this compute provider group
    #[arg(long, value_name = "GROUP")]
    pub remote: Option<String>,
    #[command(flatten)]
    pub package: PackageArgs,
    /// Queue without asking
    #[arg(long, short = 'y', requires = "remote")]
    pub yes: bool,
    /// Package it without uploading or queuing anything
    #[arg(long, requires = "remote", conflicts_with_all = ["follow", "yes"])]
    pub dry_run: bool,
    /// Follow its logs until it finishes
    #[arg(long, requires = "remote")]
    pub follow: bool,
    /// Arguments for the program, without a job
    #[arg(last = true, value_name = "ARGS")]
    pub forwarded: Vec<String>,
    /// Print help, with a job's own flags
    #[arg(short, long, action = ArgAction::SetTrue)]
    pub help: bool,
}

impl RunArgs {
    /// Choose how `run` shows on a terminal. A run of the workspace without a job draws
    /// nothing: its output is the program's.
    pub fn layout(&self) -> Layout {
        match (&self.job, &self.remote) {
            _ if self.list => Layout::Read,
            (Some(job), Some(group)) => Layout::flow(format!("Run job '{job}' on {group}")),
            (Some(job), None) => Layout::flow(format!("Run job '{job}'")),
            (None, Some(group)) => Layout::flow(format!("Run the workspace on {group}")),
            (None, None) => Layout::Bare,
        }
    }

    /// Make the run on `group` of a job, or of the workspace, whose program runs with
    /// `command`. A `bin` found for the job is the one packaged.
    fn remote_run<'a>(
        &self,
        group: &'a str,
        command: String,
        bin: Option<String>,
    ) -> RemoteRun<'a> {
        RemoteRun {
            group,
            command,
            packaging: PackageArgs {
                bin: bin.or_else(|| self.package.bin.clone()),
                ..self.package.clone()
            },
            yes: self.yes,
            dry_run: self.dry_run,
            follow: self.follow,
        }
    }
}

pub fn handle_command(args: RunArgs, context: CliContext) -> anyhow::Result<Outcome> {
    if let Some(group) = &args.remote {
        let asks = !args.yes && !args.dry_run;
        remote::check(group, &args.package, asks, context.terminal())?;
    }
    if args.list {
        let workspace = require_cargo_workspace()?;
        let bin = args.package.bin.as_deref();
        return Ok(describe(context.terminal(), &workspace, bin)?
            .definitions
            .into());
    }
    match (&args.job, &args.remote) {
        (Some(job), _) => run_job(job, &args, &context),
        (None, Some(group)) => {
            let run = args.remote_run(group, job_command(&args.forwarded), None);
            remote::queue(run, &context)
        }
        (None, None) => {
            run_locally(&args.forwarded, &context)?;
            Ok(Outcome::streamed())
        }
    }
}

impl Render for DefinitionsFile {
    fn render(&self, out: &mut Human<'_>) -> io::Result<()> {
        Table::new(["NAME", "DESCRIPTION"])
            .shrink("DESCRIPTION")
            .rows(self.jobs.iter().map(|job| {
                [
                    job.name.clone(),
                    job.description.clone().unwrap_or_default(),
                ]
            }))
            .write(out)
    }

    fn summary(&self) -> Option<String> {
        Some(list_summary(
            self.jobs.len(),
            self.jobs.len() as u64,
            ["job", "jobs"],
            "The program defines no jobs.",
        ))
    }
}

/// The data of a local job run.
#[derive(Serialize)]
struct JobRun {
    job: String,
    input: Value,
    target: Target,
    exit_code: Option<i32>,
}

/// Run `job` of the workspace binary with its input, on this machine or with --remote.
fn run_job(job: &str, args: &RunArgs, context: &CliContext) -> anyhow::Result<Outcome> {
    let terminal = context.terminal();
    let workspace = require_cargo_workspace()?;
    let described = describe(terminal, &workspace, args.package.bin.as_deref())?;
    let definition = find_job(&described.definitions, job)?;
    let start = match &args.like {
        Some(experiment) => recorded_input(context, experiment)?,
        None => definition.input_example.clone().unwrap_or(Value::Null),
    };
    let input = resolve_input(definition, start, args, terminal)?;
    let input_json = serde_json::to_string(&input)?;

    if let Some(group) = &args.remote {
        let command = job_command(&[job.to_string(), input_json]);
        let run = args.remote_run(group, command, Some(described.binary.name));
        return remote::queue(run, context);
    }

    let project = linked_project(context)?;
    let logged_in = !args.offline && project.is_some() && context.has_credentials()?;
    let (target, note) = select_target(args.offline, project.is_some(), logged_in);
    if let Some(note) = note {
        terminal.info(note);
    }
    let mut env = sdk_environment(&context.environment(), project.as_ref());
    env.push(("TRACEL_TARGET", target.as_str().to_string()));
    let exit = launch(Launch {
        program: &described.program,
        job,
        input: &input_json,
        env,
        stdout: context.output().child_stdout(),
    })?;
    terminal.program_ended(exit.stopped);
    if let Some(error) = exit_error(job, &exit) {
        return Err(error.into());
    }
    terminal.outro(&format!("Job '{job}' completed."));
    Ok(Outcome::data(JobRun {
        job: job.to_string(),
        input,
        target,
        exit_code: exit.status.code(),
    }))
}

/// Resolve the input job `definition` runs with from `start`, with the configs, job
/// flags and assignments of `args`, and check it against the job's schema. An input the
/// schema rejects stops the run; a schema that can't be used is only warned about.
fn resolve_input(
    definition: &JobDefinition,
    start: Value,
    args: &RunArgs,
    terminal: &Terminal,
) -> anyhow::Result<Value> {
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
    let job = &definition.name;
    if let Some(schema) = &definition.input_schema {
        match input::violations(schema, &input) {
            Ok(violations) if violations.is_empty() => {}
            Ok(violations) => return Err(input::invalid_input(job, &violations).into()),
            Err(error) => terminal.warning(&format!(
                "Can't check the input of job '{job}' against its schema: {error}"
            )),
        }
    }
    Ok(input)
}

/// Build the command whose help `tracel run --help` prints: after a job, with the job's
/// own flags.
pub fn help(args: &RunArgs, tracel: Command, terminal: &Terminal) -> anyhow::Result<Command> {
    let tracel = match &args.job {
        Some(job) => {
            let workspace = require_cargo_workspace()?;
            let described = describe(terminal, &workspace, args.package.bin.as_deref())?;
            job_flags::job_help(tracel, find_job(&described.definitions, job)?)
        }
        None => tracel,
    };
    Ok(job_flags::run_command(&tracel))
}

/// Find the project from --project, the environment or `tracel.toml`, or `None` when
/// none is set.
fn linked_project(context: &CliContext) -> anyhow::Result<Option<TracelProject>> {
    match resolve_namespace_project(context) {
        Ok(resolved) => Ok(Some(resolved.project)),
        Err(error) if classify(&error) == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Read the input the experiment `selector` names recorded.
fn recorded_input(context: &CliContext, selector: &ExperimentSelector) -> anyhow::Result<Value> {
    let project = resolve_namespace_project(context)?.project;
    let client = get_client_and_login_if_needed(context)?;
    let experiment = get_experiment(&client, &project, selector)?;
    Ok(recorded(experiment.experiment_num, experiment.config)?)
}

/// Take the input experiment `num` recorded as `config`, which is `null` when it
/// recorded none.
fn recorded(num: i32, config: Value) -> Result<Value, CliError> {
    if config.is_null() {
        return Err(CliError::new(
            ErrorKind::NotFound,
            format!("Experiment {num} recorded no input."),
        )
        .with_hint("Leave out --like to start from the job's example input."));
    }
    Ok(config)
}

/// Run `cargo run --release` with the arguments given after `--`: its output and exit
/// code are the command's.
fn run_locally(forwarded: &[String], context: &CliContext) -> anyhow::Result<()> {
    let mut cmd = cargo::command();
    cmd.arg("run").arg("--release");
    cmd.envs(sdk_environment(&context.environment(), context.project()));
    if !forwarded.is_empty() {
        cmd.arg("--");
        cmd.args(forwarded);
    }

    hand_over(cmd)
}

/// List the variables that name the Tracel Console and the project to the SDK.
fn sdk_environment(env: &Env, project: Option<&TracelProject>) -> Vec<(&'static str, String)> {
    let mut variables = vec![("TRACEL_ENV", environment::sdk_value(env))];
    if let Some(project) = project {
        variables.push(("TRACEL_NAMESPACE", project.namespace.clone()));
        variables.push(("TRACEL_PROJECT", project.name.clone()));
    }
    variables
}

/// Replace this process with `cmd`. `cargo run` does the same with the program it
/// builds, so a signal sent to `tracel` reaches the program instead of orphaning it.
#[cfg(unix)]
fn hand_over(mut cmd: std::process::Command) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;

    Err(cmd.exec()).context("Can't run `cargo run`")
}

/// Windows can't replace a process, so wait for the child and exit with its code.
#[cfg(not(unix))]
fn hand_over(mut cmd: std::process::Command) -> anyhow::Result<()> {
    let status = cmd.status().context("Can't run `cargo run`")?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;
    use clap::error::ErrorKind;
    use serde_json::json;
    use tracel_job::JobDefinition;

    use tracel_client::console::Env;

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
    fn the_job_list_names_and_describes_each_job() {
        let job = |name: &str, description: Option<&str>| JobDefinition {
            name: name.to_string(),
            description: description.map(str::to_string),
            input_schema: None,
            input_example: None,
        };
        let definitions = DefinitionsFile::new(vec![
            job("toy-training", Some("Run a toy training loop")),
            job("wordtok", None),
        ]);

        let mut text = Vec::new();
        definitions
            .render(&mut Human::plain(&mut text, None))
            .unwrap();

        assert_eq!(
            String::from_utf8(text).unwrap(),
            "NAME          DESCRIPTION\n\
             toy-training  Run a toy training loop\n\
             wordtok       -\n"
        );
    }

    #[test]
    fn the_sdk_environment_names_the_console_and_the_project() {
        let project = TracelProject {
            namespace: "alice".to_string(),
            name: "demo".to_string(),
        };
        let console = ("TRACEL_ENV", "Staging(2)".to_string());
        assert_eq!(
            sdk_environment(&Env::Staging(2), None),
            std::slice::from_ref(&console)
        );
        assert_eq!(
            sdk_environment(&Env::Staging(2), Some(&project)),
            [
                console,
                ("TRACEL_NAMESPACE", "alice".to_string()),
                ("TRACEL_PROJECT", "demo".to_string()),
            ]
        );
    }
}
