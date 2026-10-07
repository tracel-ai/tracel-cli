use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::Context;

use crate::error::{CliError, ErrorKind};

pub fn try_locate_manifest() -> Option<std::path::PathBuf> {
    let output = command()
        .arg("locate-project")
        .arg("--workspace")
        .output()
        .expect("Failed to run cargo locate-project");
    if !output.status.success() {
        return None;
    }

    let output_str = String::from_utf8(output.stdout).expect("Failed to parse output");
    if output_str.trim().is_empty() {
        return None;
    }
    let parsed_output: serde_json::Value =
        serde_json::from_str(&output_str).expect("Failed to parse output");

    let manifest_path_str = parsed_output["root"]
        .as_str()
        .expect("Failed to parse output")
        .to_owned();

    let manifest_path = std::path::PathBuf::from(manifest_path_str);
    Some(manifest_path)
}

/// Retrieve the command to run cargo as define by the CARGO environment variable or default to "cargo"
pub fn command() -> std::process::Command {
    std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")))
}

/// Run `command`, a cargo build named `label` in messages, and return the executables it
/// built as `(target name, path)` pairs. Compiler diagnostics are printed on stderr.
pub fn build_executables(
    mut command: std::process::Command,
    label: &str,
) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let output = command
        .arg("--message-format=json-render-diagnostics")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("Failed to run `{label}`"))?;
    if !output.status.success() {
        anyhow::bail!("`{label}` failed");
    }
    executables(&output.stdout)
}

/// The executables in cargo's JSON build messages, as `(target name, path)` pairs.
fn executables(messages: &[u8]) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let mut executables = Vec::new();
    for line in messages.split(|&b| b == b'\n') {
        let Ok(message) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if message["reason"] != "compiler-artifact" {
            continue;
        }
        let Some(executable) = message["executable"].as_str() else {
            continue;
        };
        let name = message["target"]["name"].as_str().ok_or_else(|| {
            anyhow::anyhow!("Cargo did not return a name for binary {executable}")
        })?;
        executables.push((name.to_string(), PathBuf::from(executable)));
    }
    Ok(executables)
}

/// The error for a `--bin` that names none of the `valid` binaries.
pub fn invalid_bin(bin: &str, valid: &[&str]) -> CliError {
    CliError::new(
        ErrorKind::Usage,
        format!("Invalid --bin '{bin}'. Valid values: {}.", valid.join(", ")),
    )
    .with_hint("Pass --bin <name> using one of the valid values.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executables_are_the_artifacts_with_an_executable() {
        let messages = br#"{"reason":"compiler-artifact","target":{"name":"helper","kind":["lib"]},"executable":null}
not json
{"reason":"build-script-executed","package_id":"x"}
{"reason":"compiler-artifact","target":{"name":"trainer","kind":["bin"]},"executable":"/work/target/debug/trainer"}
{"reason":"build-finished","success":true}
"#;
        assert_eq!(
            executables(messages).unwrap(),
            [(
                "trainer".to_string(),
                PathBuf::from("/work/target/debug/trainer")
            )]
        );
        assert!(executables(b"").unwrap().is_empty());
    }

    #[test]
    fn an_invalid_bin_lists_the_valid_ones() {
        let error = invalid_bin("evaluate", &["train", "serve"]);
        assert_eq!(error.kind, ErrorKind::Usage);
        assert_eq!(
            error.to_string(),
            "Invalid --bin 'evaluate'. Valid values: train, serve."
        );
    }
}
