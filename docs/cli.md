# Tracel CLI reference

This page states the rules the commands share. `tracel <command> --help` lists
each command's flags.

## Output

A command writes its result on stdout and everything else on stderr: progress,
questions, warnings and errors. The format is the first of these that is set:

1. `--json`, which can't be combined with `--output`;
2. `-o, --output <auto|human|json>`;
3. `TRACEL_OUTPUT=human` or `TRACEL_OUTPUT=json`;
4. `auto`: human on a terminal, and JSON when stdout goes to a file or another
   program.

`auth token` and `run -- <args>` are plain text under `auto` too: `auth token`
prints the bare token, and `run -- <args>` writes what the program it runs
writes. Help and
version output keep clap's own format.

On a terminal, a command that reads shows its result in a frame titled with a
line that sums it up, the project and the environment when it isn't
production. A command that changes something says what it does and where,
shows its steps and questions, and ends on a line saying what it did. Times are
in your time zone, and tables shorten descriptions to fit the terminal. A
failure is its message, then a hint when one names what to do.

Human output that goes to a file or another program is plain text: no colors,
nothing shortened, and the summary line last. Commands that change something
write nothing on stdout in human output.

## JSON

A command writes one line of JSON on stdout. On success:

```json
{"ok":true,"data":{...}}
```

On failure:

```json
{"ok":false,"error":{"code":"NOT_FOUND","message":"No model 'weights' in alice/demo.","hint":"List models with `tracel models list`.","exit_code":5}}
```

`hint` is null when there is none. With JSON output, stderr carries only errors
and the instructions a login asks you to follow.

`tracel experiments logs --follow --json` writes one line of JSON per event
instead: each log entry, with `"type":"log"` added, then
`{"type":"end","running":false}` when the experiment has finished.
`tracel jobs logs --follow --json` does the same with each page of logs, then
`{"type":"end","status":"<status>"}`, and `tracel run --remote --follow --json`
starts with the queued job, with `"type":"queued"` added, then writes the job's
log events. An error still writes the failure line.

The data of commands that read is the Tracel Console's own response, with
`null` for a metric series or summary that doesn't exist. The other commands
return:

| Command | Data |
| --- | --- |
| `login`, `login --complete` | `username`, `environment` |
| `login --no-wait` | `status` (`"pending"`), `verification_uri_complete`, `user_code`, `expires_at` |
| `logout` | `logged_out` (false when you weren't logged in) |
| `auth status` | `credential` (`"api_key"` or `"login"`), `login_ends_at`, `user` (`username`, `namespace`), `environment` |
| `auth token` | `access_token` |
| `me` | `username`, `email`, `namespace`, `environment`, `api_url` |
| `init` | `namespace`, `name`, `created`, `url` (null when the workspace was already linked) |
| `unlink` | `unlinked`, `namespace`, `name` |
| `project` | `namespace`, `name`, `description`, `created_by`, `visibility`, `source` (`"flag"`, `"env"` or `"tracel.toml"`) |
| `package` | `namespace`, `project`, `digest`, `version_id`, `mode`, `targets`, `uploaded` (false when the code version already existed) |
| `models push`, `models promote` | the new version |
| `models pull` | `model`, `version`, `directory`, `files` (each `rel_path`, `path`, `bytes`), `bytes` |
| `models alias set` | the alias |
| `models alias remove` | `model`, `alias`, `removed` |
| `artifacts download` | `experiment`, `artifact`, `directory`, `files`, `bytes` |
| `experiments compare` | `base`, `other`, `config` and `attributes` (each difference `path`, `base`, `other`), `metrics` (each `metric`, `group`, `base`, `other`, `delta`) |
| `jobs cancel`, `jobs wait` | the job, as read afterwards |
| `run --list` | `protocol`, `sdk_version`, `runner`, `jobs` (each `name`, `description`, `input_schema`, `input_example`) |
| `run <job>` | `job`, `input`, `target` (`"console"` or `"offline"`), `exit_code`, `report` (null when the program wrote none) |
| `run --remote` | `job_num` (null when the Tracel Console doesn't return it), `job_id`, `group`, `digest`, `version_id`, `command`, `uploaded` |
| `run --remote --dry-run` | `namespace`, `project`, `group`, `mode`, `digest`, `command` |

Times are RFC 3339 in UTC.

## Exit codes

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

`run -- <args>` exits with the code of the program it runs.

## Questions

A command asks a question only when stdin and stderr are terminals, the output
is human, and `CI` isn't set. `--no-input` turns questions off. A command that
needs an answer it can't ask fails with `USAGE`, or `CONFIRMATION_REQUIRED` for
a yes or no, and a hint naming the flag that answers it. Declining what a
command asks to do, such as linking an existing project, ends it without
changing anything, with exit code 0.

## The project

A command that works on a project uses the first of these:

1. `--project <namespace>/<name>`;
2. `TRACEL_NAMESPACE` and `TRACEL_PROJECT`, each of which can also override
   its half of `tracel.toml`;
3. `tracel.toml` at the root of the Cargo workspace, which `tracel init` writes.
   Its only keys are `namespace` and `project`.

With the flag or both variables, `project`, `experiments`, `jobs`, `models`,
`artifacts` and `datasets` work in any directory. `package` and `run` need
the Cargo workspace, and `init` and `unlink` change `tracel.toml` whatever
selects the project. `-C <dir>` runs a command as if it started in `<dir>`.

## Credentials

Commands use `TRACEL_API_KEY` when it is set, and your login otherwise.
`tracel auth token` always prints the login's access token, renewed when it
has expired.

`tracel login --no-wait` saves a pending login and exits, and a new one
replaces it. The pending login is kept in a file only you can read, beside the
login itself, and its device code is never printed. `tracel login --complete`
waits for the approval until the code expires, or for `--timeout <seconds>`.
After a timeout, which exits with `TIMEOUT`, the pending login is kept for the
next `--complete`; a denied or expired login has to start again.

## Packaging

`tracel package` uploads a code version, named by a digest of the workspace's
source: the SHA-256 of `path:sha256` lines sorted by path, one per file a source
package holds. Binary packages use the same digest and upload only the
binaries, so every package of the same source, in either mode and for any
target, belongs to one code version, and a binary replaces the one already
uploaded for its target. `uploaded` is false when the code version already
had what was packaged.

- `--mode binary` builds a release binary for each `--target <triple>`, the
  host when there is none and no question can be asked. `--bin <name>` picks
  the binary when the build makes several. On Linux, building for another
  architecture needs its linker, and building for another OS needs
  `cargo-zigbuild` or, for Windows, `cargo-xwin`.
- `--mode source` uploads the workspace's files, to be built where the code
  runs. It leaves out what `.gitignore`, `.ignore`, `.git/info/exclude` and
  git's global excludes file match, with or without a git repository, and
  always `.git` and `target`. Hidden files are packaged only in a git
  repository. Packaging no file fails with `USAGE`.
- `--install-targets` installs missing Rust targets with rustup without asking.

## Downloads

`models pull` downloads into `./<model>-v<version>`, and `artifacts download`
into `./<artifact>` when the artifact's name is a plain file name; `-d,
--directory <dir>` chooses another directory. Every path and existing file is
checked before anything downloads, and `--force` overwrites existing files.
Each file's size and SHA-256 checksum are checked before it is saved.

## Models

- A version is `latest`, a number such as `7` or `v7`, or an alias.
- `models push --auto-create true` creates a missing model, and `false` needs
  an existing one; without the flag, the command asks. `--description` sets a
  created model's description and needs `--auto-create true`. `--metadata` is
  a JSON object.
- `models promote --artifact` finds the artifact by id, then by a name only
  one artifact of the experiment has.
- `models alias set --expect <version>` moves the alias only if it points at
  that version.

## Experiments

- `experiments list` takes `--page` (from 0), `--limit`, and `--sort field`,
  `field,asc` or `field,desc`, which can repeat.
- `experiments metrics <number>` lists the metrics, and `--metric <name>` shows
  one's series, or its summary with `--summary`.
- `experiments logs <number>` reads one page of entries, 100 by default, and
  shows them oldest first. `--after <seq>` reads from a sequence number and
  can't be combined with `--from`, `--to` or `--offset`. `--follow` reads from
  there until the experiment finishes.
- `experiments compare <base> <other>` lists where the two experiments' config
  and attributes differ, at dotted paths with array indices such as
  `optimizer.betas[1]`; a key that is empty or holds `.`, `[` or `]` is written
  `["key"]`. A difference leaves out the side where the path doesn't exist.
  It then pairs each metric's summary value by group, for each `--metric`, or
  else every metric either experiment has, with `delta` being `other - base`.
  The Tracel Console doesn't say whether lower or higher is better, so neither
  does `compare`.

## Jobs

- A job's status is `new`, `queued`, `running`, `pending_cancellation`,
  `completed`, `failed` or `cancelled`.
- `jobs logs <number>` reads one page of the job's log file. `--start <byte>`
  reads from a byte offset, and a page's `end` is the next page's `start`.
  `--follow` reads until the job has finished.
- `jobs cancel <number>` cancels a new, queued or running job, after asking;
  `--yes` skips the question. A running job stays `pending_cancellation` until
  its compute provider stops it.
- `jobs wait <number>` checks the job every `--interval` seconds until it
  finishes. It fails with `JOB_FAILED` when the job fails or is cancelled, and
  with `TIMEOUT` when `--timeout` seconds pass first.

## Running jobs

- Jobs come from the binary of a workspace package that depends on the
  `tracel` crate. `--bin <name>` picks one when there are several, unless the
  package's `default-run` names it. The binary has to run its jobs with
  `tracel::app::cli::Cli`.
- To list its jobs, the binary is built and run with `TRACEL_DESCRIBE=<path>`,
  and has 10 seconds to write their definitions there. They're cached in
  `target/tracel/jobs.json` until the binary changes.
- `run <job>` starts from the job's example input, or from the input of the
  experiment `--like <number|latest>` names. Each `-c <file>` is merged onto it
  as a JSON merge patch (RFC 7386), then the job's flags set their fields, then
  each `--set <path>=<value>` sets one value at a dotted path with array
  indices, such as `layers[0].size`. A value that isn't JSON is a string. An
  input that doesn't match the job's schema fails with `USAGE`.
- A job's flags are its binary's: one per field of its input, typed by the
  input schema or else the example input. A nested field's flag joins the keys
  with dots and writes `_` as `-`, as in `--optimizer.weight-decay`. A boolean
  flag without a value is `true`, and an array, a map or a `null` field takes
  JSON. A field whose flag has the name of one of `run`'s own, such as
  `offline`, is set with `--set`.
- The job runs as `<binary> <job> <input>`, with stdin closed. With JSON
  output, its stdout goes to stderr.
- The run is recorded on the Tracel Console when a project is linked and a
  credential is available, and on this machine otherwise or with `--offline`.
  The program gets `TRACEL_TARGET` (`console` or `offline`), `TRACEL_ENV`,
  `TRACEL_NAMESPACE` and `TRACEL_PROJECT`, and `TRACEL_REPORT_FILE`, where it
  writes how the job is going and the experiment it recorded, if any.
- A job is stopped when `tracel` asked it to stop, or when the program exits
  with code 130. A stopped job fails with `JOB_FAILED`, as does a job whose
  report says it failed. Without a final report, exit code 0 means completed,
  2 a rejected input, and any other code or a signal failed.
- Ctrl-C, SIGTERM or SIGHUP stops the job: the program gets SIGTERM, and is
  killed when it is still running 30 seconds later.

## Remote runs

- `run <job> --remote <group>` packages the workspace with the flags of
  `package`, then queues the code version as a job on the compute provider
  group, with the resolved input. Without a job, the job command is the
  arguments after `--`. They reach the program unchanged: the job command
  single-quotes each argument that is empty, starts with `#`, or contains a
  space, tab, newline, quote or backslash.
- A job may incur costs, so the command asks before uploading anything, and
  `--yes` skips the question. Without a terminal it needs `--yes`, and fails
  with `CONFIRMATION_REQUIRED` before packaging.
- `--dry-run` packages and shows the job without logging in, uploading or
  queuing.
- `--follow` follows the job's logs, then exits as `jobs wait` does. Without a
  job number from the Tracel Console, it fails after queuing, and the job stays
  queued.
