# Affected CI

Choose the direct consumer:

- [`with-task-runner/`](with-task-runner/) for `just`, `make`, `xtask`, or scripts
- [`standalone/`](standalone/) for Cargo and cargo-nextest

Inspect scope without running work:

```bash
cargo rail plan --explain
```

Each required Cargo work decision contains its own typed package and target selection.

Validate both policy files from the repository root:

```bash
cargo rail --config examples/planning/standalone/rail.toml config validate --strict
cargo rail --config examples/planning/with-task-runner/rail.toml config validate --strict
```

## See also

- [Configuration reference](../../docs/config.md#planworkname)
- [Planning guide](../../docs/planning.md)
