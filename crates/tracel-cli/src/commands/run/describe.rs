use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context;
use cargo_metadata::{DependencyKind, Metadata};
use serde::Deserialize;
use serde_json::json;
use tracel_job::{DefinitionsFile, JobDefinition, PROTOCOL, ParseError, TRACEL_DESCRIBE};

use super::launch::wait_until;
use crate::error::{CliError, ErrorKind};
use crate::tools::build_driver::BuildDriver;
use crate::tools::cargo::{self, Binary};
use crate::tools::fs::file_sha256_and_size;
use crate::tools::workspace::WorkspaceInfo;
use crate::ui::Terminal;

/// The crate a package depends on to define jobs.
const SDK_CRATE: &str = "tracel";
/// How long a program has to write its job definitions.
const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(10);
/// The hint for definitions this CLI can't read.
const SAME_PROTOCOL: &str = "Use a tracel CLI and a tracel crate that support the same protocol.";

/// Find the job named `name` among `definitions`.
pub fn find_job<'a>(
    definitions: &'a DefinitionsFile,
    name: &str,
) -> Result<&'a JobDefinition, CliError> {
    if let Some(job) = definitions.jobs.iter().find(|job| job.name == name) {
        return Ok(job);
    }
    let names: Vec<&str> = definitions
        .jobs
        .iter()
        .map(|job| job.name.as_str())
        .collect();
    let message = match names.as_slice() {
        [] => format!("No job '{name}'. The program defines no jobs."),
        names => format!("No job '{name}'. Jobs: {}.", names.join(", ")),
    };
    Err(CliError::new(ErrorKind::Usage, message))
}

/// Say why the definitions the program `name` wrote can't be read.
fn unreadable_definitions(name: &str, error: ParseError) -> CliError {
    let message = match error {
        ParseError::Protocol { found } => format!(
            "`{name}` lists its jobs with runner protocol {found}, but this CLI reads protocol {PROTOCOL}."
        ),
        ParseError::Invalid(error) => {
            format!("`{name}` listed its jobs in a form this CLI can't read: {error}")
        }
    };
    CliError::new(ErrorKind::Internal, message).with_hint(SAME_PROTOCOL)
}

/// A workspace package, as finding the jobs sees it.
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

/// Choose the binary whose jobs `tracel run` runs: the one of the packages that depend on
/// the `tracel` crate, or `bin` among them. As with `cargo run`, a package's `default-run`
/// picks one of its binaries.
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
            "No package that depends on the `tracel` crate has a binary, so there are no jobs to run."
        } else {
            "No package in this workspace depends on the `tracel` crate, so there are no jobs to run."
        };
        return Err(CliError::new(ErrorKind::Usage, message)
            .with_hint("Run the program with `tracel run -- <args>` instead."));
    }
    if let Some(bin) = bin {
        return cargo::find_binary(&candidates, bin);
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
    let names: Vec<&str> = candidates
        .iter()
        .map(|binary| binary.name.as_str())
        .collect();
    Err(CliError::new(
        ErrorKind::Usage,
        format!(
            "Several binaries depend on the `tracel` crate: {}.",
            names.join(", ")
        ),
    )
    .with_hint("Pass --bin <name> to choose one."))
}

/// Read the cached definitions in `contents` when they were written for the binary with
/// `sha256`.
pub fn cached_definitions(contents: &str, sha256: &str) -> Option<DefinitionsFile> {
    #[derive(Deserialize)]
    struct Cache {
        sha256: String,
        definitions: DefinitionsFile,
    }
    let cache: Cache = serde_json::from_str(contents).ok()?;
    (cache.sha256 == sha256 && cache.definitions.protocol == PROTOCOL).then_some(cache.definitions)
}

fn write_cache(path: &Path, sha256: &str, definitions: &DefinitionsFile) -> anyhow::Result<()> {
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
    pub definitions: DefinitionsFile,
}

/// Build the binary whose jobs `tracel run` runs, in release, and read its job definitions.
///
/// The definitions are cached in `target/tracel/jobs.json` under the SHA-256 of the binary,
/// so the binary only runs again once it changes.
pub fn describe(
    terminal: &Terminal,
    workspace: &WorkspaceInfo,
    bin: Option<&str>,
) -> anyhow::Result<Described> {
    let binary = choose_binary(&package_binaries(&workspace.metadata), bin)?;
    let program = build(terminal, &binary)?;
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
            let definitions = run_describe(&program, &binary.name)?;
            if let Err(error) = write_cache(&cache, &sha256, &definitions) {
                terminal.warning(&format!(
                    "Can't cache the job definitions in {}: {error:#}",
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

fn build(terminal: &Terminal, binary: &Binary) -> anyhow::Result<PathBuf> {
    let (command, label) = cargo::release_build(BuildDriver::Cargo, None, None, Some(binary));
    let building = format!("Building {}", binary.name);
    cargo::build(terminal, command, &label, &building)?
        .into_iter()
        .find(|(name, _)| *name == binary.name)
        .map(|(_, path)| path)
        .with_context(|| format!("`{label}` made no binary '{}'", binary.name))
}

/// Run `program` with `TRACEL_DESCRIBE` set, and read the definitions it writes.
fn run_describe(program: &Path, name: &str) -> anyhow::Result<DefinitionsFile> {
    let directory = tempfile::tempdir().context("Can't create a temporary directory")?;
    let path = directory.path().join("jobs.json");
    let mut child = Command::new(program)
        .env(TRACEL_DESCRIBE, &path)
        // Describing creates no experiment, so it needs no Console settings.
        .env("TRACEL_TARGET", "offline")
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .spawn()
        .with_context(|| format!("Can't run {}", program.display()))?;

    let started = Instant::now();
    let Some(status) = wait_until(&mut child, || started.elapsed() >= DESCRIBE_TIMEOUT)? else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(CliError::new(
            ErrorKind::Timeout,
            format!(
                "`{name}` didn't list its jobs within {} seconds.",
                DESCRIBE_TIMEOUT.as_secs()
            ),
        )
        .with_hint(
            "To list its jobs, the program must reach `Jobs::run` quickly; move slow setup into its jobs.",
        )
        .into());
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
        .into());
    }
    read_definitions(&path, name)
}

/// Read the definitions the program `name` wrote to `path`, when they follow the
/// protocol this CLI reads.
fn read_definitions(path: &Path, name: &str) -> anyhow::Result<DefinitionsFile> {
    let json = match std::fs::read(path) {
        Ok(json) => json,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(CliError::new(
                ErrorKind::Usage,
                format!("`{name}` didn't list its jobs."),
            )
            .with_hint(
                "Define its jobs with `tracel::runtime::Jobs`, or run it with `tracel run -- <args>`.",
            )
            .into());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("Can't read {}", path.display()));
        }
    };
    DefinitionsFile::from_json(&json).map_err(|error| unreadable_definitions(name, error).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::classify;
    use crate::test_support::job;

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

    fn definitions(names: &[&str]) -> DefinitionsFile {
        DefinitionsFile::new(names.iter().map(|name| job(name, None, None)).collect())
    }

    fn cache(definitions: &DefinitionsFile) -> String {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("jobs.json");
        write_cache(&path, "abc", definitions).unwrap();
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn given_a_job_the_program_defines_when_finding_it_then_it_is_found() {
        let jobs = definitions(&["train", "evaluate"]);

        let found = find_job(&jobs, "evaluate").unwrap();

        assert_eq!(found.name, "evaluate");
    }

    #[test]
    fn given_an_unknown_job_when_finding_it_then_the_error_lists_the_jobs() {
        let jobs = definitions(&["train", "evaluate"]);

        let error = find_job(&jobs, "serve").unwrap_err();

        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(error.to_string().contains("train, evaluate"), "{error}");
    }

    #[test]
    fn given_a_program_without_jobs_when_finding_one_then_the_error_says_so() {
        let jobs = definitions(&[]);

        let error = find_job(&jobs, "serve").unwrap_err();

        assert!(error.to_string().contains("no jobs"), "{error}");
    }

    #[test]
    fn given_a_program_that_wrote_no_definitions_when_reading_them_then_it_did_not_list_its_jobs() {
        let directory = tempfile::tempdir().unwrap();

        let error = read_definitions(&directory.path().join("jobs.json"), "trainer").unwrap_err();

        assert_eq!(classify(&error), ErrorKind::Usage);
        assert!(error.to_string().contains("`trainer`"), "{error}");
    }

    #[test]
    fn given_one_binary_using_the_sdk_when_choosing_the_binary_then_it_is_chosen() {
        let packages = [
            package("tools", false, &["fmt-data"]),
            package("trainer", true, &["train"]),
        ];

        let chosen = choose_binary(&packages, None).unwrap();

        assert_eq!(chosen, binary("trainer", "train"));
    }

    #[test]
    fn given_bin_when_choosing_the_binary_then_it_picks_among_those_using_the_sdk() {
        let packages = [
            package("trainer", true, &["train"]),
            package("server", true, &["serve"]),
        ];

        let chosen = choose_binary(&packages, Some("serve")).unwrap();

        assert_eq!(chosen, binary("server", "serve"));
    }

    #[test]
    fn given_default_run_in_the_only_package_using_the_sdk_when_choosing_the_binary_then_it_decides()
     {
        let trainer = PackageBinaries {
            default_run: Some("evaluate".to_string()),
            ..package("trainer", true, &["train", "evaluate"])
        };

        let chosen = choose_binary(&[trainer], None).unwrap();

        assert_eq!(chosen, binary("trainer", "evaluate"));
    }

    #[test]
    fn given_several_binaries_using_the_sdk_when_choosing_the_binary_then_the_error_lists_them() {
        let packages = [
            package("trainer", true, &["train"]),
            package("server", true, &["serve"]),
        ];

        let error = choose_binary(&packages, None).unwrap_err();

        assert!(error.to_string().contains("train, serve"), "{error}");
    }

    #[test]
    fn given_no_package_using_the_sdk_when_choosing_the_binary_then_there_are_no_jobs_to_run() {
        let packages = [package("tools", false, &["fmt-data"])];

        let error = choose_binary(&packages, None).unwrap_err();

        assert!(error.to_string().contains("in this workspace"), "{error}");
    }

    #[test]
    fn given_a_package_using_the_sdk_without_a_binary_when_choosing_the_binary_then_there_are_no_jobs_to_run()
     {
        let packages = [package("model", true, &[])];

        let error = choose_binary(&packages, None).unwrap_err();

        assert!(error.to_string().contains("has a binary"), "{error}");
    }

    #[test]
    fn given_cached_definitions_of_the_same_binary_when_reading_the_cache_then_they_are_used() {
        let contents = cache(&definitions(&["train"]));

        let cached = cached_definitions(&contents, "abc");

        assert_eq!(cached, Some(definitions(&["train"])));
    }

    #[test]
    fn given_cached_definitions_of_another_binary_when_reading_the_cache_then_they_are_not_used() {
        let contents = cache(&definitions(&["train"]));

        let cached = cached_definitions(&contents, "def");

        assert_eq!(cached, None);
    }

    #[test]
    fn given_cached_definitions_of_another_protocol_when_reading_the_cache_then_they_are_not_used()
    {
        let contents =
            cache(&definitions(&["train"])).replace(r#""protocol": 1"#, r#""protocol": 2"#);

        let cached = cached_definitions(&contents, "abc");

        assert_eq!(cached, None);
    }
}
