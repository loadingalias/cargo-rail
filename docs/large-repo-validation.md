# Large Repo Validation

This document records reproducible validation runs against three large Rust repositories.

## Repositories

- `tokio`
- `helix`
- `meilisearch`

## Why use the branch binary directly

To test this branch without touching your globally installed `cargo-rail`:

```bash
./target/debug/cargo-rail rail --help
```

## Command matrix used

For each repo (with repo-local `.config/rail.toml`):

```bash
cargo-rail rail config validate --strict
cargo-rail rail config sync --check
cargo-rail rail plan --merge-base -f json
cargo-rail rail run --merge-base --profile ci --dry-run --print-cmd
cargo-rail rail unify --check
cargo-rail rail split init <crate> --check
cargo-rail rail split run <crate> --check --allow-dirty
cargo-rail rail sync <crate> --check --to-remote --allow-dirty
cargo-rail rail release init <crate> --check
cargo-rail rail release check <crate>
cargo-rail rail release run <crate> --check
```

## Observed outcomes

- `config validate`, `plan`, `run --dry-run`, `split init --check`: returned `0` on all three repos.
- `config sync --check`, `unify --check`, `split run --check`, `sync --check`, `release run --check` returned `1` when changes were detected (expected check-mode behavior).
- `release check` returned `2` with `require_clean = true` in dirty test worktrees; setting `require_clean = false` for test-only runs resolved this.

## Examples linkage

- Change detection: `examples/change_detection/`
- Dependency unification: `examples/unify/rail.toml`
- Split/sync: `examples/split-sync/rail.toml`
- Release: `examples/release/rail.toml`
