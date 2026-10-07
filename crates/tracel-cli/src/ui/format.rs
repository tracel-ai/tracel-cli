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
    /// `--json`, then `--output`, then `TRACEL_OUTPUT`. `auto` and no choice at all
    /// mean `auto`, which the command decides.
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
                        format!("Invalid TRACEL_OUTPUT value '{value}': expected human or json."),
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

    /// Human output for a person at a terminal, JSON for a program reading stdout.
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
    fn auto_is_the_command_default() {
        for auto in [Format::Human, Format::Json] {
            assert_eq!(Format::resolve(None, false, None, auto).unwrap(), auto);
            assert_eq!(
                Format::resolve(Some(FormatArg::Auto), false, Some("json"), auto).unwrap(),
                auto
            );
        }
        assert_eq!(Format::for_stdout(true), Format::Human);
        assert_eq!(Format::for_stdout(false), Format::Json);
    }

    #[test]
    fn flags_override_environment() {
        for environment in [Some("human"), Some("json"), Some("invalid")] {
            assert_eq!(
                Format::resolve(Some(FormatArg::Human), false, environment, Format::Json).unwrap(),
                Format::Human
            );
            assert_eq!(
                Format::resolve(None, true, environment, Format::Human).unwrap(),
                Format::Json
            );
            assert_eq!(
                Format::resolve(Some(FormatArg::Auto), false, environment, Format::Json).unwrap(),
                Format::Json
            );
        }
    }

    #[test]
    fn environment_selects_format_or_reports_usage() {
        assert_eq!(
            Format::resolve(None, false, Some("human"), Format::Json).unwrap(),
            Format::Human
        );
        assert_eq!(
            Format::resolve(None, false, Some("json"), Format::Human).unwrap(),
            Format::Json
        );
        let error = Format::resolve(None, false, Some("auto"), Format::Human).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Usage);
    }
}
