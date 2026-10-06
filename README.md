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
`USAGE` with a hint naming the required flag.

Select a project with global `--project <namespace>/<name>`. It takes precedence
over `TRACEL_NAMESPACE` and `TRACEL_PROJECT`, which each independently fall back
to `tracel.toml` at the Cargo workspace root. `project` and `model upload` work
from any directory with a flag or both variables; `package` still requires a
Cargo workspace. Global `-C <dir>` runs as if started in that directory. `init`
and `unlink` operate on `tracel.toml` and ignore project overrides.

```bash
tracel --project alice/demo project --json
TRACEL_NAMESPACE=alice TRACEL_PROJECT=demo tracel -C ./trainer project --json
```

JSON results are one line on stdout: `{"ok":true,"data":{...}}` on success, or
`{"ok":false,"error":{"code":"NOT_FOUND","message":"...","hint":null,"exit_code":5}}`
on failure. Diagnostics go to stderr. Help and version output retain their normal
format, and `train` inherits the executed program's output and exit code.

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
| `UNAVAILABLE` | 11 |

Exit codes 9 and 10 are reserved.

### `tracel train`

Run your project locally. This is a thin alias for `cargo run`: every argument
after `--` is forwarded to your binary, so `tracel train -- <args>` is equivalent
to `cargo run -- <args>`. stdin/stdout/stderr are inherited and the binary's
exit code is propagated.

```bash
# Equivalent to `cargo run`
tracel train

# Equivalent to `cargo run -- train mnist --epochs 100`
tracel train -- train mnist --epochs 100
```

### `tracel package`

Package your project for deployment on remote compute providers.

```bash
tracel package
# Source packaging without prompts
tracel package --mode source --allow-dirty --json
# Build selected binaries and install missing targets without prompts
tracel package --mode binary --target x86_64-unknown-linux-gnu --bin trainer --install-targets --commit --json
```

This creates a deployable artifact containing your code, dependencies, and configurations.

`--mode <binary|source>` is required without prompts. In binary mode, repeat
`--target <triple>` to choose targets; without prompts, omitting it builds for the
host. Use `--bin <name>` when several binaries are built. `--install-targets`
installs missing Rust targets without asking. `--commit` commits all current
changes before packaging; `--allow-dirty` continues with uncommitted changes.
These two flags cannot be combined. The code version digest remains the current
commit hash. JSON data contains `namespace`, `project`, `digest`, `version_id`,
`mode`, `targets`, and `uploaded` (false when the commit was already packaged).

### `tracel login`

Authenticate with the Console platform. The CLI prints a link and a code to
approve in your browser, and the login then lasts seven days.

```bash
tracel login
```

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
tracel init --owner my-namespace --name my-project --description "" --yes --allow-dirty --json
```

`--owner <namespace>` must name your own namespace or one of your organizations.
`--name <project>` accepts alphanumeric characters, underscores, and hyphens.
`--description <text>` supplies the new project's description; without a terminal
it defaults to empty. These flags replace the corresponding prompts. `--yes`
links an existing project without asking. `--commit` commits all current changes,
including the first commit if needed; `--allow-dirty` continues without committing
when a commit already exists. These two flags cannot be combined. A dirty
repository needs one of them without prompts. `--force` reinitializes an already
linked project; without it, asking for a different project fails with `CONFLICT`.
JSON data contains `namespace`, `name`, `created`, and `url`; `created` is false
when an existing project is linked, and `url` is null when nothing changed.

### `tracel model upload`

Upload a directory as a new version of a model.

```bash
tracel --project alice/demo model upload my-model --directory ./weights --auto-create true --description "Model weights" --json
```

Global `--project <namespace>/<name>` selects the destination. `--auto-create true`
creates a missing model without asking; without prompts, a missing model needs
this flag. `--auto-create false` requires an existing model. `--description <text>`
sets the new model's description and requires `--auto-create true`. JSON data
contains `namespace`, `project`, `model`, `version`, `files`, and `bytes`.

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
```

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
