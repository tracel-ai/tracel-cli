//! Build-driver selection for cross-compilation.
//!
//! `tracel package` runs `cargo build`, but a cross-**OS** target needs a different
//! driver that supplies the target's toolchain: `cargo xwin build` for Windows-MSVC,
//! `cargo zigbuild` for Linux/macOS. Both proxy cargo (and forward
//! `--message-format=json`), so the rest of the build pipeline is unchanged.

use std::path::Path;
use std::process::{Command, Stdio};

use tracel_client::console::project::request::{Arch, Os};

use crate::tools::linker;
use crate::tools::target::target_triple;
use crate::ui::Terminal;

/// How to drive the build of a given target.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BuildDriver {
    /// `cargo build`, for the host and for another architecture of its OS.
    Cargo,
    /// `cargo zigbuild`, for Linux and macOS from another OS, with Zig as the linker.
    Zigbuild,
    /// `cargo xwin build`, for Windows MSVC from another OS.
    Xwin,
}

impl BuildDriver {
    /// The command the driver runs, as messages show it.
    pub fn label(self) -> &'static str {
        match self {
            BuildDriver::Cargo => "cargo build",
            BuildDriver::Zigbuild => "cargo zigbuild",
            BuildDriver::Xwin => "cargo xwin build",
        }
    }

    /// The arguments after `cargo` that start the build.
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

/// Whether `bin --version` runs.
fn subcommand_installed(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Pick the driver that builds `target` on `host`: cargo within one OS, and cargo-xwin
/// or cargo-zigbuild across, else cargo, whose build then fails to link.
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

/// The hint to install the cross-build tool `target` needs.
pub fn install_hint(target: (Os, Arch)) -> &'static str {
    match target.0 {
        Os::Windows => "Install it with `cargo install --locked cargo-xwin`, and LLVM.",
        Os::Linux | Os::Macos => {
            "Install it with `cargo install --locked cargo-zigbuild`, and Zig."
        }
    }
}

/// Prepare to build `target` with `driver`, saying what the build relies on, and return
/// the linker a cross build within one OS needs.
pub fn cross_preflight(
    terminal: &Terminal,
    root: &Path,
    host: (Os, Arch),
    target: (Os, Arch),
    driver: BuildDriver,
) -> anyhow::Result<Option<&'static str>> {
    let triple = target_triple(target.0, target.1);
    match driver {
        BuildDriver::Cargo if host.0 == target.0 => {
            // Same-OS cross-arch: a `target.<triple>.linker` setting (Linux) is enough.
            let linker = linker::resolve_linker(terminal, root, host, target);
            terminal.warning(&format!(
                "Building {triple} with plain cargo, which may fail to link."
            ));
            Ok(linker)
        }
        BuildDriver::Cargo => {
            // Across operating systems without a driver, plain cargo won't link.
            terminal.warning(&format!(
                "No cross-build tool for {triple}, so the build will likely fail.\n{}",
                install_hint(target)
            ));
            Ok(None)
        }
        BuildDriver::Zigbuild => {
            terminal.info(&format!(
                "Building {triple} with cargo zigbuild, which needs Zig."
            ));
            Ok(None)
        }
        BuildDriver::Xwin => {
            terminal.info(&format!(
                "Building {triple} with cargo xwin, which needs LLVM."
            ));
            Ok(None)
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
        // Zig can't target MSVC, so without xwin it falls back to cargo, not zigbuild.
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
