use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::Context;

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

/// A cargo build that ran with cargo's own output kept back, as [`build_quietly`] runs it.
pub struct QuietBuild {
    label: String,
    succeeded: bool,
    output: Vec<u8>,
    messages: Vec<u8>,
}

impl QuietBuild {
    pub fn succeeded(&self) -> bool {
        self.succeeded
    }

    /// Prints cargo's output on stderr, as it would have printed it.
    pub fn show_output(&self) {
        let _ = io::stderr().write_all(&self.output);
    }

    /// The executables the build made, as `(target name, path)` pairs, or why it failed.
    pub fn executables(self) -> anyhow::Result<Vec<(String, PathBuf)>> {
        if !self.succeeded {
            anyhow::bail!("`{}` failed", self.label);
        }
        executables(&self.messages)
    }
}

/// Run `command`, a cargo build named `label` in messages, keeping cargo's own output back:
/// each of its status lines, such as `Compiling trainer v0.1.0`, goes to `status` as the
/// build runs.
pub fn build_quietly(
    mut command: std::process::Command,
    label: &str,
    mut status: impl FnMut(&str),
) -> anyhow::Result<QuietBuild> {
    if console::colors_enabled_stderr() {
        command.arg("--color=always");
    }
    let mut child = command
        .arg("--message-format=json-render-diagnostics")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to run `{label}`"))?;
    let mut stdout = child.stdout.take().context("cargo's stdout is not piped")?;
    let messages = std::thread::spawn(move || {
        let mut messages = Vec::new();
        stdout.read_to_end(&mut messages).map(|_| messages)
    });

    let mut output = Vec::new();
    let mut stderr = BufReader::new(child.stderr.take().context("cargo's stderr is not piped")?);
    let mut line = Vec::new();
    while stderr.read_until(b'\n', &mut line)? > 0 {
        if let Some(line) = status_line(&String::from_utf8_lossy(&line)) {
            status(&line);
        }
        output.append(&mut line);
    }
    let messages = messages
        .join()
        .map_err(|_| anyhow::anyhow!("Failed to read the output of `{label}`"))??;
    Ok(QuietBuild {
        label: label.to_string(),
        succeeded: child.wait()?.success(),
        output,
        messages,
    })
}

/// A line of cargo's output that says what it is doing, such as `Compiling trainer v0.1.0`,
/// without the path it may end with.
fn status_line(line: &str) -> Option<String> {
    const STATUSES: [&str; 6] = [
        "Blocking",
        "Compiling",
        "Downloading",
        "Downloaded",
        "Locking",
        "Updating",
    ];
    let line = console::strip_ansi_codes(line);
    let line = line.trim();
    let (word, _) = line.split_once(' ')?;
    if !STATUSES.contains(&word) {
        return None;
    }
    let line = match line.split_once(" (") {
        Some((line, _)) => line,
        None => line,
    };
    Some(line.to_string())
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
    fn status_lines_say_what_cargo_does_without_paths() {
        assert_eq!(
            status_line(
                "\u{1b}[1m\u{1b}[32m   Compiling\u{1b}[0m trainer v0.1.0 (/work/trainer)\n"
            )
            .as_deref(),
            Some("Compiling trainer v0.1.0")
        );
        assert_eq!(
            status_line("  Downloaded serde v1.0.228\n").as_deref(),
            Some("Downloaded serde v1.0.228")
        );
        for line in [
            "    Finished `release` profile [optimized] target(s) in 0.20s\n",
            "warning: unused variable: `x`\n",
            "  --> src/main.rs:3:9\n",
            "\n",
        ] {
            assert_eq!(status_line(line), None, "{line:?}");
        }
    }
}
