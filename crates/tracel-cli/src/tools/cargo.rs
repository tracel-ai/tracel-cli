use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::Context;

/// Find the manifest of the Cargo workspace around the current directory.
pub fn try_locate_manifest() -> Option<PathBuf> {
    let output = command()
        .arg("locate-project")
        .arg("--workspace")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let location: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    location["root"].as_str().map(PathBuf::from)
}

/// Start a cargo command, with the cargo `CARGO` names when it is set.
pub fn command() -> std::process::Command {
    std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")))
}

/// A finished cargo build whose own output was kept back.
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

    /// Print cargo's output on stderr, as cargo would have.
    pub fn show_output(&self) {
        let _ = io::stderr().write_all(&self.output);
    }

    /// List the executables the build made as `(target name, path)` pairs, or fail if
    /// the build did.
    pub fn executables(self) -> anyhow::Result<Vec<(String, PathBuf)>> {
        if !self.succeeded {
            anyhow::bail!("`{}` failed.", self.label);
        }
        executables(&self.messages)
    }
}

/// Run `command`, a cargo build named `label` in errors, keeping cargo's output back.
/// Each status line, such as `Compiling trainer v0.1.0`, goes to `status` as it comes.
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
        .with_context(|| format!("Can't run `{label}`"))?;
    let mut stdout = child.stdout.take().context("Can't read cargo's output")?;
    let messages = std::thread::spawn(move || {
        let mut messages = Vec::new();
        stdout.read_to_end(&mut messages).map(|_| messages)
    });

    let mut output = Vec::new();
    let mut stderr = BufReader::new(child.stderr.take().context("Can't read cargo's output")?);
    let mut line = Vec::new();
    while stderr.read_until(b'\n', &mut line)? > 0 {
        if let Some(line) = status_line(&String::from_utf8_lossy(&line)) {
            status(&line);
        }
        output.append(&mut line);
    }
    let messages = messages
        .join()
        .map_err(|_| anyhow::anyhow!("Can't read the output of `{label}`."))??;
    Ok(QuietBuild {
        label: label.to_string(),
        succeeded: child.wait()?.success(),
        output,
        messages,
    })
}

/// Find what cargo says it is doing in `line`, such as `Compiling trainer v0.1.0`,
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

/// List the executables in cargo's JSON build messages.
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
        let name = message["target"]["name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Cargo sent no name for the binary {executable}."))?;
        executables.push((name.to_string(), PathBuf::from(executable)));
    }
    Ok(executables)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn given_cargo_build_messages_when_listing_executables_then_only_artifacts_with_one_are_listed()
    {
        let messages = br#"{"reason":"compiler-artifact","target":{"name":"helper","kind":["lib"]},"executable":null}
{"reason":"build-script-executed","package_id":"trainer"}
{"reason":"compiler-artifact","target":{"name":"trainer","kind":["bin"]},"executable":"/work/target/release/trainer"}
{"reason":"build-finished","success":true}
"#;

        let executables = executables(messages).unwrap();

        assert_eq!(
            executables,
            [(
                "trainer".to_string(),
                PathBuf::from("/work/target/release/trainer")
            )]
        );
    }

    #[rstest]
    #[case::compiling(
        "\u{1b}[1m\u{1b}[32m   Compiling\u{1b}[0m trainer v0.1.0 (/work/trainer)\n",
        "Compiling trainer v0.1.0"
    )]
    #[case::downloaded("  Downloaded serde v1.0.228\n", "Downloaded serde v1.0.228")]
    fn given_a_cargo_status_when_reading_it_then_it_says_what_cargo_does_without_paths(
        #[case] line: &str,
        #[case] expected: &str,
    ) {
        let status = status_line(line);

        assert_eq!(status.as_deref(), Some(expected));
    }

    #[rstest]
    #[case::finished("    Finished `release` profile [optimized] target(s) in 0.20s\n")]
    #[case::warning("warning: unused variable: `x`\n")]
    #[case::source_location("  --> src/main.rs:3:9\n")]
    fn given_another_cargo_line_when_reading_it_then_it_is_no_status(#[case] line: &str) {
        let status = status_line(line);

        assert_eq!(status, None);
    }
}
