<div align="center">

<h1>Tracel CLI</h1>

[![Current Crates.io Version](https://img.shields.io/crates/v/tracel-cli)](https://crates.io/crates/tracel-cli)
[![Minimum Supported Rust Version](https://img.shields.io/crates/msrv/tracel-cli)](https://crates.io/crates/tracel-cli)
[![Test Status](https://github.com/tracel-ai/tracel-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/tracel-ai/tracel-cli/actions/workflows/ci.yml)
![license](https://shields.io/badge/license-MIT%2FApache--2.0-blue)

---

</div>

The Tracel CLI (`tracel`) works with the
[Tracel Console](https://console.tracel.ai/), where
[Burn](https://github.com/tracel-ai/burn) users track experiments, share models
and run training on compute providers. Together with the
[Tracel SDK](https://github.com/tracel-ai/tracel) in your project, it lets you:

- run your project locally and browse its experiments, metrics and logs;
- package your code for compute providers;
- push, pull and promote models, and download artifacts and datasets.

## Installation

```bash
cargo install tracel-cli
```

Or build it from source:

```bash
git clone https://github.com/tracel-ai/tracel-cli.git
cd tracel-cli
cargo install --path crates/tracel-cli
```

You need Rust 1.97 or later, an account on the
[Tracel Console](https://console.tracel.ai/), and the Tracel SDK in your Burn
project.

## Getting started

```bash
tracel login      # approve the login in your browser
tracel init       # link this workspace to a Console project
tracel run        # run it locally with `cargo run`
tracel experiments list
```

Every command takes `--help`. Commands print tables and messages on a terminal,
and JSON when their output goes to another program; `--json` asks for JSON
anywhere. `--no-input` makes a command fail instead of asking a question, and
`--project <namespace>/<name>` picks a project other than the one the workspace
is linked to. The [CLI reference](docs/cli.md) covers output formats, exit
codes, credentials and each command's JSON.

## Commands

### `tracel login`, `logout` and `auth`

`tracel login` shows a link and a code, and offers to open the link in your
browser. Approve the login there and the CLI is logged in for seven days.

```bash
tracel login
tracel logout
tracel auth status    # which credential commands use, and when the login ends
```

Scripts can log in in two steps, or use an API key from the Console instead:

```bash
tracel login --no-wait      # prints the link and the code to approve
tracel login --complete     # waits for the approval
TRACEL_API_KEY=... tracel me
curl -H "Authorization: Bearer $(tracel auth token)" \
  https://console.tracel.ai/api/v1/user/organizations
```

### `tracel me`

Show who you are logged in as.

```bash
tracel me
```

### `tracel init` and `unlink`

Link the workspace, the Cargo workspace you run it in, to a project on the
Tracel Console, creating the project if it doesn't exist. The link is saved in
`tracel.toml` at the workspace root.

```bash
tracel init
tracel init --owner my-org --name mnist --yes
tracel unlink
```

`--force` links a workspace that is already linked to another project.

### `tracel project`

Show the project the workspace is linked to, or list your projects and your
organizations' projects.

```bash
tracel project
tracel project list
tracel project list my-org
```

### `tracel run`

Run a job your program defines, on this machine or on a compute provider. A
job's input starts from its example, or from the input an experiment recorded
with `--like`; `-c` merges JSON files onto it, and the job's own flags or
`--set` change single values. `tracel run <job> --help` lists the job's flags. The
run is recorded on the Tracel Console when the workspace is linked, and on this
machine otherwise. With `--remote`, the workspace is packaged as `tracel
package` does and the job is queued on a compute provider group, once you
confirm, since a job may cost money. Without a job, `tracel run` is `cargo run`.

```bash
tracel run --list
tracel run train --help
tracel run train --epochs 5 --optimizer.lr 0.01
tracel run train --set epochs=5 --set optimizer.lr=0.01
tracel run train -c base.json --like 42
tracel run train --offline
tracel run train --remote gpu --mode source --yes --follow
tracel run -- --epochs 10
```

### `tracel package`

Upload a code version of the workspace, the code compute providers run: a
compiled binary, or the source to build there. Packaging the same source again
gives the same code version.

```bash
tracel package
tracel package --mode binary --target x86_64-unknown-linux-gnu --json
tracel package --mode source
```

### `tracel experiments`

Browse the project's experiments by their number, along with their metrics and
logs, and compare two of them. `exp` is short for `experiments`.

```bash
tracel experiments list --sort created_at,desc --limit 10
tracel exp get latest
tracel exp metrics 42 --metric loss
tracel exp logs 42 --level warn --level error
tracel exp logs 42 --follow
tracel exp compare 41 42 --metric loss
```

### `tracel models`

Browse the project's models and their versions, download and upload versions,
make an experiment's artifact a new version, and point aliases at versions.
A version is a number such as `7` or `v7`, an alias, or `latest`.

```bash
tracel models list
tracel models get my-model --version production
tracel models versions my-model --all
tracel models pull my-model --version v7 --directory ./weights
tracel models push my-model --directory ./weights --auto-create true
tracel models promote my-model --experiment 42 --artifact weights
tracel models alias set my-model production 7
```

### `tracel artifacts`

List the artifacts of an experiment, and download one by name or id.

```bash
tracel artifacts list 42
tracel artifacts download 42 weights --directory ./weights
```

### `tracel datasets`

Browse the project's datasets and their published versions.

```bash
tracel datasets list
tracel datasets get images
tracel datasets versions images
```

### `tracel jobs`

Browse the project's jobs on compute providers, read and follow their logs,
cancel them, and wait for them to finish.

```bash
tracel jobs list
tracel jobs get 12
tracel jobs logs 12 --follow
tracel jobs cancel 12
tracel jobs wait 12 --timeout 3600
```

## Development

```bash
cargo run --bin tracel -- --help
cargo test
```

The repository is a Cargo workspace: the CLI is in `crates/tracel-cli`, and
`xtask` holds the build tasks.

## Contributing

Issues and pull requests are welcome, whether they report a bug, ask for a
feature or improve the documentation.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

## Links

- [Tracel Console](https://console.tracel.ai/)
- [Tracel SDK](https://github.com/tracel-ai/tracel)
- [Burn](https://github.com/tracel-ai/burn)
