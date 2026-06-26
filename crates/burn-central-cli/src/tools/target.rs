//! Target (OS + architecture) helpers for binary upload.

use std::collections::HashSet;
use std::process::{Command, Stdio};

use anyhow::Context;
use colored::Colorize;
use tracel_client::request::{Arch, Os};

/// Every (os, arch) target we offer to build for, in canonical display order.
/// The host is surfaced separately and pulled to the front by `prompt_targets`.
/// macOS x86_64 (Intel) is intentionally omitted — we don't support it.
pub const ALL_TARGETS: [(Os, Arch); 5] = [
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

/// Human-friendly name for an (os, arch) pair, e.g. "Linux x86_64".
fn pretty_name(os: Os, arch: Arch) -> &'static str {
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
        other => anyhow::bail!("Unsupported host operating system for packaging: `{other}`"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => Arch::X86_64,
        "aarch64" | "arm64" => Arch::Arm64,
        other => anyhow::bail!("Unsupported host architecture for packaging: `{other}`"),
    };
    if (os, arch) == (Os::Macos, Arch::X86_64) {
        anyhow::bail!("macOS on x86_64 (Intel) is not supported for packaging.");
    }
    Ok((os, arch))
}

/// Triples reported by `rustup target list --installed`.
///
/// Returns an empty set on any failure (rustup missing, non-zero exit, bad UTF-8),
/// which conservatively makes every *cross* target appear "not installed". The host
/// target never depends on this set — a host build needs no rustup target.
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

/// Install a target's prebuilt std with `rustup target add <triple>`, streaming
/// output to the user's terminal.
pub fn add_target(triple: &str) -> anyhow::Result<()> {
    let status = Command::new("rustup")
        .arg("target")
        .arg("add")
        .arg(triple)
        .status()
        .with_context(|| {
            format!("Failed to run `rustup target add {triple}` (is rustup installed?)")
        })?;
    if !status.success() {
        anyhow::bail!("`rustup target add {triple}` failed");
    }
    Ok(())
}

/// Prompt the user to select one or more targets to build for. The host is listed
/// first and labelled "(this machine)"; cross targets not installed via rustup are
/// dimmed and annotated with the `rustup target add` command to install them.
pub fn prompt_targets(
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
                    "builds natively; no extra toolchain needed".to_string(),
                )
            } else if installed.contains(triple) {
                ((os, arch), format!("{name}  ({triple})"), String::new())
            } else {
                // Not installed: dim the whole label (stays greyed even under the
                // cursor) and keep a visible textual marker so the distinction
                // survives in terminals without color, plus an actionable hint.
                (
                    (os, arch),
                    format!("{name}  ({triple})  - not installed")
                        .dimmed()
                        .to_string(),
                    format!("run: rustup target add {triple}"),
                )
            }
        })
        .collect();

    cliclack::multiselect(
        "Select the target(s) to build for (space to toggle, enter to confirm)",
    )
    .items(&items)
    .initial_values(vec![host])
    .required(true)
    .interact()
    .map_err(anyhow::Error::from)
}

pub fn install_missing_target(missing: Vec<&str>) -> anyhow::Result<()> {
    if !missing.is_empty() {
        let list = missing.join(", ");
        if cliclack::confirm(format!(
            "These targets are not installed: {list}. Run `rustup target add` for them now?"
        ))
        .initial_value(true)
        .interact()?
        {
            for triple in &missing {
                add_target(triple)?;
            }
        } else {
            let cmds = missing
                .iter()
                .map(|triple| format!("rustup target add {triple}"))
                .collect::<Vec<_>>()
                .join("\n  ");
            anyhow::bail!(
                "Cannot build without the selected targets installed. Install them with:\n  {cmds}"
            );
        }
    }
    Ok(())
}
