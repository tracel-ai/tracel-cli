//! Cross-linker helpers for binary upload.
//!
//! `rustup target add` installs only the prebuilt std, not a linker. For the
//! predictable Linux same-OS cross-arch case we resolve a cross-linker and inject it
//! into the single build command via cargo's `--config target.<triple>.linker` flag,
//! leaving the project's `.cargo/config.toml` untouched.
//! Cross-OS targets (Windows/macOS from Linux, etc.) need a whole toolchain, not just
//! a linker entry — those are handled by a build driver (see `tools::build_driver`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use toml_edit::DocumentMut;
use tracel_client::request::{Arch, Os};

use crate::tools::target::target_triple;
use crate::tools::terminal::Terminal;

/// What (if anything) is needed to link `target` while running on `host`.
pub enum LinkerNeed {
    /// Native, a same-OS cross-arch the toolchain handles itself (macOS/Windows), or a
    /// cross-OS target (whose toolchain is supplied by the build driver, not a linker entry).
    None,
    /// Linux same-OS cross-arch: a `target.<triple>.linker` setting we inject per-build.
    ConfigEntry {
        linker: &'static str,
        install_hint: &'static str,
    },
}

/// Classify what linker setup `target` needs when building on `host`.
pub fn linker_need(host: (Os, Arch), target: (Os, Arch)) -> LinkerNeed {
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
                    install_hint: "install the cross-linker, e.g. `apt install gcc-aarch64-linux-gnu` (Debian/Ubuntu) or `pacman -S aarch64-linux-gnu-gcc` (Arch)",
                },
                Arch::X86_64 => LinkerNeed::ConfigEntry {
                    linker: "x86_64-linux-gnu-gcc",
                    install_hint: "install the cross-linker, e.g. `apt install gcc-x86-64-linux-gnu` (Debian/Ubuntu)",
                },
            },
            // macOS (clang) and Windows (MSVC) toolchains build either arch natively.
            Os::Macos | Os::Windows => LinkerNeed::None,
        };
    }

    // Different OS: a linker entry isn't enough — a build driver (cargo-xwin /
    // cargo-zigbuild) supplies the toolchain instead. See `tools::build_driver`.
    LinkerNeed::None
}

/// Read the project's cargo config, preferring `.cargo/config.toml` over the legacy
/// `.cargo/config`. Returns the path it was read from and its contents.
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

/// Whether a linker for `triple` is already configured — either via a
/// `target.<triple>.linker` entry in the project's cargo config, or the
/// `CARGO_TARGET_<triple>_LINKER` environment variable.
pub fn linker_configured(root: &Path, triple: &str) -> bool {
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

/// Whether `bin` can be executed (used to decide whether to also print an install hint).
pub fn linker_on_path(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Resolve the cross-linker to inject for a same-OS cross-arch build that needs one
/// (Linux), or `None` when the toolchain handles it natively or the user has already
/// configured a linker for `target`.
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
            // The user already set a linker for this target (cargo config or env var);
            // respect their choice and don't override it.
            if linker_configured(root, triple) {
                return None;
            }
            if !linker_on_path(linker_bin) {
                terminal.print_warning(&format!(
                    "Linker `{linker_bin}` was not found on PATH — {install_hint}."
                ));
            }
            Some(linker_bin)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique, empty scratch directory for a test, removed on drop.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("burn_linker_test_{}_{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempRoot(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const LINUX_X86: (Os, Arch) = (Os::Linux, Arch::X86_64);
    const LINUX_ARM: (Os, Arch) = (Os::Linux, Arch::Arm64);
    const MAC_X86: (Os, Arch) = (Os::Macos, Arch::X86_64);
    const MAC_ARM: (Os, Arch) = (Os::Macos, Arch::Arm64);
    const WIN_X86: (Os, Arch) = (Os::Windows, Arch::X86_64);

    #[test]
    fn linker_need_same_target_is_none() {
        assert!(matches!(
            linker_need(LINUX_X86, LINUX_X86),
            LinkerNeed::None
        ));
    }

    #[test]
    fn linker_need_linux_cross_arch_is_config_entry() {
        match linker_need(LINUX_X86, LINUX_ARM) {
            LinkerNeed::ConfigEntry { linker, .. } => assert_eq!(linker, "aarch64-linux-gnu-gcc"),
            _ => panic!("expected ConfigEntry"),
        }
        match linker_need(LINUX_ARM, LINUX_X86) {
            LinkerNeed::ConfigEntry { linker, .. } => assert_eq!(linker, "x86_64-linux-gnu-gcc"),
            _ => panic!("expected ConfigEntry"),
        }
    }

    #[test]
    fn linker_need_macos_cross_arch_is_none() {
        assert!(matches!(linker_need(MAC_X86, MAC_ARM), LinkerNeed::None));
    }

    #[test]
    fn linker_need_different_os_is_none() {
        // Cross-OS linker setup is handled by the build driver, not a linker entry.
        assert!(matches!(linker_need(LINUX_X86, WIN_X86), LinkerNeed::None));
        assert!(matches!(linker_need(LINUX_X86, MAC_ARM), LinkerNeed::None));
    }

    #[test]
    fn linker_configured_false_when_absent() {
        let root = TempRoot::new("absent");
        assert!(!linker_configured(root.path(), "aarch64-unknown-linux-gnu"));
    }
}
