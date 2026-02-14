# Unify Examples

Dependency unification validated on production Rust workspaces.

## Validated Repositories

These repos run `cargo rail unify --check` successfully with measurable impact:

| Repository | Crates | Locked Deps | Config | Results |
|---|---:|---:|---|---|
| tokio-rs/tokio | 10 | 224 | [rail.toml (local)](../../cargo-rail-testing/tokio/.config/rail.toml) | 9 deps unified, 19 undeclared features fixed, MSRV 1.85.0 |
| helix-editor/helix | 14 | 351 | [rail.toml (local)](../../cargo-rail-testing/helix/.config/rail.toml) | 15 deps unified, 25 undeclared features fixed, MSRV 1.87.0 |
| meilisearch/meilisearch | 23 | 835 | [rail.toml (local)](../../cargo-rail-testing/meilisearch/.config/rail.toml) | 54 deps unified, 97 undeclared features fixed, MSRV 1.88.0 |

**Aggregate:** 78 dependencies unified, 141 undeclared features fixed across 47 crates.

## What Unify Does

Single metadata call, comprehensive analysis:

- **Unifies versions** — writes to `[workspace.dependencies]`, converts members to `{ workspace = true }`
- **Fixes undeclared features** — detects features borrowed via Cargo's unified resolution (silent bugs)
- **Prunes dead features** — removes features never enabled in resolved graph
- **Detects unused deps** — flags dependencies not used anywhere
- **Computes MSRV** — derives minimum Rust version from dependency graph
- **Replaces workspace-hack** — enable `pin_transitives` for cargo-hakari equivalent

## Tools Replaced

cargo-rail unify consolidates 6 cargo plugins into one command:

- ❌ cargo-hakari → ✅ `cargo rail unify` (with `pin_transitives = true`)
- ❌ cargo-udeps → ✅ `cargo rail unify` (with `detect_unused = true`)
- ❌ cargo-machete → ✅ `cargo rail unify` (with `detect_unused = true`)
- ❌ cargo-shear → ✅ `cargo rail unify` (with `detect_unused = true`)
- ❌ cargo-features-manager → ✅ `cargo rail unify` (with `prune_dead_features = true`)
- ❌ cargo-msrv → ✅ `cargo rail unify` (with `msrv = true`)

## Quick Start

```bash
# Initialize config
cargo rail init

# Preview changes (safe, read-only)
cargo rail unify --check

# Apply changes
cargo rail unify

# Understand decisions
cargo rail unify --check --explain
```

## Baseline Config

Use `rail.toml` from this directory as a starting point. Key settings:

```toml
[unify]
detect_undeclared_features = true  # Find borrowed features (silent bugs)
fix_undeclared_features = true     # Auto-fix undeclared features
prune_dead_features = true         # Remove unused feature flags
detect_unused = true               # Detect unused dependencies
msrv = true                        # Compute workspace rust-version
pin_transitives = false            # Enable for cargo-hakari replacement
```

## Validation Artifacts

Latest validation run artifacts are in:
- `../../cargo-rail-testing/artifacts/unify-validation-*/`

These include:
- Per-repo baseline metrics (locked deps, features, etc.)
- Unify check outputs (JSON + text)
- Aggregate summary with totals

## Trust Checklist

Before running `unify` (without `--check`):

1. ✅ Run `cargo rail unify --check` first (read-only preview)
2. ✅ Review the diff carefully
3. ✅ Run on a branch (`git checkout -b unify-deps`)
4. ✅ Test after applying (`cargo check --workspace && cargo test --workspace`)
5. ✅ Backups are created automatically (`.config/rail-backup-*/`)
6. ✅ Use `cargo rail unify undo` if needed

## Example Output

From tokio-rs/tokio:

```
Loading metadata for 17 target(s)...
Analyzing 50 dependencies...
Computing MSRV from dependency graph...
Detecting unused dependencies...
Checking for undeclared feature dependencies...
Generating fixes for 19 undeclared feature issues...

=== Unification Plan ===

Dependencies to unify: 9
  - bytes = "^1.11.1" (used by 4 crates)
  - parking_lot = "^0.12.5" (used by 3 crates)
  - tracing = "^0.1.44", features = [std] (used by 3 crates)
  ...

Undeclared features to fix: 113 features across 10 crates
Computed MSRV: 1.85.0 (from 132 deps with rust-version)

ready: 9 dependencies, 52 member edits
Changes detected. Run without --check to apply.
```
