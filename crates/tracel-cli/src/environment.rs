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
    use rstest::rstest;

    use super::*;

    #[test]
    fn given_dev_when_resolving_the_environment_then_it_is_development_whatever_tracel_env_says() {
        let environment = resolve(true, None, Some("production")).unwrap();

        assert_eq!(name(&environment), "development");
    }

    #[test]
    fn given_staging_when_resolving_the_environment_then_it_is_that_staging_whatever_tracel_env_says()
     {
        let environment = resolve(false, Some(2), Some("development")).unwrap();

        assert_eq!(name(&environment), "staging-2");
    }

    #[test]
    fn given_no_flag_or_tracel_env_when_resolving_the_environment_then_it_is_production() {
        let environment = resolve(false, None, None).unwrap();

        assert_eq!(name(&environment), "production");
    }

    #[rstest]
    #[case::cli_development("development", "development")]
    #[case::sdk_development("Development", "development")]
    #[case::cli_staging("staging-2", "staging-2")]
    #[case::sdk_staging("Staging(2)", "staging-2")]
    #[case::sdk_production("Production", "production")]
    fn given_tracel_env_as_the_cli_or_the_sdk_writes_it_when_resolving_then_it_names_that_environment(
        #[case] variable: &str,
        #[case] expected: &str,
    ) {
        let environment = resolve(false, None, Some(variable)).unwrap();

        assert_eq!(name(&environment), expected);
    }

    #[rstest]
    #[case::unknown("bogus")]
    #[case::no_version("staging-")]
    #[case::version_too_large("Staging(256)")]
    #[case::trailing_space("Production ")]
    fn given_an_unknown_tracel_env_when_resolving_then_it_is_a_usage_error(#[case] variable: &str) {
        let error = resolve(false, None, Some(variable)).unwrap_err();

        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(
            error.to_string().contains(&format!("'{variable}'")),
            "{error}"
        );
    }
}
