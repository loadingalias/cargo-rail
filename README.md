# cargo-rail

**Monorepo orchestration for Rust workspaces. 12 dependencies. Zero workspace-hack crates.**

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 1.91+](https://img.shields.io/badge/rust-1.91%2B-orange.svg)](https://www.rust-lang.org)

[Commands](docs/commands.md) · [Config](docs/config.md) · [Recipes](docs/recipes.md) · [Crates.io](https://crates.io/crates/cargo-rail)

```bash
cargo install cargo-rail
```

---

## The Problem

Rust monorepos are powerful but painful:

- **CI tests everything** on every PR. 50 crates, 12 minutes, every time.
- **Dependency fragmentation** causes duplicate compilations. You're maintaining a workspace-hack crate or living with the overhead.
- **Extracting crates** means git-filter-repo scripts, lost commit mappings, and manual coordination.
- **Releasing** requires juggling cargo-release, git-cliff, and custom scripts.

**cargo-rail** solves all of these with one tool and 12 dependencies.

---

## What It Does

### Test only what changed

```bash
$ cargo rail test

Changed: crates/core/src/lib.rs

Affected crates:
  core        (direct change)
  api         (depends on core)
  cli         (depends on api)

Skipped (no dependency path):
  examples
  benchmarks

Running: cargo nextest run -p core -p api -p cli
```

Graph-aware change detection. Auto-detects nextest. **70-90% reduction in CI time.**

### Unify dependencies without workspace-hack

```bash
$ cargo rail unify

Analyzed 3 target triples
Unified 23 dependencies to [workspace.dependencies]
Pinned 8 transitive-only deps
Computed MSRV: 1.75.0

Features: minimal set for full functionality
Versions: resolved from Cargo's actual resolver
TOML: comments and formatting preserved
```

- **Multi-target resolution** — Analyzes all your target triples, produces one correct dependency graph
- **Minimum features** — Intersection of required features, not union. Leaner builds.
- **Prune dead features** — Removes features declared but never used (`prune_dead_features = true`)
- **Detect & remove unused deps** — Finds deps declared but not in the resolved graph (`detect_unused = true`, `remove_unused = true`)
- **Automatic MSRV** — Computes workspace MSRV from dependency requirements (`msrv = true`)
- **No workspace-hack crate** — Native `[workspace.dependencies]`, nothing extra

### Split crates with full git history

```bash
$ cargo rail split my-crate

Extracted my-crate to standalone repo
  347 commits preserved
  Same commit SHAs (deterministic)
  Commit mapping stored in git-notes
```

Three modes:

| Mode | Use Case |
|------|----------|
| `single` | One crate → one repo |
| `multi` | Multiple crates → one repo (separate directories) |
| `workspace` | Multiple crates → one monorepo (with workspace Cargo.toml) |

### Sync with safe defaults

```bash
$ cargo rail sync my-crate

Monorepo → split: pushes to main
Split → monorepo: creates PR branch (never direct to main)
```

Bidirectional sync with safety guardrails. Changes from external contributors come in as PRs for review.

### Release with dependency ordering

```bash
$ cargo rail release --all --bump minor

Planning release:
  1. core       0.4.2 → 0.5.0
  2. api        0.3.1 → 0.4.0  (depends on core)
  3. cli        0.2.0 → 0.3.0  (depends on api)

Changelog generated from conventional commits
Tags: core@0.5.0, api@0.4.0, cli@0.3.0

--execute to publish (dry-run by default)
```

Native changelog generation. Configurable publish delays. GitHub releases via `gh`.

---

## Real-World Impact

`cargo rail affected` on recent commits (HEAD~10):

| Repository | Crates | Affected | Skipped | Reduction |
|------------|--------|----------|---------|-----------|
| [helix](https://github.com/helix-editor/helix) | 13 | 5 | 8 | 62% |
| [ripgrep](https://github.com/BurntSushi/ripgrep) | 10 | 4 | 6 | 60% |
| [vello](https://github.com/linebender/vello) | 26 | 13 | 13 | 50% |
| [tikv](https://github.com/tikv/tikv) | 83 | 42 | 41 | 49% |
| [ruff](https://github.com/astral-sh/ruff) | 43 | 23 | 20 | 47% |
| [meilisearch](https://github.com/meilisearch/meilisearch) | 19 | 17 | 2 | 11% |
| [polars](https://github.com/pola-rs/polars) | 33 | 32 | 1 | 3% |

*Results vary by commit range. Monorepos with focused PRs see larger reductions.*

---

## Why cargo-rail

### 12 dependencies, 92 resolved

Compare: cargo-hakari pulls ~40 direct via guppy. release-plz pulls 600+ resolved.

Minimal supply chain. Easy to audit. Fast to compile.

### Correct by construction

cargo-rail queries Cargo's actual resolver across all your target triples:

```toml
# crate-a: tokio = "1.0"          (needs rt)
# crate-b: tokio = "^1.5"         (needs net)
# crate-c: tokio = "1"            (needs sync, linux-only)

# cargo-rail resolves across x86_64-linux, aarch64-darwin, x86_64-windows:
[workspace.dependencies]
tokio = { version = "1.5", features = ["rt", "net", "sync"] }
```

- **Versions**: Always the resolved version, not the requested range
- **Features**: Minimum set required across all targets (intersection, not union)
- **MSRV**: Auto-computed from dependency requirements when `msrv = true`

### System git, no libgit2

Split and sync use the system `git` binary directly. No libgit2 or gitoxide dependencies:

- Maximum fidelity with your existing git workflows
- Deterministic: same input → same commit SHAs
- git-notes for rebase-safe commit mapping

### Lossless TOML editing

Uses `toml_edit` to preserve comments, formatting, and structure:

```toml
# Your carefully written comments
tokio = { version = "1.5" }  # ← stays exactly here
```

---

## Commands

| Command | Purpose |
|---------|---------|
| `affected` | Show crates affected by changes |
| `test` | Run tests for affected crates only |
| `unify` | Unify deps to `[workspace.dependencies]` |
| `split` | Extract crate to standalone repo with history |
| `sync` | Bidirectional monorepo ↔ split repo sync |
| `release` | Version bump, changelog, tag, publish |
| `init` | Generate `rail.toml` configuration |
| `check` | Validate release readiness |
| `clean` | Remove generated artifacts |

Run `cargo rail <command> --help` for details, or see [docs/commands.md](docs/commands.md)

---

## Configuration

```bash
cargo rail init  # Auto-detects targets, generates rail.toml
```

```toml
# .config/rail.toml or rail.toml
targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-pc-windows-msvc"]

[unify]
pin_transitives = true      # Pin transitive-only deps (workspace-hack replacement)
msrv = true                 # Compute and set workspace MSRV
detect_unused = true        # Find unused dependencies
remove_unused = true        # Remove them automatically
prune_dead_features = true  # Remove features never used
exclude = ["openssl"]       # Skip specific deps

[release]
publish_delay = 5           # Seconds between crates.io publishes
create_github_release = true

[crates.my-crate.split]
remote = "git@github.com:org/my-crate.git"
mode = "single"             # or "multi", "workspace"
```

---

## Replaces

| Tool | What cargo-rail does instead |
|------|------------------------------|
| cargo-hakari | Resolution-based unification, no workspace-hack crate |
| cargo-hackerman | Same — workspace-hack replacement via `pin_transitives` |
| cargo-udeps | Unused dep detection via `detect_unused`, removal via `remove_unused` |
| cargo-machete | Same — detects deps not in resolved graph |
| cargo-shear | Same — unused dep detection and removal |
| cargo-msrv | Auto-compute workspace MSRV via `msrv = true` |
| cargo-release | Dependency-ordered publishing, native changelog |
| release-plz | Same, with 600 fewer dependencies |
| git-cliff | Built-in conventional commit parsing |
| Copybara | Deterministic split/sync with git-notes |
| CI shell scripts | Graph-aware `affected` with nextest integration |

### cargo-rail vs cargo-hakari

| Feature | cargo-hakari | cargo-rail |
|---------|--------------|------------|
| Requires extra crate | Yes (workspace-hack) | No |
| Preserves TOML comments | No | Yes |
| Multi-target aware | No | Yes |
| Feature computation | Union (all features) | Intersection (minimal) |
| Dependencies | ~40 via guppy | 12 |

### cargo-rail vs release-plz

| Feature | release-plz | cargo-rail |
|---------|-------------|------------|
| Version bumping | Yes | Yes |
| Changelog generation | Yes | Yes |
| Dependency ordering | Yes | Yes |
| GitHub releases | Yes | Yes |
| Dependencies | ~600 resolved | 12 |
| Crate splitting | No | Yes |
| Bidirectional sync | No | Yes |

---

## Installation

```bash
cargo install cargo-rail
```

From source:

```bash
git clone https://github.com/loadingalias/cargo-rail
cd cargo-rail && cargo install --path .
```

Requires Rust 1.91+.

---

## License

MIT

---

<sub>Built by [@loadingalias](https://github.com/loadingalias). Contributions welcome.</sub>
