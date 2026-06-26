# Burn Central Workspace

Core library for the Burn Central CLI — project management and packaging.

## Overview

`burn-central-workspace` is the engine behind the `burn` CLI. It is not intended as a general-purpose library, but its modules can be used directly if you need to integrate project management or packaging into another tool.

## Architecture

```text
burn-central-workspace
├── entity/         – ProjectContext, BurnCentralProject, burn directory management
├── execution/      – Cancellation utilities for long-running operations
├── tools/          – Cargo utilities, Git helpers, project checks
├── event/          – Reporter trait for streaming progress events
└── logging/        – Log setup utilities
```

## Main Types

### `ProjectContext`

Discovers and represents a Burn Central project in a local Cargo workspace.

```rust
use std::path::Path;
use burn_central_workspace::ProjectContext;

let manifest_path = Path::new("Cargo.toml");
let project = ProjectContext::load(manifest_path, ".burn")?;
println!("Workspace: {}", project.get_workspace_name());
```

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
