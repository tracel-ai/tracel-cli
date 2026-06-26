//! Target (OS + architecture) helpers for binary upload.

use tracel_client::request::{Arch, Os};

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
    Ok((os, arch))
}

/// Prompt the user to select the target operating system.
pub fn prompt_os() -> anyhow::Result<Os> {
    cliclack::select("Select the target operating system")
        .items(&[
            (Os::Linux, "Linux", ""),
            (Os::Macos, "macOS", ""),
            (Os::Windows, "Windows", ""),
        ])
        .interact()
        .map_err(anyhow::Error::from)
}

/// Prompt the user to select the target architecture.
pub fn prompt_arch() -> anyhow::Result<Arch> {
    cliclack::select("Select the target architecture")
        .items(&[
            (Arch::X86_64, "x86_64", ""),
            (Arch::Arm64, "arm64 / aarch64", ""),
        ])
        .interact()
        .map_err(anyhow::Error::from)
}
