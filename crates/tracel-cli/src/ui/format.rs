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
    use super::*;

    #[test]
    fn json_then_output_then_tracel_output_choose_the_format() {
        use FormatArg::{Auto, Human as HumanArg, Json as JsonArg};
        for (flag, json, environment, auto, expected) in [
            (None, false, None, Format::Human, Some(Format::Human)),
            (None, false, None, Format::Json, Some(Format::Json)),
            (
                Some(Auto),
                false,
                Some("json"),
                Format::Human,
                Some(Format::Human),
            ),
            (
                None,
                false,
                Some("human"),
                Format::Json,
                Some(Format::Human),
            ),
            (None, false, Some("json"), Format::Human, Some(Format::Json)),
            (
                Some(HumanArg),
                false,
                Some("json"),
                Format::Json,
                Some(Format::Human),
            ),
            (
                Some(JsonArg),
                false,
                Some("human"),
                Format::Human,
                Some(Format::Json),
            ),
            (
                Some(HumanArg),
                false,
                Some("xml"),
                Format::Json,
                Some(Format::Human),
            ),
            (None, true, Some("human"), Format::Human, Some(Format::Json)),
            (None, true, Some("xml"), Format::Human, Some(Format::Json)),
            (None, false, Some("xml"), Format::Human, None),
            (None, false, Some("auto"), Format::Human, None),
        ] {
            let case = (flag, json, environment, auto);
            let resolved = Format::resolve(flag, json, environment, auto);
            assert_eq!(resolved.as_ref().ok().copied(), expected, "{case:?}");
            if let Err(error) = resolved {
                assert_eq!(error.kind, ErrorKind::Usage, "{case:?}");
            }
        }
    }
}
