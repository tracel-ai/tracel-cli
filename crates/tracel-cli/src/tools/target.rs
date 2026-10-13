//! Target (OS + architecture) helpers for binary upload.

use std::collections::HashSet;
use std::process::{Command, Stdio};

use anyhow::Context;
use tracel_client::console::project::request::{Arch, Os};

use crate::error::{CliError, ErrorKind};
use crate::ui::{Declined, Terminal, Unanswered};

/// The targets packaging builds for, in display order. macOS on Intel isn't one.
const ALL_TARGETS: [(Os, Arch); 5] = [
    (Os::Linux, Arch::X86_64),
    (Os::Linux, Arch::Arm64),
    (Os::Macos, Arch::Arm64),
    (Os::Windows, Arch::X86_64),
    (Os::Windows, Arch::Arm64),
];

/// Canonical Rust target triple for an (os, arch) pair. Must match the server's
/// `TargetTriplet::Display`, because the upload-URL map is keyed by this string.
pub fn target_triple(os: Os, arch: Arch) -> &'static str {
    match (os, arch) {
        (Os::Windows, Arch::X86_64) => "x86_64-pc-windows-msvc",
        (Os::Windows, Arch::Arm64) => "aarch64-pc-windows-msvc",
        (Os::Linux, Arch::X86_64) => "x86_64-unknown-linux-gnu",
        (Os::Linux, Arch::Arm64) => "aarch64-unknown-linux-gnu",
        (Os::Macos, Arch::X86_64) => "x86_64-apple-darwin",
        (Os::Macos, Arch::Arm64) => "aarch64-apple-darwin",
    }
}

/// Read a `--target` triple.
pub fn parse_target(triple: &str) -> Result<(Os, Arch), CliError> {
    ALL_TARGETS
        .iter()
        .copied()
        .find(|&(os, arch)| target_triple(os, arch) == triple)
        .ok_or_else(|| {
            let valid = ALL_TARGETS
                .iter()
                .map(|&(os, arch)| target_triple(os, arch))
                .collect::<Vec<_>>()
                .join(", ");
            CliError::new(
                ErrorKind::Usage,
                format!("No target '{triple}'. Targets: {valid}."),
            )
        })
}

fn resolve_targets(host: (Os, Arch), requested: &[String]) -> Result<Vec<(Os, Arch)>, CliError> {
    if requested.is_empty() {
        return Ok(vec![host]);
    }
    let mut targets = Vec::new();
    for triple in requested {
        let target = parse_target(triple)?;
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    Ok(targets)
}

/// Choose the targets: those `--target` names, else the host when the user can't answer,
/// else the user's choice.
pub fn select_targets(
    terminal: &Terminal,
    requested: &[String],
    host: (Os, Arch),
    installed: &HashSet<String>,
) -> anyhow::Result<Vec<(Os, Arch)>> {
    if !requested.is_empty() || !terminal.is_interactive() {
        return resolve_targets(host, requested).map_err(anyhow::Error::from);
    }
    prompt_targets(terminal, host, installed)
}

/// Name an (os, arch) pair for people, such as "Linux x86_64".
pub fn pretty_name(os: Os, arch: Arch) -> &'static str {
    match (os, arch) {
        (Os::Linux, Arch::X86_64) => "Linux x86_64",
        (Os::Linux, Arch::Arm64) => "Linux arm64",
        (Os::Macos, Arch::X86_64) => "macOS x86_64",
        (Os::Macos, Arch::Arm64) => "macOS arm64",
        (Os::Windows, Arch::X86_64) => "Windows x86_64",
        (Os::Windows, Arch::Arm64) => "Windows arm64",
    }
}

/// Detect the host OS/arch
pub fn host_target() -> anyhow::Result<(Os, Arch)> {
    let os = match std::env::consts::OS {
        "windows" => Os::Windows,
        "linux" => Os::Linux,
        "macos" => Os::Macos,
        other => anyhow::bail!("Packaging doesn't support this operating system ({other})."),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => Arch::X86_64,
        "aarch64" | "arm64" => Arch::Arm64,
        other => anyhow::bail!("Packaging doesn't support this architecture ({other})."),
    };
    if (os, arch) == (Os::Macos, Arch::X86_64) {
        anyhow::bail!("Packaging doesn't support macOS on Intel.");
    }
    Ok((os, arch))
}

/// List the targets `rustup target list --installed` reports, none when rustup fails,
/// so that every cross target then counts as missing.
pub fn installed_targets() -> HashSet<String> {
    let output = Command::new("rustup")
        .arg("target")
        .arg("list")
        .arg("--installed")
        .stderr(Stdio::null())
        .output();

    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect(),
        _ => HashSet::new(),
    }
}

/// Install a target's standard library with `rustup target add`, showing its output
/// on stderr.
fn add_target(triple: &str) -> anyhow::Result<()> {
    let status = Command::new("rustup")
        .arg("target")
        .arg("add")
        .arg(triple)
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .status()
        .with_context(|| format!("Can't run `rustup target add {triple}`"))?;
    if !status.success() {
        anyhow::bail!("`rustup target add {triple}` failed.");
    }
    Ok(())
}

/// Ask which targets to build, the host first. Targets rustup hasn't installed are
/// dimmed.
fn prompt_targets(
    terminal: &Terminal,
    host: (Os, Arch),
    installed: &HashSet<String>,
) -> anyhow::Result<Vec<(Os, Arch)>> {
    // Host first, then the remaining targets in canonical order.
    let mut ordered: Vec<(Os, Arch)> = vec![host];
    ordered.extend(ALL_TARGETS.iter().copied().filter(|t| *t != host));

    let items: Vec<((Os, Arch), String, String)> = ordered
        .iter()
        .map(|&(os, arch)| {
            let triple = target_triple(os, arch);
            let name = pretty_name(os, arch);

            if (os, arch) == host {
                (
                    (os, arch),
                    format!("{name}  ({triple})  (this machine)"),
                    "builds natively".to_string(),
                )
            } else if installed.contains(triple) {
                ((os, arch), format!("{name}  ({triple})"), String::new())
            } else {
                // Dim the whole label, which stays dim under the cursor, and keep a
                // marker that survives without colors.
                (
                    (os, arch),
                    console::style(format!("{name}  ({triple})  not installed"))
                        .dim()
                        .to_string(),
                    "installed with rustup if chosen".to_string(),
                )
            }
        })
        .collect();

    terminal.multiselect(
        "Targets",
        &items,
        vec![host],
        Unanswered::new("Packaging needs targets.", "Pass --target <triple>."),
    )
}

/// Install the `missing` targets with rustup when `install_targets` says so or the user
/// agrees.
pub fn install_missing_target(
    terminal: &Terminal,
    missing: Vec<&str>,
    install_targets: bool,
) -> anyhow::Result<()> {
    if !missing.is_empty() {
        let list = missing.join(", ");
        if install_targets
            || terminal.confirm(
                &format!("Install {list} with rustup?"),
                true,
                Unanswered::new(
                    format!("Installing {list} needs confirmation."),
                    "Pass --install-targets to install them without asking.",
                ),
            )?
        {
            for triple in &missing {
                add_target(triple)?;
            }
        } else {
            return Err(Declined::new("Nothing was packaged.").into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_explicit_targets_defaults_to_host() {
        for host in ALL_TARGETS {
            assert_eq!(resolve_targets(host, &[]).unwrap(), vec![host]);
        }
    }

    #[test]
    fn explicit_targets_are_validated_and_deduplicated_in_order() {
        let host = (Os::Linux, Arch::X86_64);
        let requested = [
            "aarch64-unknown-linux-gnu".to_string(),
            "x86_64-pc-windows-msvc".to_string(),
            "aarch64-unknown-linux-gnu".to_string(),
        ];
        assert_eq!(
            resolve_targets(host, &requested).unwrap(),
            vec![(Os::Linux, Arch::Arm64), (Os::Windows, Arch::X86_64),]
        );
        let error = parse_target("unknown-triple").unwrap_err();
        assert_eq!(error.kind, ErrorKind::Usage);
        for (os, arch) in ALL_TARGETS {
            assert!(error.to_string().contains(target_triple(os, arch)));
        }
    }
}
