use clap::ValueEnum;

use crate::error::{CliError, ErrorKind};

/// The `--output` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum FormatArg {
    Auto,
    Human,
    Json,
}

/// How a command writes its result and errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Human,
    Json,
}

impl Format {
    /// Choose the format from `--json`, then `--output`, then `TRACEL_OUTPUT`. `auto`, or
    /// no choice, leaves it to the command.
    pub fn resolve(
        flag: Option<FormatArg>,
        json: bool,
        environment: Option<&str>,
        auto: Format,
    ) -> Result<Self, CliError> {
        if json {
            return Ok(Self::Json);
        }
        let choice = match flag {
            Some(choice) => choice,
            None => match environment {
                None => FormatArg::Auto,
                Some("human") => FormatArg::Human,
                Some("json") => FormatArg::Json,
                Some(value) => {
                    return Err(CliError::new(
                        ErrorKind::Usage,
                        format!("Unknown TRACEL_OUTPUT '{value}'. Formats: human, json."),
                    ));
                }
            },
        };
        Ok(match choice {
            FormatArg::Auto => auto,
            FormatArg::Human => Self::Human,
            FormatArg::Json => Self::Json,
        })
    }

    /// Choose human output for a terminal and JSON for a program reading stdout.
    pub fn for_stdout(stdout_is_terminal: bool) -> Self {
        if stdout_is_terminal {
            Self::Human
        } else {
            Self::Json
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn given_json_when_choosing_the_format_then_it_is_json_whatever_else_says() {
        let format = Format::resolve(Some(FormatArg::Human), true, Some("human"), Format::Human);

        assert_eq!(format.unwrap(), Format::Json);
    }

    #[rstest]
    #[case::human_over_json(FormatArg::Human, "json", Format::Human)]
    #[case::json_over_human(FormatArg::Json, "human", Format::Json)]
    #[case::auto_over_json(FormatArg::Auto, "json", Format::Human)]
    fn given_output_when_choosing_the_format_then_it_wins_over_tracel_output(
        #[case] flag: FormatArg,
        #[case] tracel_output: &str,
        #[case] expected: Format,
    ) {
        let format = Format::resolve(Some(flag), false, Some(tracel_output), Format::Human);

        assert_eq!(format.unwrap(), expected);
    }

    #[test]
    fn given_only_tracel_output_when_choosing_the_format_then_it_decides() {
        let format = Format::resolve(None, false, Some("json"), Format::Human);

        assert_eq!(format.unwrap(), Format::Json);
    }

    #[test]
    fn given_no_choice_when_choosing_the_format_then_it_is_the_commands_default() {
        let format = Format::resolve(None, false, None, Format::Json);

        assert_eq!(format.unwrap(), Format::Json);
    }

    #[test]
    fn given_an_unknown_tracel_output_when_choosing_the_format_then_it_is_a_usage_error() {
        let error = Format::resolve(None, false, Some("xml"), Format::Human).unwrap_err();

        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(error.to_string().contains("'xml'"), "{error}");
    }
}
