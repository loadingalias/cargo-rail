# Unify Example

`unify` plans manifest changes from the resolved dependency graph. It can centralize shared declarations, remove unused edges, prune dead features, repair borrowed features, derive MSRV, and pin fragmented transitive features.

## Quick Start

```bash
cargo rail init
cargo rail config sync
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
consumer_scope = "open"
detect_unused = true
compiler_diag_cache = true
msrv = true
pin_transitives = false
```

Keep `pin_transitives = false` unless replacing a workspace-hack crate. Keep `consumer_scope = "open"` for libraries or private packages with consumers outside the workspace; set it to `"workspace"` only when the workspace is the complete consumer graph. Review `--check --explain` output before apply; `cargo rail unify undo` restores the latest manifest backup.

The compiler-evidence cache binds the compiler, source tree, manifests, target, Cargo target kind, and feature selection. Repeated analysis reuses only compilation units whose full identity and Cargo freshness still match.

## Reference

- [Configuration Reference](../../docs/config.md)
- [Migration from cargo-hakari](../../docs/migrate-hakari.md)
