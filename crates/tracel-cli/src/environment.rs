//! The Tracel Console environments a command can target, and how each one is written.

use tracel_client::console::Env;
use url::Url;

use crate::error::{CliError, ErrorKind};

/// Choose the environment `--dev` or `--staging` selects, else the one `TRACEL_ENV`
/// names, else production.
pub fn resolve(dev: bool, staging: Option<u8>, variable: Option<&str>) -> Result<Env, CliError> {
    if dev {
        return Ok(Env::Development);
    }
    if let Some(version) = staging {
        return Ok(Env::Staging(version));
    }
    match variable {
        None => Ok(Env::Production),
        Some(value) => parse(value).ok_or_else(|| {
            CliError::new(ErrorKind::Usage, format!("Unknown TRACEL_ENV '{value}'."))
        }),
    }
}

/// Name an environment as JSON data and the terminal show it: "production",
/// "development" or "staging-N".
pub fn name(environment: &Env) -> String {
    match environment {
        Env::Production => "production".to_string(),
        Env::Development => "development".to_string(),
        Env::Staging(version) => format!("staging-{version}"),
    }
}

/// Write an environment as the SDK reads `TRACEL_ENV`: "Production", "Development" or
/// "Staging(N)".
pub fn sdk_value(environment: &Env) -> String {
    match environment {
        Env::Production => "Production".to_string(),
        Env::Development => "Development".to_string(),
        Env::Staging(version) => format!("Staging({version})"),
    }
}

/// Locate an environment's web Console. A development server serves it on port 9000,
/// beside its API on 9001.
pub fn web_console(environment: &Env) -> Url {
    let url = match environment {
        Env::Production => "https://console.tracel.ai/".to_string(),
        Env::Staging(version) => format!("https://s{version}-console.tracel.ai/"),
        Env::Development => "http://localhost:9000/".to_string(),
    };
    Url::parse(&url).expect("a web Console URL is valid")
}

/// Read an environment written as `name` or `sdk_value` writes it.
fn parse(value: &str) -> Option<Env> {
    match value {
        "production" | "Production" => Some(Env::Production),
        "development" | "Development" => Some(Env::Development),
        _ => value
            .strip_prefix("staging-")
            .or_else(|| value.strip_prefix("Staging(")?.strip_suffix(')'))
            .and_then(|version| version.parse().ok())
            .map(Env::Staging),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::report;

    #[test]
    fn flags_choose_the_environment_before_tracel_env() {
        for (dev, staging, variable, expected) in [
            (false, None, None, "production"),
            (false, None, Some("development"), "development"),
            (true, None, Some("production"), "development"),
            (true, None, Some("bogus"), "development"),
            (false, Some(2), Some("Development"), "staging-2"),
            (false, Some(2), Some("bogus"), "staging-2"),
        ] {
            let environment = resolve(dev, staging, variable).unwrap();
            assert_eq!(
                name(&environment),
                expected,
                "{dev} {staging:?} {variable:?}"
            );
        }
    }

    #[test]
    fn tracel_env_reads_every_environment_as_the_cli_and_the_sdk_write_it() {
        for environment in [
            Env::Production,
            Env::Development,
            Env::Staging(0),
            Env::Staging(255),
        ] {
            for written in [name(&environment), sdk_value(&environment)] {
                let read = resolve(false, None, Some(&written)).unwrap();
                assert_eq!(name(&read), name(&environment), "{written}");
            }
        }
    }

    #[test]
    fn an_unknown_tracel_env_is_a_usage_error() {
        for value in [
            "bogus",
            "",
            "Staging()",
            "Staging(1",
            "staging-",
            "staging--1",
            "Staging(256)",
            "staging-256",
            "Production ",
        ] {
            let error = anyhow::Error::new(resolve(false, None, Some(value)).unwrap_err());
            assert_eq!(report(&error), (ErrorKind::Usage, 2, false), "{value:?}");
            assert!(
                error.to_string().contains(&format!("'{value}'")),
                "{value:?}"
            );
        }
    }

    #[test]
    fn each_environment_has_its_web_console() {
        for (environment, expected) in [
            (Env::Production, "https://console.tracel.ai/"),
            (Env::Staging(3), "https://s3-console.tracel.ai/"),
            (Env::Development, "http://localhost:9000/"),
        ] {
            assert_eq!(
                web_console(&environment).as_str(),
                expected,
                "{environment:?}"
            );
        }
    }
}
