# Migrate from cargo-hakari

`cargo-rail unify` replaces the generated workspace-hack crate with explicit transitive pins in the root or a selected workspace member. The resolved graph remains inspectable through normal Cargo manifests.

## Migration

1. Create a branch and record the current `cargo tree` output and build timing.
2. Remove workspace-hack dependencies, the hack crate's workspace member entry, and `hakari.toml`.
3. Enable transitive pinning in `rail.toml`.
4. Run `cargo rail unify --check --explain` and review each planned pin.
5. Apply with `cargo rail unify`.
6. Run the workspace build and test suite, then compare `cargo tree --duplicates` with the baseline.

## Config

```toml
[unify]
pin_transitives = true
```

## Commands

```bash
cargo rail init
cargo rail unify --check --explain
cargo rail unify
```

## Mapping

| cargo-hakari | cargo-rail |
|---|---|
| workspace-hack crate | no extra crate |
| `hakari.toml` | `rail.toml` |
| `cargo hakari generate` | `cargo rail unify` |

## Rollback

```bash
cargo rail unify undo
```

`unify undo` restores cargo-rail's latest manifest backup. Use version control to restore the removed workspace-hack files.
