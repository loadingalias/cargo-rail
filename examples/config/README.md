# Config Example

A complete, documented `rail.toml` configuration file.

## Usage

```bash
# Generate config for your workspace
cargo rail init

# Or copy this example
cp examples/config/rail.toml .config/rail.toml
```

## Sections

| Section | Purpose |
|---------|---------|
| `targets` | Platform targets for multi-target validation |
| `[unify]` | Dependency unification settings |
| `[change-detection]` | Change detection rules and confidence profiles |
| `[run]` | Execution profiles for `cargo rail run` |
| `[release]` | Release automation settings |
| `[crates.<name>.*]` | Per-crate overrides |

## Validate

```bash
cargo rail config validate --strict
```

## Reference

Full documentation: [docs/config.md](../../docs/config.md)
