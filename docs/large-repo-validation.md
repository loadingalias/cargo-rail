# Large Repo Validation

This document records reproducible validation runs against four large Rust repositories, demonstrating cargo-rail's effectiveness at scale.

## Repositories Under Test

| Repository | Crates | Description | Fork Location |
|------------|--------|-------------|---------------|
| **tokio** | 10 | Async runtime for Rust | `loadingalias/tokio` |
| **helix** | 14 | Modal text editor | `loadingalias/helix` |
| **meilisearch** | 23 | Full-text search engine | `loadingalias/meilisearch` |
| **helix-db** | 6 | Graph database | `loadingalias/helix-db` |

---

## Validation Protocol

### Step A: Baseline Metrics

Update forks to latest upstream, record timing baselines.

```bash
cd <repo>
git fetch upstream && git rebase upstream/main
cargo clean
/usr/bin/time -l cargo check --workspace
/usr/bin/time -l cargo build --workspace
/usr/bin/time -l cargo test --workspace
```

### Step B: Initialize & Validate Config

```bash
cargo rail init  # if needed
cargo rail config validate --strict
cargo rail unify --check
```

### Step C: Apply Unify & Record Post-Unify Timing

```bash
cargo rail unify
cargo clean
/usr/bin/time -l cargo check --workspace
/usr/bin/time -l cargo test --workspace
```

### Step D: Configure Change Detection

Update `[change-detection]` section in rail.toml with repo-specific patterns.

### Step E: Validate on Historical PRs

```bash
for sha in $(git log --merges --format='%H' -10 main); do
  git checkout $sha^1
  cargo rail plan --since $sha~1 --explain
  git checkout main
done
```

### Step F: Prepare PRs

Create two PRs per repo:
1. `cargo-rail/unify` - Dependency unification
2. `cargo-rail/change-detection` - CI integration

---

## Baseline Metrics

**Date:** 2026-02-15

| Repository | cargo check | cargo build | cargo test | Notes |
|------------|-------------|-------------|------------|-------|
| **tokio** | 5.20s | 5.77s | 239.82s | Clean |
| **helix** | 18.20s | 43.31s | 62.85s | Clean |
| **meilisearch** | 141.26s | 181.50s | 349.62s | 5 LMDB failures (system) |
| **helix-db** | 52.52s | 37.50s | 345.18s | 8 LMDB failures (system) |

---

## Unify Analysis

**Date:** 2026-02-15

| Repository | Deps to Unify | Undeclared Features | Dead Features | MSRV Computed |
|------------|---------------|---------------------|---------------|---------------|
| **tokio** | 9 | 113 across 10 crates | 0 | 1.85.0 |
| **helix** | 15 | 37 across 9 crates | 1 | 1.87.0 |
| **meilisearch** | 54 | 283 across 22 crates | 1 | 1.88.0 |
| **helix-db** | 18 | 22 across 4 crates | 0 | 1.88.0 |

**Totals:**
- **96 dependencies** unified
- **455 undeclared features** fixed (silent bugs prevented)
- **2 dead features** pruned

---

## Post-Unify Metrics

_To be recorded after applying unify changes._

| Repository | cargo check | cargo build | cargo test | Delta |
|------------|-------------|-------------|------------|-------|
| **tokio** | TBD | TBD | TBD | TBD |
| **helix** | TBD | TBD | TBD | TBD |
| **meilisearch** | TBD | TBD | TBD | TBD |
| **helix-db** | TBD | TBD | TBD | TBD |

---

## Change Detection Results

_To be recorded after configuring change-detection and validating on historical PRs._

| Repository | PRs Analyzed | Avg Surfaces Skipped | Est. CI Reduction |
|------------|--------------|----------------------|-------------------|
| **tokio** | TBD | TBD | TBD |
| **helix** | TBD | TBD | TBD |
| **meilisearch** | TBD | TBD | TBD |
| **helix-db** | TBD | TBD | TBD |

---

## Command Reference

Test cargo-rail from this branch without touching global install:

```bash
./target/release/cargo-rail rail --help
```

### Full Command Matrix

```bash
# Configuration
cargo rail config validate --strict
cargo rail config sync --check

# Change Detection
cargo rail plan --merge-base -f json
cargo rail plan --merge-base --explain
cargo rail run --merge-base --profile ci --dry-run --print-cmd

# Dependency Unification
cargo rail unify --check
cargo rail unify

# Split/Sync (if applicable)
cargo rail split init <crate> --check
cargo rail split run <crate> --check --allow-dirty
cargo rail sync <crate> --check --to-remote --allow-dirty

# Release (if applicable)
cargo rail release init <crate> --check
cargo rail release check <crate>
cargo rail release run <crate> --check
```

---

## Expected Outcomes

- `config validate`, `plan`, `run --dry-run`, `split init --check`: exit code 0
- `unify --check`, `split run --check`, `sync --check`, `release run --check`: exit code 1 when changes detected (expected)
- `release check`: exit code 2 with `require_clean = true` in dirty worktrees

---

## Examples Linkage

- Change detection: `examples/change_detection/`
- Dependency unification: `examples/unify/`
- Split/sync: `examples/split-sync/`
- Release: `examples/release/`

---

## Artifacts

All validation artifacts stored in: `cargo-rail-testing/artifacts/YYYYMMDD/`

| Artifact Pattern | Description |
|------------------|-------------|
| `<repo>-baseline-*.txt` | Pre-unify timing |
| `<repo>-unify-check.txt` | Unify analysis output |
| `<repo>-unify-apply.txt` | Unify application output |
| `<repo>-postunify-*.txt` | Post-unify timing |
| `<repo>-plan-*.json` | Change detection plan outputs |
| `<repo>-explain-*.txt` | Human-readable plan explanations |
