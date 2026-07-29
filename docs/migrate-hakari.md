# Migrate from cargo-hakari

cargo-hakari stabilizes feature unification by generating a `workspace-hack` crate that every member depends on.
`cargo rail unify` achieves the same effect with explicit transitive pins in the workspace root or a selected member:
no generated crate to maintain, no extra dependency edge in every manifest, and the resolved graph remains
inspectable through normal Cargo manifests.

The goal is not to reproduce the workspace-hack topology in another format. Remove that topology, then let Cargo-Rail keep
only the dependency and feature edges the resolved workspace requires.

## Migration

1. Create a branch and record the current `cargo tree` output and build timing.
2. Remove workspace-hack dependencies, the hack crate's workspace member entry, and `hakari.toml`.
3. Enable transitive pinning in `rail.toml`.
4. Run `cargo rail unify --check --explain` and review each planned pin.
5. Apply with `cargo rail unify --backup`.
6. Run the workspace build and test suite, then compare `cargo tree --duplicates` with the baseline.

## Config

```toml
[unify]
transitive_pinning = { host = "root" }
```

## Commands

```bash
cargo rail init
cargo rail unify --check --explain
cargo rail unify --backup
```

## Mapping

| cargo-hakari | Cargo-Rail |
|---|---|
| workspace-hack crate | no extra crate |
| `hakari.toml` | `rail.toml` |
| `cargo hakari generate` | `cargo rail unify` |

## Rollback

```bash
cargo rail unify undo
```

`unify undo` restores Cargo-Rail's latest manifest backup. Use version control to restore the removed workspace-hack files.
