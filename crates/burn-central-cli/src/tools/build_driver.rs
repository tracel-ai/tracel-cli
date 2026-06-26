//! Build-driver selection for cross-compilation.
//!
//! `burn package` runs `cargo build`, but a cross-**OS** target needs a different
//! driver that supplies the target's toolchain: `cargo xwin build` for Windows-MSVC,
//! `cargo zigbuild` for Linux/macOS. Both proxy cargo (and forward
//! `--message-format=json`), so the rest of the build pipeline is unchanged.

use std::process::{Command, Stdio};

use tracel_client::request::{Arch, Os};

/// How to drive the build of a given target.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BuildDriver {
    /// Plain `cargo build` — host builds and same-OS cross-arch builds.
    Cargo,
    /// `cargo zigbuild` — cross-OS Linux/macOS via Zig as the C compiler/linker.
    Zigbuild,
    /// `cargo xwin build` — cross-OS Windows-MSVC.
    Xwin,
}

impl BuildDriver {
    /// Human-readable command name, used in progress and error messages.
    pub fn label(self) -> &'static str {
        match self {
            BuildDriver::Cargo => "cargo build",
            BuildDriver::Zigbuild => "cargo zigbuild",
            BuildDriver::Xwin => "cargo xwin build",
        }
    }

    /// The subcommand arguments appended to `cargo` (before `--release`).
    pub fn subcommand_args(self) -> &'static [&'static str] {
        match self {
            BuildDriver::Cargo => &["build"],
            BuildDriver::Zigbuild => &["zigbuild"],
            BuildDriver::Xwin => &["xwin", "build"],
        }
    }
}

/// Which cross-build drivers are installed on this machine.
pub struct AvailableDrivers {
    pub zigbuild: bool,
    pub xwin: bool,
}

/// Detect installed cross-build drivers by probing their cargo-subcommand binaries.
pub fn detect() -> AvailableDrivers {
    AvailableDrivers {
        zigbuild: subcommand_installed("cargo-zigbuild"),
        xwin: subcommand_installed("cargo-xwin"),
    }
}

/// Whether `bin --version` runs successfully (the binary exists and is executable).
fn subcommand_installed(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Pick the build driver for `target` when building on `host`.
///
/// Same-OS builds (host, or cross-arch within the same OS) use plain `cargo build`.
/// Cross-OS builds need a specialized driver: Windows-MSVC uses `cargo-xwin` only
/// (Zig cannot target the MSVC ABI), Linux/macOS use `cargo-zigbuild`. When the
/// needed driver is absent we fall back to `Cargo`, which will surface a clear error.
pub fn choose(host: (Os, Arch), target: (Os, Arch), avail: &AvailableDrivers) -> BuildDriver {
    if host.0 == target.0 {
        return BuildDriver::Cargo;
    }
    match target.0 {
        Os::Windows => {
            if avail.xwin {
                BuildDriver::Xwin
            } else {
                BuildDriver::Cargo
            }
        }
        Os::Linux | Os::Macos => {
            if avail.zigbuild {
                BuildDriver::Zigbuild
            } else {
                BuildDriver::Cargo
            }
        }
    }
}

/// Actionable hint when no suitable cross-OS driver is installed and we fall back to
/// plain `cargo build` (which will almost certainly fail at link).
pub fn install_hint(target: (Os, Arch)) -> &'static str {
    match target.0 {
        Os::Windows => {
            "install cargo-xwin (`cargo install --locked cargo-xwin`); it also needs LLVM/lld"
        }
        Os::Linux | Os::Macos => {
            "install cargo-zigbuild (`cargo install --locked cargo-zigbuild`) and Zig; macOS also needs the Apple SDK"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINUX_X86: (Os, Arch) = (Os::Linux, Arch::X86_64);
    const LINUX_ARM: (Os, Arch) = (Os::Linux, Arch::Arm64);
    const MAC_ARM: (Os, Arch) = (Os::Macos, Arch::Arm64);
    const WIN_X86: (Os, Arch) = (Os::Windows, Arch::X86_64);

    fn both() -> AvailableDrivers {
        AvailableDrivers {
            zigbuild: true,
            xwin: true,
        }
    }
    fn none() -> AvailableDrivers {
        AvailableDrivers {
            zigbuild: false,
            xwin: false,
        }
    }

    #[test]
    fn same_os_always_uses_cargo() {
        // Even with every driver installed, same-OS cross-arch stays on cargo build.
        assert_eq!(choose(LINUX_X86, LINUX_ARM, &both()), BuildDriver::Cargo);
        assert_eq!(choose(LINUX_X86, LINUX_X86, &both()), BuildDriver::Cargo);
    }

    #[test]
    fn cross_os_windows_uses_xwin_only() {
        assert_eq!(choose(LINUX_X86, WIN_X86, &both()), BuildDriver::Xwin);
        // Zig can't target MSVC, so without xwin we fall back to cargo (not zigbuild).
        let only_zig = AvailableDrivers {
            zigbuild: true,
            xwin: false,
        };
        assert_eq!(choose(LINUX_X86, WIN_X86, &only_zig), BuildDriver::Cargo);
    }

    #[test]
    fn cross_os_unix_uses_zigbuild() {
        assert_eq!(choose(LINUX_X86, MAC_ARM, &both()), BuildDriver::Zigbuild);
        assert_eq!(choose(LINUX_X86, MAC_ARM, &none()), BuildDriver::Cargo);
    }
}
