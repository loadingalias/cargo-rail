# Unify Example

Use `unify` to keep workspace dependencies consistent.

## Quick Start

```bash
cargo rail init
cargo rail unify --check
cargo rail unify --check --explain
cargo rail unify
```

## Common Options

```toml
[unify]
detect_undeclared_features = true
fix_undeclared_features = true
prune_dead_features = true
detect_unused = true
msrv = true
pin_transitives = false
```

## Reference

- [Configuration Reference](../../docs/config.md)
- [Migration from cargo-hakari](../../docs/migrate-hakari.md)
