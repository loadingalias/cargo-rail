# Unify

Dependency unification validated on production Rust workspaces.

## What This Replaces

| Tool | Purpose |
|------|---------|
| **cargo-hakari** | Dependency unification + workspace-hack |
| **cargo-udeps** | Unused dependency detection |
| **cargo-machete** | Unused dependency detection |
| **cargo-shear** | Unused dependency detection |
| **cargo-features-manager** | Feature management |
| **cargo-msrv** | MSRV computation |
| **cargo-sort** | Dependency sorting |

**Single command:** `cargo rail unify --check`

## Validated Repositories

| Repository | Crates | Deps Unified | Undeclared Features | MSRV |
|------------|-------:|-------------:|--------------------:|------|
| [tokio](https://github.com/loadingalias/cargo-rail-testing/tree/unify/tokio) | 10 | 13 | 7 | 1.85.0 |
| [helix](https://github.com/loadingalias/cargo-rail-testing/tree/unify/helix) | 14 | 28 | 19 | 1.87.0 |
| [meilisearch](https://github.com/loadingalias/cargo-rail-testing/tree/unify/meilisearch) | 23 | 70 | 215 | 1.88.0 |
| [helix-db](https://github.com/loadingalias/cargo-rail-testing/tree/unify/helix-db) | 6 | 21 | 17 | 1.88.0 |
| **Total** | **53** | **132** | **258** | — |

**Full results:** [unify-results.md](unify-results.md)

## What Unify Does

Single metadata call, comprehensive analysis:

- **Unifies versions** — writes to `[workspace.dependencies]`, converts members to `{ workspace = true }`
- **Fixes undeclared features** — detects features borrowed via Cargo's unified resolution (silent bugs)
- **Prunes dead features** — removes features never enabled in resolved graph
- **Detects unused deps** — flags dependencies not used anywhere
- **Computes MSRV** — derives minimum Rust version from dependency graph
- **Replaces workspace-hack** — enable `pin_transitives` for cargo-hakari equivalent

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

# Undo if needed
cargo rail unify undo
```

## Configuration

```toml
[unify]
detect_undeclared_features = true  # Find borrowed features (silent bugs)
fix_undeclared_features = true     # Auto-fix undeclared features
prune_dead_features = true         # Remove unused feature flags
detect_unused = true               # Detect unused dependencies
msrv = true                        # Compute workspace rust-version
pin_transitives = false            # Enable for cargo-hakari replacement
exclude = ["thiserror"]            # Skip specific dependencies
```

Workspace-member dependencies are unified as atomic cohorts. Excluding one member excludes its connected member cohort to prevent local-vs-registry split graphs.

## Trust Checklist

Before running `unify` (without `--check`):

1. ✅ Run `cargo rail unify --check` first (read-only preview)
2. ✅ Review the diff carefully
3. ✅ Run on a branch (`git checkout -b unify-deps`)
4. ✅ Test after applying (`cargo check --workspace && cargo test --workspace`)
5. ✅ Backups are created automatically (`.config/rail-backup-*/`)
6. ✅ Use `cargo rail unify undo` if needed

## Example Output

```
Loading metadata for 17 target(s)...
Analyzing 50 dependencies...
Computing MSRV from dependency graph...
Detecting unused dependencies...
Checking for undeclared feature dependencies...
Generating fixes for 5 undeclared feature issues...

=== Unification Plan ===

Dependencies to unify: 13
  - bytes = "^1.11.1" (used by 4 crates)
  - parking_lot = "^0.12.5" (used by 3 crates)
  - tracing = "^0.1.44", features = [std] (used by 3 crates)
  ...

Undeclared features to fix: 7 features across 5 crates
Computed MSRV: 1.85.0 (from 132 deps with rust-version)

ready: 13 dependencies, 58 member edits
Changes detected. Run without --check to apply.
```

## See Also

- [Configuration Reference](../../docs/config.md) — Full `[unify]` options
- [Migration from cargo-hakari](../../docs/migrate-hakari.md)
