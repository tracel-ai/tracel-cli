use std::io::Write;

use clap::ValueEnum;
use serde::Serialize;

use crate::error::{CliError, ErrorKind, ErrorReport};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputMode {
    #[default]
    Auto,
    Human,
    Json,
}

impl OutputMode {
    pub fn resolve(
        flag: Option<Self>,
        json: bool,
        environment: Option<&str>,
        stdout_is_terminal: bool,
    ) -> Result<Self, CliError> {
        let mode = if json {
            Self::Json
        } else if let Some(mode) = flag {
            mode
        } else {
            match environment {
                Some("human") => Self::Human,
                Some("json") => Self::Json,
                None => Self::Auto,
                Some(value) => {
                    return Err(CliError::new(
                        ErrorKind::Usage,
                        format!("Invalid TRACEL_OUTPUT value '{value}': expected human or json."),
                    ));
                }
            }
        };

        Ok(match mode {
            Self::Auto if stdout_is_terminal => Self::Human,
            Self::Auto => Self::Json,
            mode => mode,
        })
    }
}

#[derive(Serialize)]
struct Success<'a, T> {
    ok: bool,
    data: &'a T,
}

#[derive(Serialize)]
struct Failure<'a> {
    ok: bool,
    error: &'a ErrorReport<'a>,
}

pub fn write_success(data: &impl Serialize) -> anyhow::Result<()> {
    write_line(&Success { ok: true, data })
}

pub fn write_failure(error: &ErrorReport<'_>) -> anyhow::Result<()> {
    write_line(&Failure { ok: false, error })
}

fn write_line(value: &impl Serialize) -> anyhow::Result<()> {
    let line = serde_json::to_string(value)?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_uses_stdout_terminal_status() {
        assert_eq!(
            OutputMode::resolve(None, false, None, true).unwrap(),
            OutputMode::Human
        );
        assert_eq!(
            OutputMode::resolve(None, false, None, false).unwrap(),
            OutputMode::Json
        );
    }

    #[test]
    fn flags_override_environment() {
        for environment in [Some("human"), Some("json"), Some("invalid")] {
            assert_eq!(
                OutputMode::resolve(Some(OutputMode::Human), false, environment, false).unwrap(),
                OutputMode::Human
            );
            assert_eq!(
                OutputMode::resolve(None, true, environment, true).unwrap(),
                OutputMode::Json
            );
            assert_eq!(
                OutputMode::resolve(Some(OutputMode::Auto), false, environment, false).unwrap(),
                OutputMode::Json
            );
        }
    }

    #[test]
    fn environment_selects_mode_or_reports_usage() {
        assert_eq!(
            OutputMode::resolve(None, false, Some("human"), false).unwrap(),
            OutputMode::Human
        );
        assert_eq!(
            OutputMode::resolve(None, false, Some("json"), true).unwrap(),
            OutputMode::Json
        );
        let error = OutputMode::resolve(None, false, Some("auto"), true).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Usage);
    }
}
