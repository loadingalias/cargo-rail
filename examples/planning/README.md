# Planning and Execution Examples

Choose the execution owner:

- [`with-task-runner/`](with-task-runner/) if your repo already has `just`, `make`, `xtask`, or scripts
- [`standalone/`](standalone/) if you want Cargo-Rail to plan and execute directly

## Quick start

```bash
cargo rail plan --merge-base --explain
cargo rail run --merge-base --action test
```

`scope` contains the Cargo package selection. `impact` explains how changed crates and dependents produced that selection.

## Config shape

```toml
[change-detection]
confidence_profile = "balanced"

[change-detection.custom]
benchmarks = ["benches/**", "perf/**"]
```

## Validate

```bash
cargo rail config validate --strict
cargo rail plan --merge-base --explain
```

## See also

- [Configuration Reference](../../docs/config.md)
- [Planning and Execution Guide](../../docs/planning.md)
