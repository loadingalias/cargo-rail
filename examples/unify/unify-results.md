# Unify Validation

Validation Forks:
 - [tokio](https://github.com/loadingalias/tokio)
 - [helix](https://github.com/loadingalias/helix)
 - [meilisearch](https://github.com/loadingalias/meilisearch)
 - [helix-db](https://github.com/loadingalias/helix-db)

## Aggregate Results

| Metric | Count |
|---|---:|
| Total crates | 53 |
| Dependencies unified | 96 |
| Dead features pruned | 2 |
| Unused deps detected | 0 |
| Undeclared features fixed | 258 |

## Per-Repository Results

| Repository | Crates | Deps Unified | Dead Features | Undeclared Features | MSRV | Exclusions |
|---|---:|---:|---:|---:|---|---|
| [tokio](https://github.com/loadingalias/cargo-rail-testing/tree/unify/tokio) | 10 | 9 | 0 | 7 | 1.85.0 | — |
| [helix](https://github.com/loadingalias/cargo-rail-testing/tree/unify/helix) | 14 | 15 | 1 | 19 | 1.87.0 | — |
| [meilisearch](https://github.com/loadingalias/cargo-rail-testing/tree/unify/meilisearch) | 23 | 54 | 1 | 215 | 1.88.0 | `thiserror` |
| [helix-db](https://github.com/loadingalias/cargo-rail-testing/tree/unify/helix-db) | 6 | 18 | 0 | 17 | 1.88.0 | `dirs` |

## Validation Status

All repositories pass `cargo check --workspace` and `cargo test --workspace` after unification.

## Exclusions Explained

| Repository | Excluded | Reason |
|---|---|---|
| meilisearch | `thiserror` | v1 → v2 breaks `{type}` syntax in error macros |
| helix-db | `dirs` | Intentional multi-major-version in transitive deps |

## Dead Features Pruned

| Crate | Feature | Why Dead |
|---|---|---|
| helix-core | `integration` | Empty feature (no-op) |
| meilisearch | `test-ollama` | Empty feature (no-op) |

Note: These can be skipped by using the `preserve_features` option in your `rail.toml` file.

## Configuration Used

Validation run used these settings:

```toml
[unify]
major_version_conflict = "warn" # Keep multi-major deps separate
sort_dependencies = false       # Preserve upstream ordering
prune_dead_features = true      # Remove empty no-op features
fix_undeclared_features = true  # Auto-fix borrowed features
# Per-repo overrides:
# - meilisearch: exclude = ["thiserror"]
# - helix-db: exclude = ["dirs"]
```

## Tools Replaced

`cargo rail unify` replaces:

| Tool | Purpose |
|---|---|
| cargo-hakari | Dependency unification + workspace-hack |
| cargo-udeps | Unused dependency detection |
| cargo-machete | Unused dependency detection |
| cargo-shear | Unused dependency detection |
| cargo-features-manager | Feature management |
| cargo-msrv | MSRV computation |
| cargo-sort | Sorting Cargo.toml manifests |

**Single command:** `cargo rail unify --check`

## Reproducing

```bash
# Clone validation forks
git clone https://github.com/loadingalias/tokio
git clone https://github.com/loadingalias/helix
git clone https://github.com/loadingalias/meilisearch
git clone https://github.com/loadingalias/helix-db

cd tokio  # or helix, meilisearch, helix-db

# Install cargo-rail
cargo install cargo-rail

# Initialize cargo-rail
cargo rail init

# Adjusted `rail.toml` to the workspace (generally)
.config/rail.toml

# Preview changes
cargo rail unify --check

# Apply changes
cargo rail unify

# Verify
cargo check --workspace
cargo test --workspace
```
