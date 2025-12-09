# Migrating from cargo-hakari

If you're using `cargo-hakari` or a workspace hack crate, migration takes about 5 minutes.

## Why migrate?

- **No more meta-crate** cluttering your workspace
- **Single config file** instead of hakari.toml + workspace-hack crate
- **Resolution-based** - uses Cargo's actual resolver output
- **Multi-target aware** - computes intersections across all your target triples

## Steps

### 1. Create a branch

```bash
git checkout -b migrate-to-rail
```

### 2. Remove cargo-hakari setup

```bash
# Remove the workspace-hack crate
rm -rf crates/workspace-hack  # or wherever yours lives

# Remove from workspace members in root Cargo.toml
# Remove workspace-hack dependency from all member Cargo.tomls
# Delete .config/hakari.toml if present
```

### 3. Initialize cargo-rail

```bash
cargo install cargo-rail
cargo rail init
```

### 4. Enable transitive pinning

Edit `.config/rail.toml`:

```toml
[unify]
pin_transitives = true    # This replaces cargo-hakari
msrv = true               # Optional: compute MSRV from deps
prune_dead_features = true
```

### 5. Run unify

```bash
# Preview first
cargo rail unify --check

# Apply changes
cargo rail unify
```

### 6. Verify

```bash
cargo check --workspace
cargo test --workspace
```

### 7. Commit

```bash
git add -A
git commit -m "chore: migrate from cargo-hakari to cargo-rail"
```

## What `pin_transitives` does

Instead of a workspace-hack crate that forces dependency unification, cargo-rail:

1. Analyzes the resolved dependency graph per target triple
2. Identifies transitive dependencies used by multiple workspace members
3. Pins them in `[workspace.dependencies]` at the root
4. Updates member `Cargo.toml` files to use `workspace = true`

The result is the same build graph optimization without the meta-crate.

## Differences from cargo-hakari

| cargo-hakari | cargo-rail |
|--------------|------------|
| Workspace-hack crate | No extra crate |
| hakari.toml config | rail.toml config |
| `cargo hakari generate` | `cargo rail unify` |
| Single target | Multi-target (parallel) |
| Syntax-based | Resolution-based |

## Rollback

If something goes wrong:

```bash
cargo rail unify undo
```

This restores from the automatic backup created during unify.
