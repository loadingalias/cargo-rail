# cargo-rail

> A deterministic, cargo-native control plane for Rust monorepos: build/check/test/bench only what changes locally and in CI, unify the graph (deps/features), split/sync crates into new repos/monorepos, and automate releases with 14 core dependencies.

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail) [![docs.rs](https://img.shields.io/docsrs/cargo-rail)](https://docs.rs/cargo-rail) [![CI](https://img.shields.io/github/actions/workflow/status/loadingalias/cargo-rail/commit.yaml?branch=main)](https://github.com/loadingalias/cargo-rail/actions/workflows/commit.yaml) [![MSRV](https://img.shields.io/crates/msrv/cargo-rail)](https://github.com/loadingalias/cargo-rail/blob/main/Cargo.toml)

## Why

**Documented impact on real repos (tokio, helix, meilisearch):**

| Metric | Impact |
|---|---|
| **CI Surface Execution** | 55% fewer surfaces run per merge |
| **Weighted `test`/`build` Units** | 64% reduction in compute units |
| **Dependencies Unified** | 96 across 53 crates (avg 1.8 per crate) |
| **Undeclared `features` Fixed** | 258 silent bugs prevented |
| **Tooling Consolidation** | 6-8 cargo plugins → 1 command |
| **MSRV Computation** | Automatic from dependency graph |

**Compounding effects = massive time/cost savings:**
1. **Change Detection** (`plan`/`run`) reduces what runs — 55% fewer CI surfaces, 64% fewer compute units w/ shown determinism
2. **Dependency Unification** (`unify`) reduces build graph complexity — cleaner deps, smaller build units, fewer rebuilds
3. **Smaller Build Graphs** cargo-rail uses 14 core-deps for automatically removing unused dependencies, pruning truly dead features, unifying undeclared features, computing MSRV, splitting and synching crate/s to new, clean repos, and the entire release workflow w/ changelog generation. This results in fewer tools and less cargo-metadata fetches.

**Before cargo-rail:** Run 6 tools separately (hakari and/or workspace-hack, udeps, machete, shear, features-manager, msrv, sort, and more), each with different data/timing
**After cargo-rail:** `cargo rail unify --check` in one metadata call

## Quick Start

```bash
# install
cargo install cargo-rail

# generate config
cargo rail init

# dependency hygiene
cargo rail unify --check

# deterministic planning + execution
cargo rail plan --merge-base
cargo rail run --merge-base --profile ci
cargo rail plan --merge-base --explain

```

Pre-built binaries: [GitHub Releases](https://github.com/loadingalias/cargo-rail/releases)

## Workflows

### Change Planning (detection) + Execution (`plan` / `run`)

**Problem:** CICD wastes resources testing unchanged code. We either (1) test everything on every commit, or (2) build custom scripts that drift from local behavior.

**Solution:** One planner contract, used everywhere:

```bash
# Local: what would CI run if I pushed this branch?
cargo rail plan --merge-base

# CI: deterministic plan → selective execution
cargo rail plan --merge-base -f github  # outputs: build=true, test=false, docs=true, ...
cargo rail run --merge-base --profile ci  # runs ONLY what plan selected
```

**How to use this effectively:**
1. Configure change detection rules in `.config/rail.toml` (infrastructure files, doc-only changes, custom, etc.)
2. Run `plan` to see impact classification (which surfaces: build, test, bench, docs, infra, custom)
3. Run `run` to execute only selected surfaces — locally or in CI - or wire to `justfile`, `makefile`, `xtask`, or shell scripts.
4. Use `--explain` to understand any decision: "why did this run?" / "why was this skipped?" 

Result: **55% fewer CI surface executions, 64% reduction in weighted compute units** (validated on tokio/helix/meilisearch) See: [Examples](examples/change_detection).

### Dependency Unification (`unify`)

**Problem:** We all juggle 6+ cargo plugins for dependency hygiene (hakari, udeps, machete, shear, features-manager, msrv, sort, etc.). Each runs separately, on different data, with different CLI patterns... pulling the same cargo-metadata over and over. Undeclared features (borrowed from Cargo's resolver) break isolated builds silently.

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
- **Configurable** - we all have different ideas about what clean means, such as whether to remove unused dependencies or prune features or sort manifests - that's what your `rail.toml` file is for.

**Validated impact on real repos:**

| Repository | Crates | Deps Unified | Undeclared Features | MSRV Computed |
|---|---:|---:|---:|---|
| [tokio-rs/tokio](https://github.com/loadingalias/cargo-rail-testing/tree/main/tokio) | 10 | 9 | 7 | 1.85.0 |
| [helix-editor/helix](https://github.com/loadingalias/cargo-rail-testing/tree/main/helix) | 14 | 15 | 19 | 1.87.0 |
| [meilisearch/meilisearch](https://github.com/loadingalias/cargo-rail-testing/tree/main/meilisearch) | 23 | 54 | 215 | 1.88.0 |
| [helixdb/helix-db](https://github.com/loadingalias/cargo-rail-testing/tree/main/helix-db) | 6 | 18 | 17 | 1.88.0 |
| **Aggregate** | **53** | **96** | **258** | — |

Config files and validation artifacts: [Examples](examples/unify/) | [Validation Forks](https://github.com/loadingalias/cargo-rail-testing)

### Split + Sync (Google Copybara Replacement)

**Problem:** We need/want to publish crates from monorepos but want clean standalone repos with full git history. I wanted to build in a canonical dev monorepo, but release crates independently. Google'sCopybara requires so much and it's built w/ Java. Existing tools (git subtree, git-filter-repo) are one-way and manual. This offers us a bidirectional sync engine w/ 3-way merge conflict resolution in the event we need it; it never merges to `main` w/o a review PR for the canonical repo.

**Solution:** `cargo rail split` + `cargo rail sync` — bidirectional sync with 3-way conflict resolution:

```bash
# Extract crate to standalone repo with full git history
cargo rail split init crate/s  # configure once, automatically
cargo rail split run crate/s   # extract with history preserved

# Bidirectional sync
cargo rail sync crate/s --to-remote    # push monorepo changes to split repo
cargo rail sync crate/s --from-remote  # pull split repo changes (creates PR branch)
```

**Three modes:**
- `single`: one crate → one repo (most common)
- `combined`: multiple crates → one repo (shared utilities)
- `workspace`: multiple crates → workspace structure (mirrors monorepo)

Built on system git (not libgit2) for deterministic SHAs and full git fidelity with less attack surface/weight in the graph.

### Release Automation (`release`)

Release checks, versioning, changelogs, tags, dependency-order publish. Release_plz is great, but it's pulling in something like 500 deps to release our work. That's too much weight to carry around in the graph and too much attack surface in my world. I release `cargo-rail` with 1`cargo-rail`.

```bash
# Cut a new release
cargo rail release run cargo-rail --bump patch --yes  # swap 'patch', 'minor', or 'major' as needed
git push origin main --follow-tags   # follow up
```

This gives me a clean changelog, tags, crates.io / Github release.

## GitHub Actions Integration

For CICD integration, use [cargo-rail-action](https://github.com/loadingalias/cargo-rail-action) — a thin transport over `cargo rail plan -f github` that handles installation, checksum verification, and output publishing for job gating. It will make output cleaner and more readable.

The action keeps CI behavior aligned with local `plan` + `run` workflows.

## Config

- `cargo rail init` generates `.config/rail.toml`.
- `cargo rail config sync` updates `.config/rail.toml` with latest defaults from new releases installed.
- `cargo rail config validate` validates `.config/rail.toml` for when you're unsure.

By default, on `cargo rail init`, the `rail.toml` file is written to the `.config/` directory. It's created if it doesn't exist. However, you can move/change it to the workspace root if you prefer it there. You can also 'unhide' the file if you'd like. [Reference](docs/config.md)

**Full documentation:**
- [Configuration Reference](docs/config.md)
- [Command Reference](docs/commands.md)
- [Architecture](docs/architecture.md)
- [Change Detection Recipe](docs/change-detection-recipe.md)
- [Change Detection Operations Guide](docs/change-detection-operations.md)
- [How to Use Change Detection Effectively](docs/how-to-use-cargo-rail-change-detection.md)
- [Troubleshooting](docs/troubleshooting.md)

## Migration Guides

- [Migrate from `cargo-hakari`](docs/migrate-hakari.md)
- [Upgrade from `cargo-rail` v0.9.x to v0.10.x](docs/upgrade-to-v0.10.0.md)

## Tested & Proven On Large Repos

All core workflows (`plan`/`run`, `unify`) validated on production repos with full git history:

| Repository | Crates | Validation | Fork |
|---|---:|---|---|
| tokio-rs/tokio | 10 | Unify (9 deps, 7 features), Plan/run | [Fork](https://github.com/loadingalias/cargo-rail-testing/tree/main/tokio) |
| helix-editor/helix | 14 | Unify (15 deps, 19 features), Plan/run | [Fork](https://github.com/loadingalias/cargo-rail-testing/tree/main/helix) |
| meilisearch/meilisearch | 23 | Unify (54 deps, 215 features), Plan/run | [Fork](https://github.com/loadingalias/cargo-rail-testing/tree/main/meilisearch) |
| helixdb/helix-db | 6 | Unify (18 deps, 17 features), Plan/run | [Fork](https://github.com/loadingalias/cargo-rail-testing/tree/main/helix-db) |

**Validation forks**: [cargo-rail-testing](https://github.com/loadingalias/cargo-rail-testing) — full configs, integration guides, and reproducible artifacts.

**How to Validate:**

1. **Reproducibility**: Every command in [docs/large-repo-validation.md](docs/large-repo-validation.md) runs on forked repos with real merge history
2. **Metrics collection**: Automated scripts measure execution reduction, surface accuracy, plan duration, unify impact
3. **Quality audit**: Heuristics flag potential false positives/negatives for human review
4. **Real-world scenarios**: Tests run on actual merge commits and real dependency graphs, not synthetic fixtures

**Unify results (4 repos, 53 crates):**
- 96 dependencies unified to `[workspace.dependencies]`
- 258 undeclared features fixed (silent bugs prevented)
- 2 dead features pruned
- MSRV computed for all repos (1.85.0 - 1.88.0)

Full protocol and raw artifacts: [examples/README.md](examples/README.md)

## Examples

Each workflow includes working config files and reproducible command sequences:

| Workflow | Examples | Real-World Configs |
|---|---|---|
| Change detection | [examples/change_detection/](examples/change_detection/) | [tokio](https://github.com/loadingalias/cargo-rail-testing/blob/main/tokio/.config/rail.toml), [helix](https://github.com/loadingalias/cargo-rail-testing/blob/main/helix/.config/rail.toml), [meilisearch](https://github.com/loadingalias/cargo-rail-testing/blob/main/meilisearch/.config/rail.toml) |
| Unify | [examples/unify/](examples/unify/) | [Validation results](examples/unify/VALIDATION_RESULTS.md) |
| Split/sync | [examples/split-sync/](examples/split-sync/) | — |
| Release | [examples/release/](examples/release/) | — |

**Full validation configs**: Each fork in [cargo-rail-testing](https://github.com/loadingalias/cargo-rail-testing) includes:
- `.config/rail.toml` — production-ready config
- `docs/cargo-rail-integration-guide.md` — step-by-step integration
- `docs/CHANGE_DETECTION_METRICS.md` — measured impact analysis

## Getting Help

- Issues: [GitHub Issues](https://github.com/loadingalias/cargo-rail/issues)
- Crate: [crates.io/cargo-rail](https://crates.io/crates/cargo-rail)

## License

Licensed under [MIT](LICENSE).
