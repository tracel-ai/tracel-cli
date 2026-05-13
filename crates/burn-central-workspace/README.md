# Burn Central Workspace

Core library for the Burn Central CLI — project management, function discovery, code generation, and local execution.

## Overview

`burn-central-workspace` is the engine behind the `burn` CLI. It is not intended as a general-purpose library, but its modules can be used directly if you need to integrate project management or local execution into another tool.

## Architecture

```text
burn-central-workspace
├── entity/         – ProjectContext, BurnCentralProject, burn directory management
├── execution/      – Local build-and-run pipeline (LocalExecutor, LocalExecutionConfig)
├── generation/     – Generates the executable wrapper crate from discovered functions
├── tools/          – Cargo utilities, Git helpers, function discovery
├── compute_provider/ – Runtime integration for remote compute providers
├── event/          – Reporter trait for streaming execution events
└── logging/        – Log setup utilities
```

## Main Types

### `ProjectContext`

Discovers and represents a Burn Central project in a local Cargo workspace.

```rust
use burn_central_workspace::ProjectContext;

let project = ProjectContext::discover()?;
let registry = project.load_functions()?;
println!("Found {} functions", registry.num_functions());
```

### `LocalExecutor`

Builds and runs a `#[register]`-tagged function locally. The pipeline is: discover functions → generate wrapper crate → `cargo build` → run binary.

```rust
use burn_central_workspace::{
    ProjectContext,
    execution::local::{LocalExecutor, LocalExecutionConfig},
    execution::{ProcedureType, BuildProfile},
};

let project = ProjectContext::discover()?;
let executor = LocalExecutor::new(&project);

let config = LocalExecutionConfig::new(
    api_key,
    env,
    None,           // package — resolved automatically if None
    "my_training".to_string(),
    ProcedureType::Training,
    code_version,
)
.with_build_profile(BuildProfile::Release);

let result = executor.execute(config, None)?;
```

### `FunctionDiscovery`

Finds functions annotated with `#[register(training)]` or `#[register(inference)]` by running `cargo rustc -Zunpretty=expanded` and parsing the emitted `BCFN1|...|END` markers.

```rust
use burn_central_workspace::tools::function_discovery::{FunctionDiscovery, DiscoveryConfig};

let discovery = FunctionDiscovery::new(&project_root);
let result = discovery.discover_functions(&config, &cancel_token, None)?;
```

### Compute Provider

For remote execution contexts, the `compute_provider` module exposes the `TrainingJobArgs` struct used to dispatch jobs.

## Function Marker Format

The `#[register]` macro emits a string constant in the expanded source:

```text
BCFN1|<mod_path>|<fn_name>|<builder_fn>|<routine_name>|<proc_type>|END
```

The function's AST is also emitted as a `_BURN_FUNCTION_AST_<NAME>` byte constant for inspection by the CLI.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
