# Unify Example

`unify` plans manifest changes from the resolved dependency graph. It can centralize shared declarations, remove unused edges, prune dead features, repair borrowed features, derive MSRV, and pin fragmented transitive features.

## Quick Start

```bash
cargo rail init
cargo rail config migrate --check
cargo rail unify --check
cargo rail unify --check --explain
cargo rail unify
```

## Common Options

```toml
[unify]
consumer_scope = "open"
msrv_policy = { mode = "compute", source = "workspace" }
```

Omit `transitive_pinning` unless replacing a workspace-hack crate. Keep `consumer_scope = "open"` for libraries or
private packages with consumers outside the workspace; set it to `"workspace"` only when the workspace is the complete
consumer graph. Diagnostics are unconditional. `--check` does not mutate manifests, may update compiler-evidence cache
and reports under `target/cargo-rail/`, and exits 1 for pending edits; running without `--check` applies proven edits.
Review `--check --explain` output before apply; `cargo rail unify undo` restores the latest manifest backup.

Compiler evidence caching and deterministic manifest ordering are internal correctness mechanisms, not configuration.

## Reference

- [Configuration Reference](../../docs/config.md)
- [Migration from cargo-hakari](../../docs/migrate-hakari.md)
