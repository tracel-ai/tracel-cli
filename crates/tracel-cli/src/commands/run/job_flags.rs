//! The job's own flags on `tracel run <job>`, such as `--epochs 5`: those of the job's
//! command line, which [`job_command`] builds from the job's definition. Only a job whose
//! input has a JSON schema has flags.
//!
//! `tracel run` reads its command line before it knows the job's definition, so each long
//! flag it doesn't take itself is taken as one of the job's, and read with the job's
//! command line once the job is described. `tracel run` keeps its own flags, so a job
//! field whose flag has the name of one of them is set with `--set` instead.

use std::ffi::OsString;

use clap::builder::Resettable;
use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};
use serde_json::Value;
use tracel_job::JobDefinition;
use tracel_job::cli::{job_command, job_input};

use crate::error::{CliError, ErrorKind};

/// The help heading of the job's flags in `tracel run <JOB> --help`.
const HEADING: &str = "Job options";

/// Make `tracel run` take, as flags of its job, the long flags among `args` that it
/// doesn't take itself.
pub fn with_job_flags(tracel: Command, args: &[OsString]) -> Command {
    let taken = run_flags(&tracel);
    let mut names: Vec<&str> = Vec::new();
    let flags = args
        .iter()
        .skip(1)
        .take_while(|arg| *arg != "--")
        .filter_map(|arg| arg.to_str()?.strip_prefix("--"))
        .map(flag_name);
    for name in flags {
        if !name.is_empty()
            && !name.starts_with('-')
            && !taken.iter().any(|taken| taken == name)
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    tracel.mut_subcommand("run", |run| run.args(names.into_iter().map(job_flag)))
}

/// Read the job's flags in `matches`, those [`with_job_flags`] added, as arguments of the
/// job's command line.
pub fn job_flags(matches: &ArgMatches) -> Vec<String> {
    let mut arguments = Vec::new();
    for flag in matches.ids().map(|id| id.as_str()) {
        if !flag.starts_with("--") {
            continue;
        }
        for mut values in matches
            .get_occurrences::<String>(flag)
            .into_iter()
            .flatten()
        {
            arguments.push(match values.next() {
                Some(value) => format!("{flag}={value}"),
                None => flag.to_string(),
            });
        }
    }
    arguments
}

/// Set the job's flags `flags` on `input`, as the job's command line `<job> <input>
/// <flags>` sets them: each flag sets one field, an object it gives is laid over the
/// field's, and `null` is kept. A job whose input has no schema has no flags.
pub fn apply(
    definition: &JobDefinition,
    input: Value,
    flags: &[String],
) -> Result<Value, CliError> {
    let Some(flag) = flags.first() else {
        return Ok(input);
    };
    let job = &definition.name;
    if definition.input_schema.is_none() {
        let flag = flag_name(flag);
        return Err(CliError::new(
            ErrorKind::Usage,
            format!("Job '{job}' has no flag {flag}: its input has no JSON schema."),
        )
        .with_hint(
            "Set its input with `--set <PATH>=<VALUE>`, or give the job's input a JSON schema with `Job::with_schema`.",
        ));
    }
    let arguments = [job.clone(), input.to_string()]
        .into_iter()
        .chain(flags.iter().cloned());
    let matches = job_command(definition)
        .disable_help_flag(true)
        .try_get_matches_from(arguments)
        .map_err(|error| {
            CliError::new(ErrorKind::Usage, message(&error)).with_hint(format!(
                "List the flags of job '{job}' with `tracel run {job} --help`."
            ))
        })?;
    job_input(&matches, |path| std::fs::read_to_string(path))
        .map_err(|error| CliError::new(ErrorKind::Usage, error.to_string()))
}

/// Make `tracel run` that of the job `definition`, for `tracel run <job> --help`: with the
/// job's flags, but those `tracel run` takes itself.
pub fn job_help(tracel: Command, definition: &JobDefinition) -> Command {
    let taken = run_flags(&tracel);
    let flags: Vec<Arg> = job_command(definition)
        .get_arguments()
        .filter_map(|arg| {
            let long = arg.get_long()?;
            (!taken.iter().any(|taken| taken == long)).then(|| {
                arg.clone()
                    .id(format!("--{long}"))
                    .required_unless_present(Resettable::Reset)
                    .help_heading(HEADING)
            })
        })
        .collect();
    let usage = format!("tracel run {} [OPTIONS]", definition.name);
    tracel.mut_subcommand("run", |run| {
        let about = match &definition.description {
            Some(description) => Resettable::Value(description.into()),
            None => Resettable::Reset,
        };
        run.about(about)
            .override_usage(usage)
            .mut_arg("job", |arg| arg.hide(true))
            .mut_arg("list", |arg| arg.hide(true))
            .mut_arg("forwarded", |arg| arg.hide(true))
            .args(flags)
    })
}

/// Find the `tracel run` command of `tracel`, built, so it has its help flag and the
/// global flags.
pub fn run_command(tracel: &Command) -> Command {
    let mut tracel = tracel.clone();
    tracel.build();
    tracel
        .find_subcommand("run")
        .expect("`tracel` has a `run` command")
        .clone()
}

/// List the long flags `tracel run` takes, with their aliases.
fn run_flags(tracel: &Command) -> Vec<String> {
    run_command(tracel)
        .get_arguments()
        .flat_map(|arg| {
            arg.get_long()
                .into_iter()
                .chain(arg.get_all_aliases().unwrap_or_default())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Take the name of a long flag written `name` or `name=value`.
fn flag_name(flag: &str) -> &str {
    flag.split_once('=').map_or(flag, |(name, _)| name)
}

/// Make the argument of `--<name>`, one of the job's flags: given no value, or one, which
/// may be a negative number, as the job's flags take. It needs a job, which `--list`
/// can't be given.
fn job_flag(name: &str) -> Arg {
    Arg::new(format!("--{name}"))
        .long(name.to_string())
        .value_name("VALUE")
        .num_args(0..=1)
        .action(ArgAction::Append)
        .value_parser(value_parser!(String))
        .allow_negative_numbers(true)
        .requires("job")
        .conflicts_with("list")
        .hide(true)
}

/// Write the message of clap's `error` without its usage: its lines, trimmed, joined by
/// `; `.
fn message(error: &clap::Error) -> String {
    let rendered = error.to_string();
    let message = rendered.split("\nUsage:").next().unwrap_or_default().trim();
    let message = message.strip_prefix("error: ").unwrap_or(message);
    message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;
    use rstest::rstest;
    use serde_json::json;

    use super::*;
    use crate::cli::{CliArgs, Commands};
    use crate::error::ErrorReport;
    use crate::test_support::{job, parse};

    fn train() -> JobDefinition {
        let schema = json!({
            "type": "object",
            "properties": {
                "epochs": {"type": "integer"},
                "shuffle": {"type": "boolean"},
                "tag": {"type": "string"},
                "optimizer": {
                    "type": "object",
                    "properties": {"lr": {"type": "number"}}
                }
            }
        });
        job("train", Some(schema), Some(json!({"epochs": 10})))
    }

    fn job_flags_of(args: &[&str]) -> Vec<String> {
        match parse(args).unwrap().command {
            Some(Commands::Run(run)) => run.job_flags,
            other => panic!("{args:?} parse as {other:?}"),
        }
    }

    #[test]
    fn given_job_flags_when_clap_checks_tracel_run_then_it_is_valid() {
        let args: Vec<OsString> = ["tracel", "run", "train", "--epochs", "5"]
            .map(OsString::from)
            .into();

        let tracel = with_job_flags(CliArgs::command(), &args);

        tracel.debug_assert();
    }

    #[test]
    fn given_a_jobs_help_when_clap_checks_it_then_it_is_valid() {
        let mut help = job_help(CliArgs::command(), &train());

        help.build();

        help.debug_assert();
    }

    #[test]
    fn given_flags_tracel_run_does_not_take_when_parsing_it_then_they_are_the_jobs() {
        let flags = job_flags_of(&[
            "run",
            "train",
            "--epochs",
            "5",
            "--offline",
            "--optimizer.lr",
            "-0.5",
        ]);

        assert_eq!(flags, ["--epochs=5", "--optimizer.lr=-0.5"]);
    }

    #[test]
    fn given_a_job_flag_without_a_value_when_parsing_tracel_run_then_it_is_kept_bare() {
        let flags = job_flags_of(&["run", "train", "--shuffle"]);

        assert_eq!(flags, ["--shuffle"]);
    }

    #[test]
    fn given_job_flags_when_applying_them_then_each_sets_its_field_with_its_type() {
        let input = json!({"epochs": 10, "optimizer": {"lr": 0.001, "decay": 0.1}});
        let flags = [
            "--epochs=5".to_string(),
            "--shuffle".to_string(),
            "--optimizer.lr=0.01".to_string(),
        ];

        let input = apply(&train(), input, &flags).unwrap();

        assert_eq!(
            input,
            json!({"epochs": 5, "shuffle": true, "optimizer": {"lr": 0.01, "decay": 0.1}})
        );
    }

    #[rstest]
    #[case::wrong_type("--epochs=ten")]
    #[case::unknown_flag("--epoch=5")]
    #[case::missing_value("--tag")]
    fn given_a_flag_the_job_cannot_take_when_applying_it_then_the_hint_points_at_the_jobs_help(
        #[case] flag: &str,
    ) {
        let error =
            anyhow::Error::from(apply(&train(), json!({}), &[flag.to_string()]).unwrap_err());

        let hint = ErrorReport::new(&error).hint.unwrap();

        assert!(hint.contains("tracel run train --help"), "{hint}");
    }

    #[test]
    fn given_a_job_without_an_input_schema_when_applying_a_flag_then_it_has_no_flags() {
        let evaluate = job("evaluate", None, Some(json!({"split": "test"})));

        let error = apply(&evaluate, json!({}), &["--split=valid".to_string()]).unwrap_err();

        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(error.to_string().contains("--split"), "{error}");
    }
}
