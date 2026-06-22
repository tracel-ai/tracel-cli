<div align="center">

<h1>Burn Central CLI</h1>

[![Current Crates.io Version](https://img.shields.io/crates/v/burn-central-cli)](https://crates.io/crates/burn-central-cli)
[![Minimum Supported Rust Version](https://img.shields.io/crates/msrv/burn-central-cli)](https://crates.io/crates/burn-central-cli)
[![Test Status](https://github.com/tracel-ai/burn-central-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/tracel-ai/burn-central-cli/actions/workflows/ci.yml)
![license](https://shields.io/badge/license-MIT%2FApache--2.0-blue)

---

</div>

## Description

The Burn Central CLI (`burn`) is the command-line tool for interacting with [Burn Central](https://s1-console.tracel.ai/), the centralized platform for experiment tracking, model sharing, and deployment for [Burn](https://github.com/tracel-ai/burn) users.

This CLI works in conjunction with the [Burn Central SDK](https://github.com/tracel-ai/burn-central) to provide a seamless workflow for:

- Running training jobs locally or remotely
- Managing experiments and tracking metrics
- Packaging and deploying models
- Integrating with compute providers
- Managing project configurations

## Installation

### Install from crates.io

```bash
cargo install burn-central-cli
```

### Build from source

```bash
git clone https://github.com/tracel-ai/burn-central-cli.git
cd burn-central-cli
cargo install --path crates/burn-central-cli
```

After installation, the `burn` command will be available in your terminal.

## Prerequisites

1. **Burn Central Account**: Create an account at [console.tracel.ai](https://central.burn.dev/)
2. **Rust**: Version 1.87.0 or higher
3. **Burn Central SDK**: Add the SDK to your Burn project (see [Quick Start](#quick-start))

## Quick Start

### 1. Add the Burn Central SDK to your project

Add the SDK to your `Cargo.toml`:

```toml
[dependencies]
burn-central = "0.1.0"
```

### 2. Register your training function

Use the `#[register]` macro to make your training function discoverable:

```rust
use burn_central::{
    experiment::ExperimentRun,
    macros::register,
    runtime::{Args, ArtifactLoader, Model},
};

#[register(training, name = "mnist")]
pub fn training(
    client: &ExperimentRun,
    config: Args<YourExperimentConfig>,
    loader: ArtifactLoader<ModelArtifact>,
) -> Result<Model<ModelArtifact>, String> {
    // Your training logic here...
    Ok(Model(model_artifact))
}
```

See the [SDK documentation](https://github.com/tracel-ai/burn-central) for complete integration details.

### 3. Initialize your project

Navigate to your Burn project directory and run:

```bash
burn init
```

This will:

- Link your local project to Burn Central
- Create or select a project on the platform
- Configure your local environment

### 4. Login to Burn Central

```bash
burn login
```

This opens your browser to authenticate with Burn Central and stores your credentials locally.

### 5. Run your training

```bash
burn train -- <args>
```

`burn train` is a thin alias for `cargo run`. Everything after `--` is forwarded
to your project's binary, which uses the SDK to send metrics, logs, and
checkpoints to Burn Central in real-time.

## Commands

### `burn train`

Run your project locally. This is a thin alias for `cargo run`: every argument
after `--` is forwarded to your binary, so `burn train -- <args>` is equivalent
to `cargo run -- <args>`. stdin/stdout/stderr are inherited and the binary's
exit code is propagated.

```bash
# Equivalent to `cargo run`
burn train

# Equivalent to `cargo run -- train mnist --epochs 100`
burn train -- train mnist --epochs 100
```

### `burn package`

Package your project for deployment on remote compute providers.

```bash
burn package
```

This creates a deployable artifact containing your code, dependencies, and configurations.

### `burn login`

Authenticate with the Burn Central platform.

```bash
burn login
```

### `burn init`

Initialize or reinitialize a Burn Central project in the current directory.

```bash
# Interactive initialization
burn init
```

### `burn unlink`

Unlink the current directory from Burn Central.

```bash
burn unlink
```

### `burn me`

Display information about the currently authenticated user.

```bash
burn me
```

### `burn project`

Display information about the current project.

```bash
burn project
```

## Project Structure

The Burn Central CLI is organized as a Cargo workspace:

```text
burn-central-cli/
├── crates/
│   ├── burn-central-cli/       # Main CLI binary
│   └── burn-central-workspace/ # Core library for project management
└── xtask/                       # Build utilities
```

### `burn-central-workspace`

The `burn-central-workspace` crate is a standalone library that provides:

- Project discovery and management
- Project packaging and upload
- Client integration with Burn Central

This library can be used independently in other applications. See the [workspace README](crates/burn-central-workspace/README.md) for detailed documentation.

## How It Works

1. **Execution**: `burn train` runs your project via `cargo run`
2. **Tracking**: The SDK sends metrics, logs, and checkpoints to Burn Central during execution
3. **Management**: Provides tools to manage projects, experiments, and deployments

## Integration with the Burn Central SDK

The CLI works seamlessly with the Burn Central SDK. Here's how they connect:

1. **SDK Integration**: Add the SDK to your project and use the `#[register]` macro
2. **Execution**: `burn train` runs your project via `cargo run`
3. **Tracking**: The SDK sends data to Burn Central during execution

For detailed SDK usage, see the [SDK README](https://github.com/tracel-ai/burn-central).

## Development

### Running from source

```bash
cargo run --bin burn -- --help
```

### Running tests

```bash
cargo test
```

### Development mode

For testing against a local Burn Central instance:

```bash
burn --dev login
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

- [Burn Central Platform](https://console.tracel.ai/)
- [Burn Central SDK](https://github.com/tracel-ai/burn-central)
- [Burn Framework](https://github.com/tracel-ai/burn)
