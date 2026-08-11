# Planner Scope Examples

Choose the direct consumer:

- [`with-task-runner/`](with-task-runner/) for `just`, `make`, `xtask`, or scripts
- [`standalone/`](standalone/) for Cargo and cargo-nextest

## Quick start

```bash
cargo rail plan --merge-base --explain
cargo nextest run --workspace
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
- [Planning Guide](../../docs/planning.md)
