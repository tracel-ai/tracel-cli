//! Cross-linker helpers for binary upload.
//!
//! `rustup target add` installs only the prebuilt std, not a linker. For the
//! predictable Linux same-OS cross-arch case we can write a
//! `[target.<triple>] linker = "..."` entry into the project's `.cargo/config.toml`.
//! Cross-OS targets (Windows/macOS from Linux, etc.) need a whole toolchain, not just
//! a linker entry — those are handled by a build driver (see `tools::build_driver`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context;
use toml_edit::{DocumentMut, Item, Table, value};
use tracel_client::request::{Arch, Os};

/// What (if anything) is needed to link `target` while running on `host`.
pub enum LinkerNeed {
    /// Native, a same-OS cross-arch the toolchain handles itself (macOS/Windows), or a
    /// cross-OS target (whose toolchain is supplied by the build driver, not a linker entry).
    None,
    /// Linux same-OS cross-arch: a `[target.<triple>] linker` entry we can auto-add.
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

/// Add `[target.<triple>] linker = "<linker>"` to the project's cargo config,
/// preserving existing content and comments. No-op if the entry already exists.
pub fn add_linker_entry(root: &Path, triple: &str, linker: &str) -> anyhow::Result<()> {
    let (path, mut doc) = match read_config(root) {
        Some((path, content)) => {
            let doc = content
                .parse::<DocumentMut>()
                .with_context(|| format!("Failed to parse {}", path.display()))?;
            (path, doc)
        }
        None => (root.join(".cargo").join("config.toml"), DocumentMut::new()),
    };

    let target_existed = doc.get("target").is_some();
    let target_tbl = doc
        .entry("target")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .context("`target` is not a table in the cargo config")?;
    // Keep only the `[target.<triple>]` sub-header when we create `target` fresh.
    if !target_existed {
        target_tbl.set_implicit(true);
    }

    let triple_tbl = target_tbl
        .entry(triple)
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .with_context(|| format!("`target.{triple}` is not a table in the cargo config"))?;
    if !triple_tbl.contains_key("linker") {
        triple_tbl["linker"] = value(linker);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
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

        fn config(&self) -> String {
            std::fs::read_to_string(self.0.join(".cargo/config.toml")).unwrap()
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
    fn add_linker_entry_creates_config() {
        let root = TempRoot::new("creates");
        let triple = "aarch64-unknown-linux-gnu";
        add_linker_entry(root.path(), triple, "aarch64-linux-gnu-gcc").unwrap();

        let content = root.config();
        let doc: DocumentMut = content.parse().unwrap();
        assert_eq!(
            doc["target"][triple]["linker"].as_str(),
            Some("aarch64-linux-gnu-gcc")
        );
        assert!(linker_configured(root.path(), triple));
    }

    #[test]
    fn add_linker_entry_preserves_existing_content() {
        let root = TempRoot::new("preserves");
        let cargo_dir = root.path().join(".cargo");
        std::fs::create_dir_all(&cargo_dir).unwrap();
        std::fs::write(
            cargo_dir.join("config.toml"),
            "# keep me\n[build]\njobs = 4\n",
        )
        .unwrap();

        let triple = "aarch64-unknown-linux-gnu";
        add_linker_entry(root.path(), triple, "aarch64-linux-gnu-gcc").unwrap();

        let content = root.config();
        assert!(content.contains("# keep me"), "comment dropped: {content}");
        assert!(
            content.contains("jobs = 4"),
            "build table dropped: {content}"
        );
        let doc: DocumentMut = content.parse().unwrap();
        assert_eq!(
            doc["target"][triple]["linker"].as_str(),
            Some("aarch64-linux-gnu-gcc")
        );
    }

    #[test]
    fn add_linker_entry_is_idempotent() {
        let root = TempRoot::new("idempotent");
        let triple = "aarch64-unknown-linux-gnu";
        add_linker_entry(root.path(), triple, "first-linker").unwrap();
        // A second call must not overwrite an existing linker entry.
        add_linker_entry(root.path(), triple, "second-linker").unwrap();

        let doc: DocumentMut = root.config().parse().unwrap();
        assert_eq!(
            doc["target"][triple]["linker"].as_str(),
            Some("first-linker")
        );
    }

    #[test]
    fn linker_configured_false_when_absent() {
        let root = TempRoot::new("absent");
        assert!(!linker_configured(root.path(), "aarch64-unknown-linux-gnu"));
    }
}
