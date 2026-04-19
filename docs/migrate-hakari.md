# Migrate from cargo-hakari

`cargo-rail unify` replaces the workspace-hack flow with one config file and one command.

## Minimal Migration

1. remove the workspace-hack crate
2. remove `hakari.toml`
3. enable transitive pinning in `rail.toml`
4. run `cargo rail unify --check`
5. apply with `cargo rail unify`

## Config

```toml
[unify]
pin_transitives = true
```

## Commands

```bash
cargo rail init
cargo rail unify --check
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
