use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context;
use cargo_metadata::{DependencyKind, Metadata};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{CliError, ErrorKind};
use crate::tools::cargo;
use crate::tools::fs::file_sha256_and_size;
use crate::tools::workspace::WorkspaceInfo;
use crate::ui::Terminal;

/// The crate a package depends on to register jobs.
const SDK_CRATE: &str = "tracel";
/// The runner protocol version this CLI reads.
const PROTOCOL: u32 = 1;
/// How long a program has to write its job definitions.
const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The file a program writes to the path in `TRACEL_DESCRIBE`: the jobs it can run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Definitions {
    pub protocol: u32,
    pub sdk_version: String,
    pub runner: String,
    pub jobs: Vec<JobDefinition>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobDefinition {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub input_schema: Option<Value>,
    #[serde(default)]
    pub input_example: Option<Value>,
}

impl Definitions {
    /// Parse a definitions file of this protocol version.
    pub fn parse(contents: &str) -> anyhow::Result<Self> {
        #[derive(Deserialize)]
        struct Header {
            protocol: u32,
        }
        let header: Header =
            serde_json::from_str(contents).context("The job definitions are not valid JSON")?;
        if header.protocol != PROTOCOL {
            return Err(CliError::new(
                ErrorKind::Internal,
                format!(
                    "The program lists its jobs with runner protocol {}, but this CLI reads protocol {PROTOCOL}.",
                    header.protocol
                ),
            )
            .with_hint("Use a tracel CLI and a tracel crate that support the same protocol.")
            .into());
        }
        serde_json::from_str(contents).context("The job definitions are not valid")
    }

    /// The job named `name`.
    pub fn job(&self, name: &str) -> Result<&JobDefinition, CliError> {
        if let Some(job) = self.jobs.iter().find(|job| job.name == name) {
            return Ok(job);
        }
        let message = if self.jobs.is_empty() {
            format!("Unknown job '{name}': the program registers no jobs.")
        } else {
            let names: Vec<&str> = self.jobs.iter().map(|job| job.name.as_str()).collect();
            format!("Unknown job '{name}'. Valid values: {}.", names.join(", "))
        };
        Err(CliError::new(ErrorKind::Usage, message)
            .with_hint("List the jobs with `tracel run --list`."))
    }
}

/// A workspace package, as job discovery sees it.
#[derive(Clone, Debug)]
pub struct PackageBinaries {
    pub package: String,
    /// Whether the package depends on the `tracel` crate.
    pub uses_sdk: bool,
    pub bins: Vec<String>,
    pub default_run: Option<String>,
}

pub fn package_binaries(metadata: &Metadata) -> Vec<PackageBinaries> {
    metadata
        .workspace_packages()
        .into_iter()
        .map(|package| PackageBinaries {
            package: package.name.to_string(),
            uses_sdk: package.dependencies.iter().any(|dependency| {
                dependency.name == SDK_CRATE && dependency.kind == DependencyKind::Normal
            }),
            bins: package
                .targets
                .iter()
                .filter(|target| target.is_bin())
                .map(|target| target.name.clone())
                .collect(),
            default_run: package.default_run.clone(),
        })
        .collect()
}

/// A binary target of a workspace package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binary {
    pub package: String,
    pub name: String,
}

/// The binary whose jobs `tracel run` runs: the one of the packages that depend on the
/// `tracel` crate, or `bin` among them. Like `cargo run`, a package's `default-run` picks one
/// of its binaries.
pub fn choose_binary(packages: &[PackageBinaries], bin: Option<&str>) -> Result<Binary, CliError> {
    let candidates: Vec<Binary> = packages
        .iter()
        .filter(|package| package.uses_sdk)
        .flat_map(|package| {
            package.bins.iter().map(|name| Binary {
                package: package.package.clone(),
                name: name.clone(),
            })
        })
        .collect();
    if candidates.is_empty() {
        let message = if packages.iter().any(|package| package.uses_sdk) {
            "No package that depends on the `tracel` crate has a binary target, so there are no jobs to run."
        } else {
            "No package in this workspace depends on the `tracel` crate, so there are no jobs to run."
        };
        return Err(CliError::new(ErrorKind::Usage, message)
            .with_hint("Run the program with `tracel run -- <args>` instead."));
    }

    let names: Vec<&str> = candidates
        .iter()
        .map(|binary| binary.name.as_str())
        .collect();
    if let Some(bin) = bin {
        return candidates
            .iter()
            .find(|binary| binary.name == bin)
            .cloned()
            .ok_or_else(|| cargo::invalid_bin(bin, &names));
    }
    if let [binary] = candidates.as_slice() {
        return Ok(binary.clone());
    }
    let package = &candidates[0].package;
    if candidates.iter().all(|binary| &binary.package == package) {
        let default_run = packages
            .iter()
            .find(|candidate| &candidate.package == package)
            .and_then(|package| package.default_run.as_deref());
        if let Some(binary) = candidates
            .iter()
            .find(|binary| Some(binary.name.as_str()) == default_run)
        {
            return Ok(binary.clone());
        }
    }
    Err(CliError::new(
        ErrorKind::Usage,
        format!(
            "Several binaries depend on the `tracel` crate: {}.",
            names.join(", ")
        ),
    )
    .with_hint("Pass --bin <NAME> to choose one."))
}

/// The cached definitions in `contents` when they were written for the binary with `sha256`.
pub fn cached_definitions(contents: &str, sha256: &str) -> Option<Definitions> {
    #[derive(Deserialize)]
    struct Cache {
        sha256: String,
        definitions: Definitions,
    }
    let cache: Cache = serde_json::from_str(contents).ok()?;
    (cache.sha256 == sha256 && cache.definitions.protocol == PROTOCOL).then_some(cache.definitions)
}

fn write_cache(path: &Path, sha256: &str, definitions: &Definitions) -> anyhow::Result<()> {
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }
    let contents = serde_json::to_vec_pretty(&json!({
        "sha256": sha256,
        "definitions": definitions,
    }))?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The jobs of a workspace binary, and where it is built.
pub struct Described {
    pub binary: Binary,
    pub program: PathBuf,
    pub definitions: Definitions,
}

/// Build the binary whose jobs `tracel run` runs, and read its job definitions.
///
/// The definitions are cached in `target/tracel/jobs.json` under the SHA-256 of the binary,
/// so the binary only runs again once it changes.
pub fn describe(
    terminal: &Terminal,
    workspace: &WorkspaceInfo,
    bin: Option<&str>,
) -> anyhow::Result<Described> {
    let binary = choose_binary(&package_binaries(&workspace.metadata), bin)?;
    let program = build(&binary)?;
    let (sha256, _) = file_sha256_and_size(&program)?;
    let cache = workspace
        .metadata
        .target_directory
        .as_std_path()
        .join("tracel")
        .join("jobs.json");
    let cached = std::fs::read_to_string(&cache)
        .ok()
        .and_then(|contents| cached_definitions(&contents, &sha256));
    let definitions = match cached {
        Some(definitions) => definitions,
        None => {
            let definitions = run_describe(&program, &binary.name, DESCRIBE_TIMEOUT)?;
            if let Err(error) = write_cache(&cache, &sha256, &definitions) {
                terminal.print_warning(&format!(
                    "Could not cache the job definitions in {}: {error:#}",
                    cache.display()
                ));
            }
            definitions
        }
    };
    Ok(Described {
        binary,
        program,
        definitions,
    })
}

fn build(binary: &Binary) -> anyhow::Result<PathBuf> {
    let label = format!(
        "cargo build --package {} --bin {}",
        binary.package, binary.name
    );
    let mut command = cargo::command();
    command
        .arg("build")
        .arg("--package")
        .arg(&binary.package)
        .arg("--bin")
        .arg(&binary.name);
    cargo::build_executables(command, &label)?
        .into_iter()
        .find(|(name, _)| *name == binary.name)
        .map(|(_, path)| path)
        .ok_or_else(|| anyhow::anyhow!("`{label}` did not produce the binary {}", binary.name))
}

/// Run `program` with `TRACEL_DESCRIBE` set, and read the definitions it writes.
fn run_describe(program: &Path, name: &str, timeout: Duration) -> anyhow::Result<Definitions> {
    let directory = tempfile::tempdir().context("Failed to create a temporary directory")?;
    let path = directory.path().join("jobs.json");
    let mut child = Command::new(program)
        .env("TRACEL_DESCRIBE", &path)
        // Describing creates no experiment, so it needs no Console settings.
        .env("TRACEL_TARGET", "offline")
        .env_remove("TRACEL_REPORT_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .spawn()
        .with_context(|| format!("Failed to run {}", program.display()))?;

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CliError::new(
                ErrorKind::Timeout,
                format!(
                    "`{name}` did not list its jobs within {} seconds.",
                    timeout.as_secs()
                ),
            )
            .with_hint(
                "With TRACEL_DESCRIBE set, the program must reach `Cli::run` quickly; move slow setup into its jobs.",
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if !status.success() {
        let ended = match status.code() {
            Some(code) => format!("exited with code {code}"),
            None => "was ended by a signal".to_string(),
        };
        return Err(CliError::new(
            ErrorKind::Internal,
            format!("`{name}` {ended} instead of listing its jobs."),
        )
        .with_hint("Its error is above.")
        .into());
    }
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(CliError::new(
                ErrorKind::Usage,
                format!("`{name}` did not list its jobs."),
            )
            .with_hint(
                "Register its jobs with `tracel::app::cli::Cli` from tracel 0.10 or later, or run it with `tracel run -- <args>`.",
            )
            .into());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to read {}", path.display()));
        }
    };
    Definitions::parse(&contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFINITIONS: &str = r#"{
        "protocol": 1,
        "sdk_version": "0.10.0",
        "runner": "cli",
        "jobs": [
            {
                "name": "toy-training",
                "kind": "experiment",
                "description": "Run a toy training loop",
                "input_schema": {"type": "object"},
                "input_example": {"epochs": 3}
            },
            {
                "name": "wordtok",
                "kind": "inference",
                "description": null,
                "input_schema": null,
                "input_example": null
            }
        ]
    }"#;

    fn package(name: &str, uses_sdk: bool, bins: &[&str]) -> PackageBinaries {
        PackageBinaries {
            package: name.to_string(),
            uses_sdk,
            bins: bins.iter().map(|bin| bin.to_string()).collect(),
            default_run: None,
        }
    }

    fn binary(package: &str, name: &str) -> Binary {
        Binary {
            package: package.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn definitions_parse_and_find_jobs() {
        let definitions = Definitions::parse(DEFINITIONS).unwrap();
        assert_eq!(definitions.sdk_version, "0.10.0");
        assert_eq!(definitions.runner, "cli");
        let job = definitions.job("toy-training").unwrap();
        assert_eq!(job.kind, "experiment");
        assert_eq!(job.input_example, Some(json!({"epochs": 3})));
        assert_eq!(definitions.job("wordtok").unwrap().input_schema, None);

        let error = definitions.job("train").unwrap_err();
        assert_eq!(error.kind, ErrorKind::Usage);
        assert_eq!(
            error.to_string(),
            "Unknown job 'train'. Valid values: toy-training, wordtok."
        );
        let empty = Definitions {
            jobs: Vec::new(),
            ..definitions.clone()
        };
        assert!(
            empty
                .job("train")
                .unwrap_err()
                .to_string()
                .contains("no jobs")
        );

        let value: Value = serde_json::from_str(DEFINITIONS).unwrap();
        assert_eq!(serde_json::to_value(&definitions).unwrap(), value);
    }

    #[test]
    fn other_protocols_are_rejected() {
        let error = Definitions::parse(r#"{"protocol": 2, "jobs": "elsewhere"}"#).unwrap_err();
        assert_eq!(crate::error::classify(&error), ErrorKind::Internal);
        assert!(error.to_string().contains("protocol 2"));
        assert!(Definitions::parse("{").is_err());
        assert!(Definitions::parse(r#"{"protocol": 1}"#).is_err());
    }

    #[test]
    fn only_binaries_of_packages_using_the_sdk_are_candidates() {
        let packages = [
            package("tools", false, &["fmt-data"]),
            package("trainer", true, &["train"]),
            package("model", true, &[]),
        ];
        assert_eq!(
            choose_binary(&packages, None).unwrap(),
            binary("trainer", "train")
        );
        assert_eq!(
            choose_binary(&packages, Some("train")).unwrap(),
            binary("trainer", "train")
        );
        let error = choose_binary(&packages, Some("fmt-data")).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Usage);
        assert_eq!(
            error.to_string(),
            "Invalid --bin 'fmt-data'. Valid values: train."
        );
    }

    #[test]
    fn without_a_package_using_the_sdk_plain_runs_are_suggested() {
        for packages in [
            vec![package("tools", false, &["fmt-data"])],
            vec![package("model", true, &[])],
            vec![],
        ] {
            let error = choose_binary(&packages, None).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Usage);
            assert!(error.to_string().contains("`tracel` crate"), "{error}");
            let error = anyhow::Error::from(error);
            let report = crate::error::ErrorReport::new(&error);
            assert!(report.hint.unwrap().contains("tracel run -- <args>"));
        }
    }

    #[test]
    fn several_candidates_need_bin_unless_default_run_picks_one() {
        let packages = [
            package("trainer", true, &["train", "evaluate"]),
            package("server", true, &["serve"]),
        ];
        let error = choose_binary(&packages, None).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Usage);
        assert_eq!(
            error.to_string(),
            "Several binaries depend on the `tracel` crate: train, evaluate, serve."
        );
        assert_eq!(
            choose_binary(&packages, Some("serve")).unwrap(),
            binary("server", "serve")
        );

        let mut trainer = package("trainer", true, &["train", "evaluate"]);
        trainer.default_run = Some("evaluate".to_string());
        assert_eq!(
            choose_binary(std::slice::from_ref(&trainer), None).unwrap(),
            binary("trainer", "evaluate")
        );
        // `default-run` chooses within its package only.
        assert!(choose_binary(&[trainer, package("server", true, &["serve"])], None).is_err());
    }

    #[test]
    fn the_cache_hits_only_for_the_same_binary_and_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracel").join("jobs.json");
        let definitions = Definitions::parse(DEFINITIONS).unwrap();
        write_cache(&path, "abc", &definitions).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            cached_definitions(&contents, "abc"),
            Some(definitions.clone())
        );
        assert_eq!(cached_definitions(&contents, "def"), None);
        assert_eq!(cached_definitions("{", "abc"), None);
        let other_protocol = contents.replace(r#""protocol": 1"#, r#""protocol": 2"#);
        assert_eq!(cached_definitions(&other_protocol, "abc"), None);
        assert!(!dir.path().join("tracel").join("jobs.json.tmp").exists());
    }
}
