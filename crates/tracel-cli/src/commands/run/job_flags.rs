//! The job's own flags on `tracel run <JOB>`, such as `--epochs 5`: those of the job's command
//! line, which [`job_command`] builds from the job's definition.
//!
//! `tracel run` reads its command line before it knows the job's definition, so each long flag
//! it does not take itself is taken as one of the job's, and read with the job's command line
//! once the job is described. `tracel run` keeps its own flags, so a job field whose flag has
//! the name of one of them is set with `--set` instead.

use std::ffi::OsString;

use clap::builder::Resettable;
use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};
use serde_json::Value;
use tracel_job::{JobDefinition, job_command, job_input};

use crate::error::{CliError, ErrorKind};

/// The help heading of the job's flags in `tracel run <JOB> --help`.
const HEADING: &str = "Job options";

/// `tracel` whose `tracel run` also takes, as flags of its JOB, the long flags among `args` that
/// it does not take itself.
pub fn with_job_flags(tracel: Command, args: &[OsString]) -> Command {
    let run = run_command(&tracel);
    let taken = long_flags(&run);
    let mut names: Vec<&str> = Vec::new();
    let flags = args
        .iter()
        .skip(1)
        .take_while(|arg| *arg != "--")
        .filter_map(|arg| arg.to_str()?.strip_prefix("--"))
        .map(|flag| flag.split_once('=').map_or(flag, |(name, _)| name));
    for name in flags {
        if !name.is_empty()
            && !name.starts_with('-')
            && !taken.contains(&name)
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    tracel.mut_subcommand("run", |run| run.args(names.into_iter().map(job_flag)))
}

/// The job's flags in `matches`, those of `tracel run` that [`with_job_flags`] added, as
/// arguments of the job's command line.
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

/// `input` with the job's flags `flags` set, as the job's command line `<JOB> <input> <flags>`
/// sets them: each flag sets one field, an object it gives is overlaid onto the field's, and
/// `null` is kept.
pub fn apply(
    definition: &JobDefinition,
    input: Value,
    flags: &[String],
) -> Result<Value, CliError> {
    if flags.is_empty() {
        return Ok(input);
    }
    let job = &definition.name;
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
    Ok(job_input(&matches))
}

/// `tracel` whose `tracel run` is that of the job `definition`, for `tracel run <JOB> --help`:
/// with the job's flags, but those `tracel run` takes itself.
pub fn job_help(tracel: Command, definition: &JobDefinition) -> Command {
    let run = run_command(&tracel);
    let taken = long_flags(&run);
    let flags: Vec<Arg> = job_command(definition)
        .get_arguments()
        .filter_map(|arg| {
            let long = arg.get_long()?;
            (!taken.contains(&long)).then(|| {
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

/// The `tracel run` command of `tracel`, built, so it has its help flag and the global flags.
pub fn run_command(tracel: &Command) -> Command {
    let mut tracel = tracel.clone();
    tracel.build();
    tracel
        .find_subcommand("run")
        .expect("`tracel` has a `run` command")
        .clone()
}

/// The long flags `run` takes, with their aliases.
fn long_flags(run: &Command) -> Vec<&str> {
    run.get_arguments()
        .flat_map(|arg| {
            arg.get_long()
                .into_iter()
                .chain(arg.get_all_aliases().unwrap_or_default())
        })
        .collect()
}

/// The argument of `--<name>`, one of the job's flags: given no value, or one, which may be a
/// negative number, as the job's flags take. It needs a JOB, which `--list` cannot be given.
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

/// The message of clap's `error`, without its usage: its lines, trimmed, joined by `; `.
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
    use clap::error::ErrorKind as ClapErrorKind;
    use serde_json::json;
    use tracel_job::JobKind;

    use super::*;
    use crate::cli::{CliArgs, Commands};
    use crate::commands::run::RunArgs;

    /// A job with a nested input, whose `offline` and `targets` fields share their names with
    /// `tracel run`'s flag `--offline` and argument id `targets`.
    fn toy() -> JobDefinition {
        JobDefinition {
            name: "toy".to_string(),
            kind: JobKind::Experiment,
            description: Some("Train a toy model".to_string()),
            input_schema: Some(json!({
                "type": "object",
                "properties": {
                    "epochs": {"description": "Passes over the data.", "type": "integer"},
                    "offline": {"type": "boolean"},
                    "shuffle": {"type": "boolean"},
                    "tag": {"type": "string"},
                    "targets": {"type": "array", "items": {"type": "string"}},
                    "optimizer": {
                        "type": "object",
                        "properties": {"lr": {"type": "number"}},
                        "required": ["lr"]
                    }
                },
                "required": ["epochs", "optimizer", "tag"]
            })),
            input_example: Some(json!({
                "epochs": 10,
                "offline": false,
                "shuffle": false,
                "targets": [],
                "optimizer": {"lr": 0.001}
            })),
        }
    }

    fn parse(arguments: &[&str]) -> Result<RunArgs, clap::Error> {
        let Some(Commands::Run(args)) = CliArgs::try_parse_args(arguments)?.command else {
            panic!("Expected run command");
        };
        Ok(args)
    }

    fn flags(arguments: &[&str]) -> Vec<String> {
        let mut flags = parse(arguments).unwrap().job_flags;
        flags.sort();
        flags
    }

    #[test]
    fn the_command_with_job_flags_is_valid() {
        let args: Vec<OsString> = ["tracel", "run", "toy", "--epochs", "5", "--optimizer.lr=1"]
            .map(OsString::from)
            .into();
        with_job_flags(CliArgs::command(), &args).debug_assert();
        let mut help = job_help(CliArgs::command(), &toy());
        help.build();
        help.debug_assert();
    }

    #[test]
    fn long_flags_tracel_run_does_not_take_are_the_jobs() {
        let args = parse(&[
            "tracel",
            "run",
            "toy",
            "--epochs",
            "5",
            "--offline",
            "--optimizer.lr",
            "-0.5",
            "--json",
            "--set",
            "tag=x",
            "--bin",
            "trainer",
            "-C",
            "/tmp",
            "--tag=-x",
        ])
        .unwrap();
        assert_eq!(args.job.as_deref(), Some("toy"));
        assert!(args.offline);
        assert_eq!(args.package.bin.as_deref(), Some("trainer"));
        assert_eq!(args.assignments.len(), 1);
        let mut job_flags = args.job_flags;
        job_flags.sort();
        assert_eq!(job_flags, ["--epochs=5", "--optimizer.lr=-0.5", "--tag=-x"]);
        let cli =
            CliArgs::try_parse_args(["tracel", "run", "toy", "--epochs", "5", "--json"]).unwrap();
        assert!(cli.json);
    }

    #[test]
    fn a_job_flag_takes_one_value_or_none() {
        assert_eq!(
            flags(&["tracel", "run", "toy", "--shuffle", "--offline"]),
            ["--shuffle"]
        );
        assert_eq!(
            flags(&["tracel", "run", "toy", "--shuffle", "false", "--shuffle"]),
            ["--shuffle", "--shuffle=false"]
        );
        assert_eq!(flags(&["tracel", "run", "toy", "--tag="]), ["--tag="]);
        assert!(flags(&["tracel", "run", "toy"]).is_empty());
    }

    #[test]
    fn job_flags_need_a_job_and_stay_before_double_dash() {
        for (arguments, kind) in [
            (
                &["tracel", "run", "--epochs", "5"][..],
                ClapErrorKind::MissingRequiredArgument,
            ),
            (
                &["tracel", "run", "--list", "--epochs", "5"],
                ClapErrorKind::ArgumentConflict,
            ),
            (
                &["tracel", "run", "toy", "stray"],
                ClapErrorKind::UnknownArgument,
            ),
            (
                &["tracel", "run", "toy", "-x"],
                ClapErrorKind::UnknownArgument,
            ),
            (
                &["tracel", "run", "toy", "--epochs", "5", "--", "x"],
                ClapErrorKind::ArgumentConflict,
            ),
            (
                &["tracel", "--epochs", "5", "run", "toy"],
                ClapErrorKind::UnknownArgument,
            ),
            (
                &["tracel", "jobs", "list", "--epochs", "5"],
                ClapErrorKind::UnknownArgument,
            ),
        ] {
            let error = parse(arguments).unwrap_err();
            assert_eq!(error.kind(), kind, "{arguments:?}");
            assert_eq!(error.exit_code(), 2);
        }
        let args = parse(&["tracel", "run", "--", "toy", "--epochs", "5"]).unwrap();
        assert!(args.job_flags.is_empty());
        assert_eq!(args.forwarded, ["toy", "--epochs", "5"]);
    }

    #[test]
    fn help_is_read_with_the_job_and_its_flags() {
        let args = parse(&["tracel", "run", "toy", "--epochs", "5", "--help"]).unwrap();
        assert!(args.help);
        assert_eq!(args.job.as_deref(), Some("toy"));
        assert_eq!(args.job_flags, ["--epochs=5"]);
        for help in ["-h", "--help"] {
            assert!(parse(&["tracel", "run", help]).unwrap().help);
        }
    }

    #[test]
    fn flags_parse_as_their_type_onto_the_input() {
        let toy = toy();
        let input = apply(
            &toy,
            json!({"epochs": 10, "tag": "a", "optimizer": {"lr": 0.001, "decay": 0.1}}),
            &[
                "--epochs=5".to_string(),
                "--shuffle".to_string(),
                "--optimizer.lr=0.01".to_string(),
                r#"--targets=["cpu"]"#.to_string(),
            ],
        )
        .unwrap();
        assert_eq!(
            input,
            json!({
                "epochs": 5,
                "shuffle": true,
                "tag": "a",
                "targets": ["cpu"],
                "optimizer": {"lr": 0.01, "decay": 0.1}
            })
        );
        assert_eq!(
            apply(&toy, json!({"epochs": 1}), &[]).unwrap(),
            json!({"epochs": 1})
        );
        assert_eq!(
            apply(&toy, Value::Null, &["--tag=x".to_string()]).unwrap(),
            json!({"tag": "x"})
        );
    }

    #[test]
    fn unusable_flags_are_usage_errors_naming_the_flag() {
        let toy = toy();
        for (flag, message) in [
            (
                "--epochs=ten",
                "invalid value 'ten' for '--epochs <INT>': expected an integer",
            ),
            (
                "--epoch=5",
                "unexpected argument '--epoch' found; tip: a similar argument exists: '--epochs'",
            ),
            (
                "--tag",
                "a value is required for '--tag <STRING>' but none was supplied",
            ),
        ] {
            let error = apply(&toy, json!({}), &[flag.to_string()]).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Usage, "{flag}");
            assert_eq!(error.to_string(), message);
            let error = anyhow::Error::from(error);
            assert_eq!(
                crate::error::ErrorReport::new(&error).hint,
                Some("List the flags of job 'toy' with `tracel run toy --help`.")
            );
        }
    }

    #[test]
    fn a_field_whose_flag_tracel_run_takes_is_set_with_set() {
        let args = parse(&["tracel", "run", "toy", "--offline"]).unwrap();
        assert!(args.offline);
        assert!(args.job_flags.is_empty());

        let input = super::super::input::resolve(
            &toy(),
            json!({"offline": false}),
            &[],
            &[],
            &[super::super::input::parse_assignment("offline=true").unwrap()],
        )
        .unwrap();
        assert_eq!(input, json!({"offline": true}));
    }

    #[test]
    fn the_job_help_lists_the_jobs_flags_beside_tracel_runs() {
        let help = run_command(&job_help(CliArgs::command(), &toy()))
            .render_long_help()
            .to_string();

        for text in [
            "Train a toy model",
            "Usage: tracel run toy [OPTIONS]",
            "Job options:",
            "--epochs <INT>",
            "Passes over the data.",
            "[default: 10]",
            "--optimizer.lr <FLOAT>",
            "--shuffle [<BOOL>]",
            "--targets <JSON>",
            "--set <PATH=VALUE>",
            "--remote <GROUP>",
            "--json",
        ] {
            assert!(help.contains(text), "no {text} in\n{help}");
        }
        assert_eq!(help.matches("--offline").count(), 1, "{help}");
        assert_eq!(help.matches("--config <FILE>").count(), 1, "{help}");
        for text in ["--list", "[JOB]", "[ARGS]", "JSON merged before the input"] {
            assert!(!help.contains(text), "{text} in\n{help}");
        }
    }

    #[test]
    fn without_a_job_the_help_is_that_of_tracel_run() {
        let help = run_command(&CliArgs::command())
            .render_long_help()
            .to_string();
        assert!(
            help.contains("Usage: tracel run [OPTIONS] [JOB] [-- <ARGS>...]"),
            "{help}"
        );
        assert!(help.contains("--list"), "{help}");
        assert!(help.contains("-h, --help"), "{help}");
        assert!(!help.contains("Job options"), "{help}");
    }
}
