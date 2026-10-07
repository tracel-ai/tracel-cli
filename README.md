<div align="center">

<h1>Tracel CLI</h1>

[![Current Crates.io Version](https://img.shields.io/crates/v/tracel-cli)](https://crates.io/crates/tracel-cli)
[![Minimum Supported Rust Version](https://img.shields.io/crates/msrv/tracel-cli)](https://crates.io/crates/tracel-cli)
[![Test Status](https://github.com/tracel-ai/tracel-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/tracel-ai/tracel-cli/actions/workflows/ci.yml)
![license](https://shields.io/badge/license-MIT%2FApache--2.0-blue)

---

</div>

## Description

The Tracel CLI (`tracel`) is the command-line tool for interacting with [Tracel Console](https://console.tracel.ai/), the centralized platform for experiment tracking, model sharing, and deployment for [Burn](https://github.com/tracel-ai/burn) users.

This CLI works in conjunction with the [Tracel SDK](https://github.com/tracel-ai/tracetracell) to provide a seamless workflow for:

- Running training jobs locally or remotely
- Managing experiments and tracking metrics
- Packaging and deploying models
- Integrating with compute providers
- Managing project configurations

## Installation

### Install from crates.io

```bash
cargo install tracel-cli
```

### Build from source

```bash
git clone https://github.com/tracel-ai/tracel-cli.git
cd tracel-cli
cargo install --path crates/tracel-cli
```

After installation, the `tracel` command will be available in your terminal.

## Prerequisites

1. **Tracel Account**: Create an account at [console.tracel.ai](https://console.tracel.ai/)
2. **Rust**: Version 1.87.0 or higher
3. **Tracel SDK**: Add the SDK to your Burn project

## Commands

All commands accept `--json` or `-o, --output <auto|human|json>`. The default
`auto` format uses human output on a terminal and JSON when stdout is redirected.
`TRACEL_OUTPUT=human` or `TRACEL_OUTPUT=json` sets the format when no output flag
is given. `--json` and `--output` cannot be combined.

All commands also accept `--no-input` to disable prompts. Prompts require stdin
and stderr to be terminals and human output. Missing input in scripts produces
`USAGE`, and a missing yes/no confirmation produces `CONFIRMATION_REQUIRED`, each
with a hint naming the flag to pass.

Select a project with global `--project <namespace>/<name>`. It takes precedence
over `TRACEL_NAMESPACE` and `TRACEL_PROJECT`, which each independently fall back
to `tracel.toml` at the Cargo workspace root. `project`, `experiments`, `jobs`,
`models`, `artifacts`, and `datasets` work from any directory with a flag or
both variables; `package` and `run` still require a Cargo workspace. Global `-C
<dir>` runs as if started in that directory. `init` and `unlink` operate on
`tracel.toml` and ignore project overrides.

```bash
tracel --project alice/demo project --json
TRACEL_NAMESPACE=alice TRACEL_PROJECT=demo tracel -C ./trainer project --json
```

JSON results are one line on stdout: `{"ok":true,"data":{...}}` on success, or
`{"ok":false,"error":{"code":"NOT_FOUND","message":"...","hint":null,"exit_code":5}}`
on failure. Diagnostics go to stderr. Help and version output retain their normal
format, and `tracel run` without a job, `--list`, or `--remote` inherits the
executed program's output and exit code.

Human output keeps the same split: stdout carries only results, such as the
tables and details of commands that read, while progress, prompts, warnings,
and errors go to stderr. Commands that change something, such as `init`,
`login`, `models push`, or `artifacts download`, report on stderr and print
nothing on stdout. `auth token` prints the bare token unless JSON is requested,
even when stdout is redirected. On a terminal, tables shorten long descriptions
to fit its width; redirected output is never shortened.

| Error code | Exit code |
| --- | --- |
| Success | 0 |
| `INTERNAL` | 1 |
| `USAGE` | 2 |
| `NOT_AUTHENTICATED` | 3 |
| `FORBIDDEN` | 4 |
| `NOT_FOUND` | 5 |
| `CONFLICT` | 6 |
| `CONFIRMATION_REQUIRED` | 7 |
| `LIMIT_REACHED` | 8 |
| `JOB_FAILED` | 9 |
| `TIMEOUT` | 10 |
| `UNAVAILABLE` | 11 |

### `tracel run`

Run a job your program registers, locally or on a compute provider, or run your
program as `cargo run` does.

```bash
# List the jobs of the workspace binary that uses the tracel crate
tracel run --list
# Run a job with its example input, changing two values
tracel run mnist --set num_epochs=5 --set optimizer.lr=0.01
# Merge JSON files onto the input, then set a value
tracel run mnist -c base.json -c gpu.json --set batch_size=64
# Start from the input Console experiment 42 recorded
tracel run mnist --like 42 --set num_epochs=20
# Record the run on this machine
tracel run mnist --offline
# Queue the job on the `gpu` compute provider group and follow it
tracel run mnist --set num_epochs=5 --remote gpu --mode source --yes --follow

# Equivalent to `cargo run`
tracel run
# Equivalent to `cargo run -- train mnist --epochs 100`
tracel run -- train mnist --epochs 100
```

Jobs come from the binary of a workspace package that depends on the `tracel`
crate (dev and build dependencies do not count). With several such binaries,
pass `--bin <NAME>`, unless the package's `default-run` names one, as with
`cargo run`. In a workspace without such a package, `--list` and job runs fail
with `USAGE`; run the program with `tracel run -- <args>` instead. The binary
must run its jobs with `tracel::app::cli::Cli` (tracel 0.10 or later).

The binary is built with `cargo build`, then run with `TRACEL_DESCRIBE=<path>`,
which makes it write its job definitions to that path and exit without running a
job. It must do so within 10 seconds, or it is killed and the command fails with
`TIMEOUT` (exit code 10). The definitions are cached in `target/tracel/jobs.json`
under the SHA-256 of the binary, so the binary only describes its jobs again
after it changes.

`--list` prints a `NAME`, `KIND`, `DESCRIPTION` table. JSON data is the
definitions file; `kind` is `experiment` or `inference`, and `input_schema` and
`input_example` are `null` when the job has none:

```json
{"protocol":1,"sdk_version":"0.10.0","runner":"cli","jobs":[{"name":"mnist","kind":"experiment","description":"Train the MNIST classifier","input_schema":{"type":"object","properties":{"num_epochs":{"type":"integer"}}},"input_example":{"num_epochs":10,"optimizer":{"lr":0.001}}}]}
```

`tracel run <JOB>` resolves the job's input in order:

1. It starts from the job's `input_example`, or `null` when it has none.
   `--like <NUM|latest>` starts from the config that Console experiment recorded
   instead, which needs a linked project and a login.
2. Each `-c, --config <FILE>` is a JSON file merged onto the input with JSON merge
   patch (RFC 7386): objects merge, other values replace, and `null` removes a
   field.
3. Each `--set <PATH>=<VALUE>` sets one value. `PATH` is dotted keys with `[i]`
   array indices, such as `optimizer.lr` or `layers[0].size`; missing objects and
   arrays are created, and the index one past the end of an array appends.
   `VALUE` is JSON, or a string when it does not parse as JSON: `--set epochs=5`
   sets a number and `--set name=baseline` a string.

When the job has an `input_schema`, the input is validated against it; references
outside the schema are not fetched. An invalid input fails with `USAGE` naming
each path and reason, such as `epochs: "x" is not of type "integer"`, and an
unknown job fails with `USAGE` listing the job names. A `JOB` cannot be combined
with arguments after `--`. `-c`, `--set`, `--like`, and `--offline` require a
`JOB`, and `--bin` requires a `JOB`, `--list`, or `--remote`.

The job then runs as `<binary> <JOB> <input-json>`, without `cargo run`. Its
stdout and stderr are inherited and its stdin is closed. In JSON mode, its stdout
goes to stderr, so stdout holds only the command's JSON.

The run is recorded on the Tracel Console when a project is linked (`tracel.toml`,
`--project`, or `TRACEL_NAMESPACE` and `TRACEL_PROJECT`) and a credential is
available (`TRACEL_API_KEY`, or a `tracel login` that has not ended). Otherwise
it is recorded offline, and a line on stderr says why. `--offline` always records
offline. The program gets:

| Variable | Value |
| --- | --- |
| `TRACEL_TARGET` | `console` or `offline` |
| `TRACEL_ENV` | the Console environment |
| `TRACEL_NAMESPACE`, `TRACEL_PROJECT` | the project, when one is linked |
| `TRACEL_REPORT_FILE` | a temporary file for the run report |

The program writes the run report to `TRACEL_REPORT_FILE` when it creates the
experiment, and again when the experiment ends. Offline, `experiment` is `null`
and `dir` names the run directory. `status` is `running`, `completed`, `failed`,
or `cancelled`:

```json
{"protocol":1,"job":"mnist","experiment":{"num":42,"url":"https://console.tracel.ai/..."},"status":"completed","started_at":"2026-10-06T12:00:00Z","finished_at":"2026-10-06T12:30:00Z","error":null}
```

Human output names the experiment and its URL, or the offline run directory, as
soon as the report does. The command exits 0 when the report says `completed`,
and fails with `JOB_FAILED` (exit code 9) naming the job, its status, and the
experiment when it says `failed` or `cancelled`. Without a final report, the
program's exit code decides: 0 is completed, 2 fails with `USAGE` (the program
rejected the job or its input, and printed why), 130 is cancelled, and any other
code is failed; a program ended by a signal is cancelled when `tracel` stopped it,
and failed otherwise. JSON data is
`{"job":"mnist","input":{...},"target":"console","exit_code":0,"report":{...}}`,
with `report` `null` when the program wrote none.

Ctrl-C, SIGTERM, or SIGHUP stops the job: `tracel` sends the program SIGTERM, and
kills it when it is still running 30 seconds later. The program runs in its own
process group, so a Ctrl-C in the terminal reaches it only as that SIGTERM. On
Windows, the console delivers Ctrl-C to the program itself, and `tracel` kills it
when it is still running 30 seconds later.

`--remote <GROUP>` runs your project as a job on a compute provider group
instead. The workspace is packaged as with `tracel package`, with the same
`--mode`, `--target`, `--bin`, and `--install-targets` flags and the same rules
without prompts. With a `JOB`, the input is resolved and validated as above, and
the job command is `<JOB> '<input-json>'`; in binary mode the uploaded binary is
the one that registers the job. Without a `JOB`, the job command is the
arguments after `--`. Either way the arguments reach the program unchanged: the
job command single-quotes each argument that is empty, starts with `#`, or
contains a space, tab, newline, quote, or backslash. The packaging flags,
`--yes`, `--dry-run`, and `--follow` require `--remote`, and `--offline` cannot
be combined with it.

A job may incur costs, so after packaging, and before uploading anything, the
command asks for confirmation naming the project, group, code version digest,
and command. `-y, --yes` skips it. Without prompts, `--yes` is required,
otherwise the command fails with `CONFIRMATION_REQUIRED` (exit code 7) before
packaging. `--dry-run` packages and computes the digest without logging in,
uploading, or queuing, and needs no `--yes`; it cannot be combined with `--yes`
or `--follow`.

JSON data contains `job_num`, `job_id`, `group`, `digest`, `version_id`,
`command`, and `uploaded` (false when the same content was already packaged).
Human output shows the job number with `tracel jobs logs <N> --follow` and
`tracel jobs wait <N>`. `job_num` is null when the server does not return it;
find the job with `tracel jobs list`. A dry run returns `namespace`, `project`,
`group`, `mode`, `digest`, and `command`.

`--follow` follows the job's logs like `tracel jobs logs <N> --follow`, then
exits like `tracel jobs wait <N>`: 0 when the job completed, `JOB_FAILED` (exit
code 9) when it failed or was cancelled. JSON output is NDJSON: an item with
`"type":"queued"` and the data above, then the log and end items of
`jobs logs --follow`. Without a job number, `--follow` fails after queuing and
the job stays queued.

Without a `JOB`, `--list`, or `--remote`, `tracel run` is a thin wrapper around
`cargo run`: arguments are forwarded to your binary only after `--`, so
`tracel run -- <args>` is equivalent to `cargo run -- <args>`, and `tracel run`
runs the default binary with no arguments. stdin/stdout/stderr are inherited and
the binary's exit code is propagated.

### `tracel package`

Package your project for deployment on remote compute providers.

```bash
tracel package
# Source packaging without prompts
tracel package --mode source --json
# Build selected binaries and install missing targets without prompts
tracel package --mode binary --target x86_64-unknown-linux-gnu --bin trainer --install-targets --json
```

This creates a deployable artifact containing your code, dependencies, and configurations.

`--mode <binary|source>` is required without prompts. In binary mode, repeat
`--target <triple>` to choose targets; without prompts, omitting it builds for the
host. Use `--bin <name>` when several binaries are built. `--install-targets`
installs missing Rust targets without asking.

In source mode, the archive holds the files of the Cargo workspace, except those
matched by `.gitignore` or `.ignore` files in the workspace or its parent
directories, by `.git/info/exclude`, or by git's global excludes file, with or
without a git repository. `.git` and `target` directories are always left out.
Hidden files (names starting with `.`) are packaged inside a git repository unless
ignored, and left out otherwise. Packaging fails with `USAGE` when no file is left.

The code version digest is a SHA-256 of the packaged content: the hex digest of
`name:sha256` lines sorted by name, one per packaged file. In source mode the
names are the workspace-relative paths of the archived files; in binary mode they
are the target triples of the uploaded binaries. Packaging the same content again
gives the same digest, and any change gives a new one. JSON data contains
`namespace`, `project`, `digest`, `version_id`, `mode`, `targets`, and `uploaded`
(false when the same content was already packaged).

### `tracel login`

Authenticate with the Console platform. The CLI prints a link and a code to
approve in your browser, and the login then lasts seven days.

```bash
tracel login
```

For scripts, start a login with `--no-wait`, hand the approval link and user code
to a person, then finish with `--complete`:

```bash
tracel login --no-wait --json
tracel login --complete --json
# Wait up to 30 seconds, keeping the pending login if approval takes longer
tracel login --complete --timeout 30 --json
```

`--no-wait` saves the pending login locally and exits successfully without
waiting. Starting again replaces the previous pending login. JSON data contains
`status` (`"pending"`), `verification_uri_complete`, `user_code`, and `expires_at`
(RFC 3339). The device code is kept in an owner-only file beside the session
file and is never printed.

`--complete` waits for approval until the code expires, or until `--timeout
<seconds>` elapses. A timeout returns `TIMEOUT` (exit code 10); run `tracel login
--complete` again to continue. Expired or denied logins must be started again.
Successful completion returns the same JSON data as plain login: `username` and
`environment`. `--no-wait` and `--complete` conflict; `--timeout` requires
`--complete`. Plain `tracel login` still waits for approval.

When `TRACEL_API_KEY` is set, commands use that API key instead of the login.

### `tracel logout`

End the login on the server and forget it locally.

```bash
tracel logout
```

### `tracel auth`

```bash
# Which credential commands use, the user they act as, and when the login ends
tracel auth status

# Print an access token of the login for scripts; TRACEL_API_KEY is ignored
curl -H "Authorization: Bearer $(tracel auth token)" https://console.tracel.ai/api/v1/user/organizations
```

### `tracel init`

Initialize or reinitialize a Tracel project in the current directory.

```bash
# Interactive initialization
tracel init
# Initialize without prompts, accepting a project that already exists
tracel init --owner my-namespace --name my-project --description "" --yes --json
```

`--owner <namespace>` must name your own namespace or one of your organizations.
`--name <project>` accepts alphanumeric characters, underscores, and hyphens.
`--description <text>` supplies the new project's description; without a terminal
it defaults to empty. These flags replace the corresponding prompts. `--yes`
links an existing project without asking. `--force` reinitializes an already
linked project; without it, asking for a different project fails with `CONFLICT`.
JSON data contains `namespace`, `name`, `created`, and `url`; `created` is false
when an existing project is linked, and `url` is null when nothing changed.

### `tracel models`

Browse and manage models in the selected project's model registry.

```bash
tracel models list
tracel models get my-model
tracel models get my-model --version production --json
tracel models versions my-model --all
tracel models pull my-model --version v7 --directory ./weights --force
tracel --project alice/demo models push my-model --directory ./weights --auto-create true --description "Model weights" --metadata '{"format":"safetensors"}' --json
tracel models promote my-model --experiment 42 --artifact weights --alias production --metadata '{"accuracy":0.98}' --json
tracel models alias list my-model
tracel models alias set my-model production 7 --expect 6
tracel models alias remove my-model production
```

- `list` shows names, version counts, latest versions, aliases, and creation times.
- `get <MODEL>` shows model details. `--version <REF>` shows version details,
  files, and metadata. References accept `latest`, a number such as `7` or `v7`,
  or an alias, and are resolved by the server.
- `versions <MODEL>` lists ready versions. `--all` includes pending, failed,
  and deleted versions.
- `pull <MODEL>` defaults to `--version latest` and downloads into
  `./<MODEL>-v<resolved version>`. `-d, --directory <DIR>` selects another
  destination. `--force` overwrites existing files. Paths and overwrite conflicts
  are checked before downloading; declared sizes and SHA-256 checksums are
  verified before each file is saved.
- `push <MODEL> -d, --directory <DIR>` uploads local files as a new version.
  `-a, --auto-create <true|false>` creates a missing model when true and requires
  an existing model when false. Omit it to ask when interactive; a missing model
  without prompts requires `--auto-create true`. `--description <TEXT>` sets
  the new model's description and requires `--auto-create true`. `--metadata
  <JSON>` sets version metadata and must be a JSON object.
- `promote <MODEL> --experiment <NUM> --artifact <NAME|ID>` copies an experiment
  artifact into a ready model version. Artifact ids take precedence over names;
  an ambiguous name requires the id. `--alias <ALIAS>` points an alias at the
  new version. `--metadata <JSON>`, `--auto-create <true|false>`, and
  `--description <TEXT>` have the same meaning as for `push`.
- `alias list <MODEL>` lists aliases and their versions. `alias set <MODEL>
  <ALIAS> <VERSION>` creates or moves an alias to a ready version; `--expect
  <VERSION>` requires that the alias currently points at that version.
  Version numbers must be at least 1. `alias remove <MODEL> <ALIAS>` removes it.

JSON data is the server response: the model list (`items` and `total`), a
model, a version, the version list, the alias list, or an alias. `push` and `promote`
return the new version, including its state, source, manifest, metadata, and
aliases. `pull` data contains `model`, the resolved version object as `version`,
`directory`, `files` (each with `rel_path`, `path`, and downloaded `bytes`),
and total `bytes`. `alias remove` returns `model`, `alias`, and `removed: true`.

### `tracel artifacts`

Browse and download artifacts using positive project-scoped experiment numbers.

```bash
tracel artifacts list 42 --name weights --json
tracel artifacts download 42 weights --directory ./weights --force --json
```

`list <EXPERIMENT>` shows name, kind, id, manifest file count, and creation time.
`--name <NAME>` keeps artifacts whose name contains `NAME`. `download
<EXPERIMENT> <NAME|ID>` selects an artifact by id first, otherwise by an exact,
unique name. Ambiguous names require an id.
The destination defaults to `./<artifact name>` when the name is a single normal
path component; otherwise pass `-d, --directory <DIR>`. `--force` overwrites
existing files. Declared manifest sizes and SHA-256 checksums are verified.

JSON data for `list` is the server response (`items` and `total`). `download`
data contains `experiment`, the server's artifact object as `artifact`,
`directory`, `files` (each with `rel_path`, `path`, and downloaded `bytes`),
and total `bytes`.

### `tracel datasets`

Browse datasets in the selected project.

```bash
tracel datasets list --page 0 --per-page 25 --json
tracel datasets get images
tracel datasets versions images --page 0 --per-page 10 --json
```

`list` shows name, description, and id, followed by the number shown and total
when more datasets exist. `get <DATASET>` shows details and pretty metadata.
`versions <DATASET>` lists version, item count, source, and creation time.
`list` and `versions` accept zero-based `--page <N>` and positive
`--per-page <N>`; omitted values use the server's defaults.

JSON data is the server response. The list responses contain `items` and
`total_count`; dataset objects contain `id`, `name`, `description`, and
`metadata`.

### `tracel unlink`

Unlink the current directory from Tracel project. `--yes` skips the confirmation.

```bash
tracel unlink
tracel unlink --yes
```

### `tracel me`

Display information about the currently authenticated user.

```bash
tracel me
```

### `tracel project`

Display information about the current project.

```bash
tracel project
tracel project list --json
tracel project list alice
```

`list [NAMESPACE]` lists your own projects followed by each of your organizations'
projects. Supply your namespace or an organization namespace to limit the list.
It works without a linked project or `tracel.toml`. Human output shows
`namespace/name`, visibility, description, and creation time. JSON data is an
array of the server's project objects, including `namespace_name`.
Bare `project` returns `namespace`, `name`, `description`, `created_by`,
`visibility`, and project resolution `source`.

### `tracel experiments`

Browse and compare experiments in the selected project; `exp` is a visible alias.
`list` accepts zero-based `--page`, `--limit`, and repeated `--sort` values in the
server format: `field`, `field,asc`, or `field,desc`. `get <num|latest>` shows
details and config. Numbers are project-scoped experiment numbers.

```bash
tracel --project alice/demo experiments list --sort created_at,desc --limit 10 --json
tracel exp get latest
tracel exp metrics 42 --metric loss --max-points 100 --downsampling 1
tracel exp logs 42 --level info --level error --follow --json
tracel exp compare 41 42 --metric loss --metric accuracy --json
```

`metrics <num>` lists definitions without `--metric`; adding `--summary` returns
the named metric's summary. Series default to 100 maximum points and a
downsampling factor of 1. `logs <num>` reads one page (100 entries by default),
with `--level`, `--search`, `--from`, `--to`, `--offset`, `--after`, and metadata
filters (`--metadata key=value`, `--metadata-not key=value`, `--metadata-exists
key`). `--after` is a log sequence cursor and cannot be combined with time ranges
or offsets. `--follow` uses that cursor and polls every two seconds until the
experiment finishes, including any remaining pages.

`compare <BASE> <OTHER>` compares two different experiments. It lists where their
`config` and `attributes` differ, at dotted paths with array indices such as
`optimizer.betas[1]`. Keys that are empty or contain `.`, `[`, or `]` are written
as `["key"]`, and an empty path stands for the whole value. Objects and arrays are
compared member by member; any other change, including a change of type, is one
difference. It then pairs each metric's summary values (`optimal_value` from
`metrics --summary`) by group, for each `--metric <NAME>` or else every metric
either experiment defines. `delta` is `other - base`. The server does not report
whether a lower or higher value is better for a metric, so `compare` reports the
delta without calling it better or worse.

Apart from `compare`, JSON data is the server response, with `null` for an
unavailable metric series or summary. Following logs emits NDJSON instead of a
success envelope: each log item has `"type":"log"` added, followed by
`{"type":"end","running":false}`. Errors still use the standard error envelope.
Human output uses tables, experiment key/value lines, or one timestamp, level, and
message line per log entry.

`compare` data is `{"base":41,"other":42,"config":[...],"attributes":[...],"metrics":[...]}`.
Each difference is `{"path":"optimizer.lr","base":0.01,"other":0.001}`, leaving
out a side where the path does not exist: `{"path":"seed","base":1}` means the
other experiment has no `seed`, while `"other":null` would be a JSON `null`. Each
metric entry is `{"metric":"loss","group":"valid","base":0.5,"other":0.375,"delta":-0.125}`;
`base` or `other` is `null` when that experiment has no summary for the group, and
`delta` is then `null` too. Human output is a header line, config and attribute
difference tables (`PATH`, `BASE`, `OTHER`, with `-` for a missing side and long
values shortened to fit a terminal), and a metric table (`METRIC`, `GROUP`,
`BASE`, `OTHER`, `DELTA`), or a line such as `No config differences.` for an
empty section.

### `tracel jobs`

Browse, follow and cancel jobs in the selected project. Jobs are selected by
positive project-scoped job numbers.

```bash
tracel jobs list
tracel jobs get 12 --json
tracel jobs logs 12 --start 204800
tracel jobs logs 12 --follow --json
tracel jobs cancel 12 --yes
tracel jobs wait 12 --timeout 3600 --interval 10
```

- `list` shows number, status, command, and creation, start, and completion times,
  newest first. On a terminal, long commands are shortened to fit.
- `get <NUM>` shows status, status message, command, code version, compute
  provider, cost, and times.
- `logs <NUM>` reads one page of the job's log file, up to the server's page size.
  `--start <BYTE>` reads from a byte offset; each page's `end` is the next
  `start`. `--follow` reads from that offset, polls every two seconds until the
  job is completed, failed, or cancelled, then reads the remaining logs.
- `cancel <NUM>` cancels a new, queued, or running job and asks for confirmation
  first; `-y, --yes` skips it. Without prompts, `--yes` is required, otherwise
  the command fails with `CONFIRMATION_REQUIRED` (exit code 7). A running job
  moves to `pending_cancellation` until its compute provider stops it. Cancelling
  a job in any other status fails with `CONFLICT`.
- `wait <NUM>` checks the job's status every `--interval <SECONDS>` (default 5)
  until it is completed, failed, or cancelled. A completed job exits 0. A failed
  or cancelled job fails with `JOB_FAILED` (exit code 9). `--timeout <SECONDS>`
  stops waiting after that many seconds with `TIMEOUT` (exit code 10); there is no
  limit by default, and the job keeps running either way.

JSON data is the server response: the job list (`jobs`), a job, or a log page
(`logs`, `start`, `end`, `total_size`, and `has_more`). `cancel` and a completed
`wait` return the job as read afterwards. Job statuses are `new`, `queued`,
`running`, `pending_cancellation`, `completed`, `failed`, and `cancelled`.
Following logs emits NDJSON instead of a success envelope: each non-empty log page
has `"type":"log"` added, followed by `{"type":"end","status":"<status>"}`.
Errors still use the standard error envelope. Human output uses a table, job
key/value lines, or the raw log text; a page with more logs after it ends with
the `--start` of the next page.

## Project Structure

The Tracel CLI is organized as a Cargo workspace:

```text
tracel-cli/
├── crates/
│   └── tracel-cli/ 
└── xtask/                       # Build utilities
```

## Development

### Running from source

```bash
cargo run --bin tracel-- --help
```

### Running tests

```bash
cargo test
```

### Development mode

For testing against a local Console instance:

```bash
tracel --dev login
```

This connects to `http://localhost:9001` and uses separate development credentials.

## Contribution

Contributions are welcome! Please feel free to:

- Report issues or bugs
- Request new features
- Submit pull requests
- Improve documentation

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

## Links

- [Console Platform](https://console.tracel.ai/)
- [Tracel SDK](https://github.com/tracel-ai/tracel)
- [Burn Framework](https://github.com/tracel-ai/burn)
