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

    #[test]
    fn environment_values_match_sdk_and_lowercase_forms() {
        for value in [None, Some("Production"), Some("production")] {
            assert!(matches!(resolve(false, None, value), Ok(Env::Production)));
        }
        for value in ["Development", "development"] {
            assert!(matches!(
                resolve(false, None, Some(value)),
                Ok(Env::Development)
            ));
        }
        for version in [0, 1, 255] {
            for value in [format!("Staging({version})"), format!("staging-{version}")] {
                assert!(matches!(
                    resolve(false, None, Some(&value)),
                    Ok(Env::Staging(number)) if number == version
                ));
            }
        }
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
            let error = resolve(false, None, Some(value)).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Usage);
            assert!(error.to_string().contains("TRACEL_ENV"));
        }
    }

    #[test]
    fn environment_flags_override_variable() {
        for value in [None, Some("Production"), Some("Development"), Some("bogus")] {
            assert!(matches!(resolve(true, None, value), Ok(Env::Development)));
            assert!(matches!(
                resolve(false, Some(2), value),
                Ok(Env::Staging(2))
            ));
        }
    }
}
