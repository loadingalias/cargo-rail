# Unify Example

`unify` plans manifest changes from the resolved dependency graph. It can centralize shared declarations, remove unused edges, prune dead features, repair borrowed features, derive MSRV, and pin fragmented transitive features.

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

Keep `pin_transitives = false` unless replacing a workspace-hack crate. Review `--check --explain` output before apply; `cargo rail unify undo` restores the latest manifest backup.

## Reference

- [Configuration Reference](../../docs/config.md)
- [Migration from cargo-hakari](../../docs/migrate-hakari.md)
