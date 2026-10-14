use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::Context;
use cargo_metadata::{Message, Metadata};

use crate::error::{CliError, ErrorKind};
use crate::tools::build_driver::BuildDriver;
use crate::ui::Terminal;

/// Find the manifest of the Cargo workspace around the current directory.
pub fn try_locate_manifest() -> Option<PathBuf> {
    let output = command()
        .args(["locate-project", "--workspace", "--message-format", "plain"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    Some(PathBuf::from(path.trim_end_matches(['\r', '\n'])))
}

/// Start a cargo command, with the cargo `CARGO` names when it is set.
pub fn command() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")))
}

/// A binary target of a workspace package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binary {
    pub package: String,
    pub name: String,
}

/// List the binary targets of the workspace's packages.
pub fn binaries(metadata: &Metadata) -> Vec<Binary> {
    metadata
        .workspace_packages()
        .into_iter()
        .flat_map(|package| {
            package
                .targets
                .iter()
                .filter(|target| target.is_bin())
                .map(|target| Binary {
                    package: package.name.to_string(),
                    name: target.name.clone(),
                })
        })
        .collect()
}

/// Find the binary `--bin <name>` names among `binaries`, with its package.
pub fn find_binary(binaries: &[Binary], name: &str) -> Result<Binary, CliError> {
    if let Some(binary) = binaries.iter().find(|binary| binary.name == name) {
        return Ok(binary.clone());
    }
    let names: Vec<&str> = binaries.iter().map(|binary| binary.name.as_str()).collect();
    Err(CliError::new(
        ErrorKind::Usage,
        format!("No binary '{name}'. Binaries: {}.", names.join(", ")),
    ))
}

/// Make the release build of `bin`, or of every binary, with `driver`, for `target` when
/// cross-building, and the command as messages show it.
pub fn release_build(
    driver: BuildDriver,
    target: Option<&str>,
    linker: Option<&str>,
    bin: Option<&Binary>,
) -> (Command, String) {
    let mut command = command();
    command.args(driver.subcommand_args()).arg("--release");
    let mut label = format!("{} --release", driver.label());
    if let Some(bin) = bin {
        command.args(["--package", &bin.package, "--bin", &bin.name]);
        label.push_str(&format!(" --package {} --bin {}", bin.package, bin.name));
    }
    if let Some(triple) = target {
        command.args(["--target", triple]);
        label.push_str(&format!(" --target {triple}"));
        if let Some(linker) = linker {
            command
                .arg("--config")
                .arg(format!("target.{triple}.linker=\"{linker}\""));
        }
    }
    (command, label)
}

/// Run `command`, a cargo build named `label` in errors, under a spinner saying `building`
/// and what cargo compiles, and list the executables it built as `(target name, path)`
/// pairs. The spinner clears once the build is done; cargo's own output shows only when
/// the build fails.
pub fn build(
    terminal: &Terminal,
    command: Command,
    label: &str,
    building: &str,
) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let spinner = terminal.spinner(building);
    let built = build_quietly(command, label, |status| {
        spinner.set_message(format!("{building}: {status}"));
    });
    spinner.clear();
    let built = built?;
    if !built.succeeded {
        let _ = io::stderr().write_all(&built.output);
        anyhow::bail!("`{label}` failed.");
    }
    Ok(built.executables)
}

/// A finished cargo build whose own output was kept back.
struct QuietBuild {
    succeeded: bool,
    output: Vec<u8>,
    executables: Vec<(String, PathBuf)>,
}

/// Run `command`, a cargo build named `label` in errors, keeping cargo's output back.
/// Each status line, such as `Compiling trainer v0.1.0`, goes to `status` as it comes.
fn build_quietly(
    mut command: Command,
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
    let stdout = child.stdout.take().context("Can't read cargo's output")?;
    let executables = std::thread::spawn(move || executables(BufReader::new(stdout)));

    let mut output = Vec::new();
    let mut stderr = BufReader::new(child.stderr.take().context("Can't read cargo's output")?);
    let mut line = Vec::new();
    while stderr.read_until(b'\n', &mut line)? > 0 {
        if let Some(line) = status_line(&String::from_utf8_lossy(&line)) {
            status(&line);
        }
        output.append(&mut line);
    }
    let executables = executables
        .join()
        .map_err(|_| anyhow::anyhow!("Can't read the output of `{label}`."))??;
    Ok(QuietBuild {
        succeeded: child.wait()?.success(),
        output,
        executables,
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

/// List the executables in cargo's JSON build messages as `(target name, path)` pairs.
fn executables(messages: impl BufRead) -> io::Result<Vec<(String, PathBuf)>> {
    let mut executables = Vec::new();
    for message in Message::parse_stream(messages) {
        let Message::CompilerArtifact(artifact) = message? else {
            continue;
        };
        if let Some(path) = artifact.executable {
            executables.push((artifact.target.name, path.into_std_path_buf()));
        }
    }
    Ok(executables)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

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
