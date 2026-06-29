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
```

This creates a deployable artifact containing your code, dependencies, and configurations.

### `tracel login`

Authenticate with the Console platform.

```bash
tracel login
```

### `tracel init`

Initialize or reinitialize a Tracel project in the current directory.

```bash
# Interactive initialization
tracel init
```

### `tracel unlink`

Unlink the current directory from Tracel project.

```bash
tracel unlink
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
