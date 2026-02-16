# Large Repo Validation

This document records reproducible validation runs against four large Rust repositories, demonstrating cargo-rail's effectiveness at scale.

## Repositories Under Test

| Repository | Crates | Description | Fork |
|------------|--------|-------------|------|
| **tokio** | 10 | Async runtime for Rust | [Fork](https://github.com/loadingalias/cargo-rail-testing/tree/main/tokio) |
| **helix** | 14 | Modal text editor | [Fork](https://github.com/loadingalias/cargo-rail-testing/tree/main/helix) |
| **meilisearch** | 23 | Full-text search engine | [Fork](https://github.com/loadingalias/cargo-rail-testing/tree/main/meilisearch) |
| **helix-db** | 6 | Graph database | [Fork](https://github.com/loadingalias/cargo-rail-testing/tree/main/helix-db) |

**Validation forks**: [cargo-rail-testing](https://github.com/loadingalias/cargo-rail-testing)

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

| Repository | Deps Unified | Undeclared Features | Dead Features | MSRV | Exclusions |
|------------|--------------|---------------------|---------------|------|------------|
| **tokio** | 9 | 7 across 5 crates | 0 | 1.85.0 | — |
| **helix** | 15 | 19 across 6 crates | 1 | 1.87.0 | — |
| **meilisearch** | 54 | 215 across 22 crates | 1 | 1.88.0 | `thiserror` |
| **helix-db** | 18 | 17 across 4 crates | 0 | 1.88.0 | `dirs` |

**Totals:**
- **96 dependencies** unified
- **258 undeclared features** fixed (silent bugs prevented)
- **2 dead features** pruned

**Exclusions Explained:**
- `thiserror` (meilisearch): v1 → v2 breaks `{type}` syntax in error macros
- `dirs` (helix-db): Intentional multi-major-version in transitive deps

---

## Post-Unify Metrics

**Date:** 2026-02-15

All repositories pass `cargo check --workspace` after unification:

| Repository | Status | Notes |
|------------|--------|-------|
| **tokio** | ✅ Pass | No warnings |
| **helix** | ✅ Pass | No warnings |
| **meilisearch** | ✅ Pass | No warnings |
| **helix-db** | ✅ Pass | 1 warning (duplicate bench target, upstream issue) |

**Build graph impact:** Unification reduces duplicate dependency resolutions. Exact timing improvements vary by workspace structure and caching state.

---

## Change Detection Results

**Date:** 2026-02-15

Analysis of 10 recent commits per repository:

| Repository | Commits | Skip Build | Skip Tests | Targeted | Key Insight |
|------------|---------|------------|------------|----------|-------------|
| **tokio** | 10 | 10% | 0% | 80% | Most commits need tests; value in crate targeting |
| **helix** | 10 | 20% | 20% | 20% | Config/doc changes detectable |
| **meilisearch** | 10 | **50%** | **50%** | 70% | High CI reduction potential |
| **helix-db** | 10 | 0% | 0% | 70% | Value in crate targeting, not skipping |

**Analysis:**
- **meilisearch** has strongest change-detection value (50% CI reduction)
- **tokio** and **helix-db** benefit from crate targeting (70-80% of runs affect subset)
- **helix** moderate benefit from config/doc detection

**Metrics source:** [cargo-rail-testing](https://github.com/loadingalias/cargo-rail-testing) — each fork includes `docs/CHANGE_DETECTION_METRICS.md`

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
