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

`auth token` and `run` are plain text under `auto` too: `auth token` prints
the bare token, and `run` writes what the program it runs writes. Help and
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
`{"type":"end","status":"<status>"}`. An error still writes the failure line.

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
| `jobs cancel`, `jobs wait` | the job, as read afterwards |

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

`run` exits with the code of the program it runs.

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
3. `tracel.toml` at the root of the Cargo workspace, which `tracel init` writes
   with the keys `namespace` and `project`.

With the flag or both variables, `project`, `experiments`, `models`,
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

`tracel package` uploads a code version, which the current commit names:

- `--mode binary` builds a release binary for each `--target <triple>`, the
  host when there is none and no question can be asked. `--bin <name>` picks
  the binary when the build makes several. On Linux, building for another
  architecture needs its linker, and building for another OS needs
  `cargo-zigbuild` or, for Windows, `cargo-xwin`.
- `--mode source` uploads the workspace's files, without what git ignores, to
  be built where the code runs.
- `--install-targets` installs missing Rust targets with rustup without asking.
- `--commit` commits all changes first, and `--allow-dirty` packages without
  them. Uncommitted changes are never part of the code version.

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
