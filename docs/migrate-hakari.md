# Migrate from cargo-hakari

cargo-hakari stabilizes feature unification by generating a `workspace-hack` crate that every member depends on.
`cargo rail unify` can plan explicit transitive pins in the workspace root or a selected member, but it does not detect,
remove, or prove equivalence with a generated workspace-hack crate. Treat this as a manual migration experiment until
your workspace build, test, and performance evidence establishes parity.

The goal is not to reproduce the workspace-hack topology in another format. Remove that topology, then let Cargo-Rail keep
only the dependency and feature edges the resolved workspace requires.

## Migration

1. Create a branch and record the current `cargo tree` output and build timing.
2. In a reviewable commit, manually remove workspace-hack dependencies, the hack crate's workspace member entry, and
   `hakari.toml`.
3. Enable transitive pinning in `rail.toml`.
4. Run `cargo rail unify --check --explain` and review each planned pin.
5. Apply with `cargo rail unify --backup`.
6. Run every supported feature/target build and test view, then compare `cargo tree --duplicates` and build timing with
   the baseline. Restore the migration commit if parity is not established.

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

## Rollback

```bash
cargo rail unify undo
```

`unify undo` restores Cargo-Rail's latest manifest and lockfile backup. Use version control to restore the removed
workspace-hack files.
