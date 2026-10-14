//! The linker of a Linux build for another architecture. `rustup target add` installs a
//! target's standard library but no linker, so the build gets one through cargo's
//! `--config target.<triple>.linker`, and the project's cargo config stays as it is.
//! A build for another OS needs a whole toolchain, which a build driver supplies.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use toml_edit::DocumentMut;
use tracel_client::console::project::request::{Arch, Os};

use crate::tools::target::target_triple;
use crate::ui::Terminal;

/// What linking `target` on `host` needs.
enum LinkerNeed {
    /// Nothing: the toolchain or the build driver links it.
    None,
    /// A `target.<triple>.linker` setting for the build, on Linux.
    ConfigEntry {
        linker: &'static str,
        install_hint: &'static str,
    },
}

/// Classify what linker setup `target` needs when building on `host`.
fn linker_need(host: (Os, Arch), target: (Os, Arch)) -> LinkerNeed {
    if host == target {
        return LinkerNeed::None;
    }

    let (host_os, _) = host;
    let (target_os, target_arch) = target;

    if host_os == target_os {
        // Same OS, different arch.
        return match target_os {
            Os::Linux => match target_arch {
                Arch::Arm64 => LinkerNeed::ConfigEntry {
                    linker: "aarch64-linux-gnu-gcc",
                    install_hint: "Install it with `apt install gcc-aarch64-linux-gnu`.",
                },
                Arch::X86_64 => LinkerNeed::ConfigEntry {
                    linker: "x86_64-linux-gnu-gcc",
                    install_hint: "Install it with `apt install gcc-x86-64-linux-gnu`.",
                },
            },
            // macOS (clang) and Windows (MSVC) toolchains build either arch natively.
            Os::Macos | Os::Windows => LinkerNeed::None,
        };
    }

    // For another OS, a build driver supplies the toolchain.
    LinkerNeed::None
}

/// Read the project's cargo config, `.cargo/config.toml` before the older
/// `.cargo/config`, as its path and contents.
fn read_config(root: &Path) -> Option<(PathBuf, String)> {
    for name in [".cargo/config.toml", ".cargo/config"] {
        let path = root.join(name);
        if let Ok(content) = std::fs::read_to_string(&path) {
            return Some((path, content));
        }
    }
    None
}

/// The cargo env var that overrides the linker for `triple`, e.g.
/// `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER`.
fn linker_env_var(triple: &str) -> String {
    format!(
        "CARGO_TARGET_{}_LINKER",
        triple.to_uppercase().replace(['-', '.'], "_")
    )
}

/// Whether `triple` already has a linker, from the project's cargo config or
/// `CARGO_TARGET_<TRIPLE>_LINKER`.
fn linker_configured(root: &Path, triple: &str) -> bool {
    if std::env::var_os(linker_env_var(triple)).is_some() {
        return true;
    }
    if let Some((_, content)) = read_config(root) {
        if let Ok(doc) = content.parse::<DocumentMut>() {
            return doc
                .get("target")
                .and_then(|t| t.get(triple))
                .and_then(|t| t.get("linker"))
                .is_some();
        }
    }
    false
}

/// Whether `bin` runs from PATH.
fn linker_on_path(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Choose the linker to give a Linux build for another architecture, unless its
/// toolchain needs none or the user set one.
pub fn resolve_linker(
    terminal: &Terminal,
    root: &Path,
    host: (Os, Arch),
    target: (Os, Arch),
) -> Option<&'static str> {
    let triple = target_triple(target.0, target.1);
    match linker_need(host, target) {
        LinkerNeed::None => None,
        LinkerNeed::ConfigEntry {
            linker: linker_bin,
            install_hint,
        } => {
            if linker_configured(root, triple) {
                return None;
            }
            if !linker_on_path(linker_bin) {
                terminal.warning(&format!("No linker {linker_bin} on PATH.\n{install_hint}"));
            }
            Some(linker_bin)
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    const LINUX_X86: (Os, Arch) = (Os::Linux, Arch::X86_64);
    const LINUX_ARM: (Os, Arch) = (Os::Linux, Arch::Arm64);
    const MAC_X86: (Os, Arch) = (Os::Macos, Arch::X86_64);
    const MAC_ARM: (Os, Arch) = (Os::Macos, Arch::Arm64);
    const WIN_X86: (Os, Arch) = (Os::Windows, Arch::X86_64);

    fn linker(need: LinkerNeed) -> Option<&'static str> {
        match need {
            LinkerNeed::None => None,
            LinkerNeed::ConfigEntry { linker, .. } => Some(linker),
        }
    }

    #[test]
    fn given_the_host_target_when_building_then_no_linker_is_needed() {
        let need = linker_need(LINUX_X86, LINUX_X86);

        assert_eq!(linker(need), None);
    }

    #[rstest]
    #[case::to_arm(LINUX_X86, LINUX_ARM, "aarch64-linux-gnu-gcc")]
    #[case::to_x86(LINUX_ARM, LINUX_X86, "x86_64-linux-gnu-gcc")]
    fn given_another_linux_architecture_when_building_then_its_cross_gcc_links(
        #[case] host: (Os, Arch),
        #[case] target: (Os, Arch),
        #[case] expected: &str,
    ) {
        let need = linker_need(host, target);

        assert_eq!(linker(need), Some(expected));
    }

    #[rstest]
    #[case::macos_architectures(MAC_X86, MAC_ARM)]
    #[case::linux_to_windows(LINUX_X86, WIN_X86)]
    #[case::linux_to_macos(LINUX_X86, MAC_ARM)]
    fn given_a_target_the_toolchain_or_build_driver_links_when_building_then_no_linker_is_needed(
        #[case] host: (Os, Arch),
        #[case] target: (Os, Arch),
    ) {
        let need = linker_need(host, target);

        assert_eq!(linker(need), None);
    }

    #[test]
    fn given_no_cargo_config_when_checking_the_linker_then_it_is_not_configured() {
        let root = tempfile::tempdir().unwrap();

        let configured = linker_configured(root.path(), "aarch64-unknown-linux-gnu");

        assert!(!configured);
    }
}
