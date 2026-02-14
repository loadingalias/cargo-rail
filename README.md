# cargo-rail

> Cargo-native control plane for Rust monorepos: plan changes, run only needed work locally and in CI, unify the graph (deps/features), split/sync crates into new repos/monorepos, and automate releases with 14 core dependencies.

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail) [![docs.rs](https://img.shields.io/docsrs/cargo-rail)](https://docs.rs/cargo-rail) [![CI](https://img.shields.io/github/actions/workflow/status/loadingalias/cargo-rail/commit.yaml?branch=main)](https://github.com/loadingalias/cargo-rail/actions/workflows/commit.yaml) [![MSRV](https://img.shields.io/crates/msrv/cargo-rail)](https://github.com/loadingalias/cargo-rail/blob/main/Cargo.toml)

## Why cargo-rail

**Impact on real repos (validated across tokio, helix, meilisearch):**

| Metric | Impact |
|---|---|
| **CI surface execution** | 55% fewer surfaces run per merge |
| **Weighted test/build units** | 64% reduction in compute units |
| **Dependencies unified** | 78 across 47 crates (avg 1.7 per crate) |
| **Undeclared features fixed** | 141 silent bugs prevented |
| **Tooling consolidation** | 6 cargo plugins → 1 command |
| **MSRV computation** | Automatic from dependency graph |

**Two compound effects = massive time/cost savings:**
1. **Change detection** (`plan`/`run`) reduces what runs — 55% fewer CI surfaces, 64% fewer compute units
2. **Dependency unification** (`unify`) reduces build graph complexity — cleaner deps, smaller build units, fewer rebuilds

**Before cargo-rail:** Run 6 tools separately (hakari, udeps, machete, shear, features-manager, msrv), each with different data/timing
**After cargo-rail:** `cargo rail unify --check` in one metadata call

## Quick Start

```bash
# install
cargo install cargo-rail

# generate config
cargo rail init

# deterministic planning + execution
cargo rail plan --merge-base
cargo rail run --merge-base --profile ci

# dependency hygiene
cargo rail unify --check
```

Pre-built binaries: [GitHub Releases](https://github.com/loadingalias/cargo-rail/releases)

## Core Workflows

### Change Planning + Execution (`plan` / `run`)

**Problem:** CI wastes resources testing unchanged code. Teams either (1) test everything on every commit, or (2) build custom scripts that drift from local behavior.

**Solution:** One planner contract, used everywhere:

```bash
# Local: what would CI run if I pushed this branch?
cargo rail plan --merge-base

# CI: deterministic plan → selective execution
cargo rail plan --merge-base -f github  # outputs: build=true, test=false, docs=true, ...
cargo rail run --merge-base --profile ci  # runs ONLY what plan selected
```

**How repos use this:**
1. Configure change detection rules in `.config/rail.toml` (infrastructure files, doc-only changes, etc.)
2. Run `plan` to see impact classification (which surfaces: build, test, bench, docs, infra)
3. Run `run` to execute only selected surfaces — locally or in CI
4. Use `--explain` to understand any decision: "why did this run?" / "why was this skipped?"

Result: **55% fewer CI surface executions, 64% reduction in weighted compute units** (validated on tokio/helix/meilisearch).

### Dependency Unification (`unify`)

**Problem:** Teams juggle 6+ cargo plugins for dependency hygiene (hakari, udeps, machete, shear, features-manager, msrv). Each runs separately, on different data, with different CLI patterns. Undeclared features (borrowed from Cargo's resolver) break isolated builds.

**Solution:** `cargo rail unify` — one command, one metadata call, comprehensive analysis:

```bash
cargo rail unify --check    # preview all changes
cargo rail unify            # apply workspace-wide
cargo rail unify --explain  # understand each decision
```

**What it does:**
- **Unifies versions** — writes to `[workspace.dependencies]`, converts members to `{ workspace = true }`
- **Fixes undeclared features** — detects features borrowed via Cargo's unified resolution
- **Prunes dead features** — removes features never enabled in resolved graph
- **Detects unused deps** — flags dependencies not used anywhere
- **Computes MSRV** — derives minimum Rust version from dependency graph
- **Replaces workspace-hack** — enable `pin_transitives` for cargo-hakari equivalent

**Validated impact on real repos:**

| Repository | Crates | Locked Deps | Deps Unified | Undeclared Features | MSRV Computed |
|---|---:|---:|---:|---:|---|
| tokio-rs/tokio | 10 | 224 | 9 | 19 | 1.85.0 |
| helix-editor/helix | 14 | 351 | 15 | 25 | 1.87.0 |
| meilisearch/meilisearch | 23 | 835 | 54 | 97 | 1.88.0 |
| **Aggregate** | **47** | **1,410** | **78** | **141** | — |

Config files and validation artifacts: [examples/unify/](examples/unify/)

**Tools replaced:** cargo-hakari, cargo-udeps, cargo-machete, cargo-shear, cargo-features-manager, cargo-msrv (6 tools → 1 command)

### Split + Sync (Google Copybara Replacement)

**Problem:** Teams need to publish crates from monorepos but want clean standalone repos with full git history. Existing tools (git subtree, git-filter-repo) are one-way and manual. Google's Copybara requires Bazel and complex config.

**Solution:** `cargo rail split` + `cargo rail sync` — bidirectional sync with 3-way conflict resolution:

```bash
# Extract crate to standalone repo with full git history
cargo rail split init my-crate  # configure once
cargo rail split run my-crate   # extract with history preserved

# Bidirectional sync
cargo rail sync my-crate --to-remote    # push monorepo changes to split repo
cargo rail sync my-crate --from-remote  # pull split repo changes (creates PR branch)
```

**Three modes:**
- `single`: one crate → one repo (most common)
- `combined`: multiple crates → one repo (shared utilities)
- `workspace`: multiple crates → workspace structure (mirrors monorepo)

Built on system git (not libgit2) for deterministic SHAs and full git fidelity.

### Release Automation (`release`)

Release checks, versioning, changelogs, tags, dependency-order publish.

## Why This Model

- One planner contract for local and CI.
- One executor (`run`) for planner-selected surfaces.
- One config (`rail.toml`) for policy.
- Check-mode exit code `1` means "changes detected" (not a crash).

## GitHub Actions Integration

For CI integration, use [cargo-rail-action](https://github.com/loadingalias/cargo-rail-action) — a thin transport over `cargo rail plan -f github` that handles installation, checksum verification, and output publishing for job gating.

The action keeps CI behavior aligned with local `plan` + `run` workflows.

## Configuration

`cargo rail init` generates `.config/rail.toml`.

**Example config:**

```toml
# Platform targets for multi-target validation
targets = [
  "aarch64-apple-darwin",
  "aarch64-unknown-linux-gnu",
  "x86_64-pc-windows-msvc",
  "x86_64-unknown-linux-gnu",
]

[unify]
pin_transitives = false      # Enable for cargo-hakari replacement
detect_unused = true         # Detect unused dependencies
prune_dead_features = true   # Remove features never enabled
msrv = true                  # Compute workspace rust-version
detect_undeclared_features = true  # Find borrowed features

[release]
tag_format = "{prefix}{version}"
publish_delay = 5
sign_tags = true

[change-detection]
# Files triggering full workspace rebuild
infrastructure = [
  ".github/**",
  "scripts/**",
  "justfile",
  "rust-toolchain.toml",
]
```

**Full documentation:**
- [Configuration reference](docs/config.md)
- [Command reference](docs/commands.md)
- [Architecture](docs/architecture.md)
- [Change detection recipe](docs/change-detection-recipe.md)
- [Change detection operations guide](docs/change-detection-operations.md)
- [Troubleshooting](docs/troubleshooting.md)

## Migration Guides

- Replace `affected` / `test` flows for pre-v0.10.0 releases of `cargo-rail`: [docs/adr/0001-migrate-affected-test-to-plan-run.md](docs/adr/0001-migrate-affected-test-to-plan-run.md)
- Replace `cargo-hakari`: [docs/migrate-hakari.md](docs/migrate-hakari.md)

## Proven On Large Repos

All core workflows (`plan`/`run`, `unify`, `split`, `sync`, `release`) validated on production repos:

| Repository | Crates | Locked Deps | Validation Coverage |
|---|---:|---:|---|
| [tokio-rs/tokio](https://github.com/tokio-rs/tokio) | 10 | 224 | Plan/run (5 merges), unify, split, sync, release |
| [helix-editor/helix](https://github.com/helix-editor/helix) | 14 | 351 | Plan/run (5 merges), unify, split, sync, release |
| [meilisearch/meilisearch](https://github.com/meilisearch/meilisearch) | 23 | 835 | Plan/run (5 merges), unify, split, sync, release |

**Why this matters:**

Validation isn't a single test — it's a protocol:

1. **Reproducibility**: Every command in [docs/large-repo-validation.md](docs/large-repo-validation.md) runs on forked repos with real merge history
2. **Metrics collection**: Automated scripts measure execution reduction, surface accuracy, plan duration, unify impact
3. **Quality audit**: Heuristics flag potential false positives/negatives for human review
4. **Real-world scenarios**: Tests run on actual merge commits and real dependency graphs, not synthetic fixtures

**Change detection results (15 merge scenarios):**
- 55% execution reduction rate (surfaces)
- 64% weighted reduction rate (compute units)
- Average plan duration: 632ms
- Quality audit: 2 potential false-negatives, 1 potential false-positive (flagged for review)

**Unify results (3 repos, 47 crates):**
- 78 dependencies unified
- 141 undeclared features fixed (silent bugs prevented)
- 2 dead features pruned
- MSRV computed for all repos (1.85.0 - 1.88.0)

Full protocol and raw artifacts: [examples/README.md](examples/README.md)

## Examples

Each workflow includes working config files and reproducible command sequences:

- Change detection: [examples/change_detection/](examples/change_detection/)
- Unify: [examples/unify/](examples/unify/)
- Split/sync: [examples/split-sync/](examples/split-sync/)
- Release: [examples/release/](examples/release/)

All examples run on real repos (tokio, helix, meilisearch) and include:
- Rail config (`.config/rail.toml`)
- Command sequence (step-by-step)
- Expected outputs
- Validation artifacts

## Getting Help

- Issues: [GitHub Issues](https://github.com/loadingalias/cargo-rail/issues)
- Crate: [crates.io/cargo-rail](https://crates.io/crates/cargo-rail)

## License

Licensed under [MIT](LICENSE).
