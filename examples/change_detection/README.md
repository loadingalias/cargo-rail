# Change Detection Examples

Choose one pattern:

- [`with-task-runner/`](with-task-runner/) if your repo already has `just`, `make`, `xtask`, or scripts
- [`standalone/`](standalone/) if you want `cargo-rail` to plan and execute directly

## Quick Start

```bash
cargo rail plan --merge-base --explain
cargo rail run --merge-base --surface test
```

Use `scope` for execution. Use `impact` for explanation.

## Config Shape

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

## See Also

- [Configuration Reference](../../docs/config.md)
- [Change Detection Guide](../../docs/change-detection.md)
