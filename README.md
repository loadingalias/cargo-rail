# cargo-rail

> Deterministic Rust monorepo orchestration: run only impacted CI surfaces, unify dependency graphs, split/sync crates across repos, and automate releases.

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail) [![docs.rs](https://img.shields.io/docsrs/cargo-rail)](https://docs.rs/cargo-rail) [![CI](https://img.shields.io/github/actions/workflow/status/loadingalias/cargo-rail/commit.yaml?branch=main)](https://github.com/loadingalias/cargo-rail/actions/workflows/commit.yaml) [![MSRV](https://img.shields.io/crates/msrv/cargo-rail)](https://github.com/loadingalias/cargo-rail/blob/main/Cargo.toml)

## Why

**Documented change-detection impact on real repos (tokio, helix, meilisearch, helix-db):**

| Metric | Impact |
|---|---|
| **Commits Measured** | 80 (20 per repo) |
| **Could Skip Build** | 21% |
| **Could Skip Tests** | 19% |
| **Targeted (Not Full Run)** | 68% |
| **Dependencies Unified** | 132 across 53 crates |
| **Undeclared `features` Fixed** | 258 silent bugs prevented |
| **MSRV Computation** | Automatic from dependency graph |

**Why this matters:**
1. **Change Detection** (`plan`/`run`) reduces what runs — 68% of measured commits avoided full-run behavior, with explicit plan traces.
2. **Dependency Unification** (`unify`) reduces graph complexity — cleaner deps, smaller build units, fewer rebuilds.
3. **Toolchain Consolidation** keeps workflows in one tool with 14 core dependencies, reducing repeated metadata fetches and plugin sprawl.

**Before cargo-rail:** run multiple plugins independently (hakari/workspace-hack, udeps, machete, shear, features-manager, msrv, sort, and others), often over separate metadata views.  
**After cargo-rail:** run `cargo rail unify --check` with one metadata call.

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

**Problem:** CI/CD often runs full pipelines for small changes, or relies on custom scripts that drift from local behavior.

**Solution:** one planner contract used locally and in CI:

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
3. Run `run` to execute only selected surfaces, locally or in CI, and integrate with `justfile`, `makefile`, `xtask`, or shell scripts.
4. Use `--explain` to understand any decision: "why did this run?" / "why was this skipped?" 

Result: **21% build skip, 19% test skip, 68% targeted/non-full runs** across 80 measured commits (tokio/helix/meilisearch/helix-db). See: [Examples](examples/change_detection).

### Dependency Unification (`unify`)

**Problem:** dependency hygiene usually requires multiple plugins (hakari, udeps, machete, shear, features-manager, msrv, sort, and others), each with separate commands and metadata passes. Undeclared features borrowed from Cargo's resolver can break isolated builds silently.

**Solution:** `cargo rail unify` provides one command and one metadata pass for integrated analysis:

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
- **Configurable** — tune behavior in `rail.toml` (unused removal, feature pruning, sorting, and more).

Unused detection combines resolved-graph analysis with rustc diagnostics (`unused_crate_dependencies`) so deps that are resolved but never referenced are also removed. Optional deps and unconfigured target-gated deps are conservatively skipped.

**Validated impact on real repos:**

| Repository | Crates | Deps Unified | Undeclared Features | MSRV Computed |
|---|---:|---:|---:|---|
| [tokio-rs/tokio](https://github.com/loadingalias/cargo-rail-testing/tree/unify/tokio) | 10 | 13 | 7 | 1.85.0 |
| [helix-editor/helix](https://github.com/loadingalias/cargo-rail-testing/tree/unify/helix) | 14 | 28 | 19 | 1.87.0 |
| [meilisearch/meilisearch](https://github.com/loadingalias/cargo-rail-testing/tree/unify/meilisearch) | 23 | 70 | 215 | 1.88.0 |
| [helixdb/helix-db](https://github.com/loadingalias/cargo-rail-testing/tree/unify/helix-db) | 6 | 21 | 17 | 1.88.0 |
| **Aggregate** | **53** | **132** | **258** | — |

Config files and validation artifacts: [Examples](examples/unify/) | [Validation Forks](https://github.com/loadingalias/cargo-rail-testing)

### Split + Sync (Copybara Alternative)

**Problem:** teams often need to develop in a monorepo but publish crates from standalone repos with full history. Existing tools (`git subtree`, `git-filter-repo`) are mostly one-way and manual.

**Solution:** `cargo rail split` + `cargo rail sync` — bidirectional sync with 3-way conflict resolution:

```bash
# Extract a crate to a standalone repo with full git history
cargo rail split init crates/my-crate  # configure once
cargo rail split run crates/my-crate   # extract with history preserved

# Bidirectional sync
cargo rail sync crates/my-crate --to-remote    # push monorepo changes to split repo
cargo rail sync crates/my-crate --from-remote  # pull split repo changes (creates PR branch)
```

**Three modes:**
- `single`: one crate → one repo (most common)
- `combined`: multiple crates → one repo (shared utilities)
- `workspace`: multiple crates → workspace structure (mirrors monorepo)

Built on system git (not libgit2) for deterministic SHAs and full git fidelity with less attack surface/weight in the graph.

### Release Automation (`release`)

Release checks, versioning, changelogs, tags, and dependency-order publish in one flow, with a lean dependency footprint.

```bash
# Cut a new release
cargo rail release run cargo-rail --bump patch --yes  # swap 'patch', 'minor', or 'major' as needed
git push origin main --follow-tags   # follow up
```

This produces changelog entries, tags, crates.io publish, and GitHub release metadata.

## GitHub Actions Integration

For CI/CD integration, use [cargo-rail-action](https://github.com/loadingalias/cargo-rail-action) (`uses: loadingalias/cargo-rail-action@v3`) — a thin transport over `cargo rail plan -f github` that handles installation, checksum verification, and output publishing for job gating.

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
- [Change Detection Guide](docs/change-detection.md)
- [Troubleshooting](docs/troubleshooting.md)

## Migration Guides

- [Migrate from `cargo-hakari`](docs/migrate-hakari.md)
- [Upgrade from `cargo-rail` v0.9.x to v0.10.x](docs/upgrade-to-v0.10.0.md)

## Tested & Proven On Large Repos

All core workflows (`plan`/`run`, `unify`) validated on production repos with full git history:

| Repository | Crates | Validation | Fork |
|---|---:|---|---|
| tokio-rs/tokio | 10 | Unify (13 deps, 7 features), Plan/run | [Fork](https://github.com/loadingalias/cargo-rail-testing/tree/unify/tokio) |
| helix-editor/helix | 14 | Unify (28 deps, 19 features), Plan/run | [Fork](https://github.com/loadingalias/cargo-rail-testing/tree/unify/helix) |
| meilisearch/meilisearch | 23 | Unify (70 deps, 215 features), Plan/run | [Fork](https://github.com/loadingalias/cargo-rail-testing/tree/unify/meilisearch) |
| helixdb/helix-db | 6 | Unify (21 deps, 17 features), Plan/run | [Fork](https://github.com/loadingalias/cargo-rail-testing/tree/unify/helix-db) |

**Validation forks**: [cargo-rail-testing](https://github.com/loadingalias/cargo-rail-testing) — full configs, integration guides, and reproducible artifacts.

**How to Validate:**

1. **Reproducibility**: Every command in [examples/validation-protocol.md](examples/validation-protocol.md) runs on forked repos with real merge history
2. **Metrics collection**: Automated scripts measure execution reduction, surface accuracy, plan duration, unify impact
3. **Quality audit**: Heuristics flag potential false positives/negatives for human review
4. **Real-world scenarios**: Tests run on actual merge commits and real dependency graphs, not synthetic fixtures

**Unify results (4 repos, 53 crates):**
- 132 dependencies unified to `[workspace.dependencies]`
- 258 undeclared features fixed (silent bugs prevented)
- 2 dead features pruned
- MSRV computed for all repos (1.85.0 - 1.88.0)

Full protocol and raw artifacts: [Validation protocol](examples/validation-protocol.md) | [Unify results](examples/unify/unify-results.md)

## Examples

Each workflow includes working config files and reproducible command sequences:

| Workflow | Examples | Real-World Configs |
|---|---|---|
| Change detection | [examples/change_detection/](examples/change_detection/) | [tokio](https://github.com/loadingalias/cargo-rail-testing/blob/unify/tokio/.config/rail.toml), [helix](https://github.com/loadingalias/cargo-rail-testing/blob/unify/helix/.config/rail.toml), [meilisearch](https://github.com/loadingalias/cargo-rail-testing/blob/unify/meilisearch/.config/rail.toml) |
| Unify | [examples/unify/](examples/unify/) | [Validation results](examples/unify/unify-results.md) |
| Split/sync | [examples/split-sync/](examples/split-sync/) | — |
| Release | [examples/release/](examples/release/) | — |

**Full validation configs**: Each fork in [cargo-rail-testing](https://github.com/loadingalias/cargo-rail-testing) includes:
- `.config/rail.toml` — production-ready config
- `docs/cargo-rail-integration-guide.md` — step-by-step integration
- `docs/CHANGE_DETECTION_METRICS.md` — measured impact analysis

## Getting Help

- Issues: [GitHub Issues](https://github.com/loadingalias/cargo-rail/issues)
- Crate: [crates.io/cargo-rail](https://crates.io/crates/cargo-rail)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## Security

See [SECURITY.md](SECURITY.md).

## Code of Conduct

See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## License

Licensed under [MIT](LICENSE).
