# cargo-rail

**Graph-aware workspace orchestration for Rust monorepos.**

Split crates to standalone repos, sync bidirectionally, run smart CI, enforce policies, orchestrate releases—all from one tool.

---

## Status

This crate is under active development. I've deliberately not published anything yet. If you stumble upon it and are interested in it; feel free. I'd love any reviews early on. However, be aware, it's not ready for any kind of high-stakes, production work. It has many things that need to be fixed. It has many unhinged comments/TODO items.

I estimate this will be v1 ready in the next 3-5 days. (Today: 11/14/2025)

---

## Quick Start

```bash
# Initialize
cargo rail init

# Graph-aware CI
cargo rail graph affected --since origin/main
cargo rail graph test --since origin/main

# Split/sync
cargo rail split my-crate --apply
cargo rail sync my-crate --apply

# Enforce policies
cargo rail lint deps --fix --apply
cargo rail lint versions --json

# Release orchestration
cargo rail release plan my-crate
cargo rail release apply my-crate --dry-run
```

---

## Four Pillars

### 1. Graph Orchestration ✅

Test only what changed. Stop wasting CI time.

```bash
cargo rail graph affected --since origin/main --format json
cargo rail graph test --since origin/main
cargo rail graph check --workspace
cargo rail graph clippy --since origin/main
```

### 2. Split/Sync ✅

Split crates to standalone repos with full history. Sync bidirectionally.

```bash
cargo rail split my-crate --apply
cargo rail sync my-crate --apply
cargo rail sync my-crate --apply --from-remote  # PR branch for external contributions
```

### 3. Policy & Linting ✅

Enforce consistency. Prevent dependency drift.

```bash
cargo rail lint deps --fix --apply          # Workspace inheritance
cargo rail lint versions --strict           # Duplicate versions
cargo rail lint manifest                    # Edition, MSRV, patch/replace
```

### 4. Release Orchestration ✅

Plan releases with conventional commits. Coordinate mono + split repos.

```bash
cargo rail release plan --all
cargo rail release apply my-crate --dry-run
```

---

## Commands

### Graph

```bash
cargo rail graph affected       # Show affected crates
cargo rail graph test          # Smart test targeting
cargo rail graph check         # Smart cargo check
cargo rail graph clippy        # Smart clippy
```

**Flags:** `--since <ref>`, `--workspace`, `--dry-run`, `--format json|names`

### Split/Sync

```bash
cargo rail init                # Initialize rail.toml
cargo rail split <name>        # Split with history
cargo rail sync <name>         # Bidirectional sync
cargo rail sync --all          # Sync all splits
```

**Flags:** `--apply` (default: dry-run), `--json`, `--from-remote`

### Lint

```bash
cargo rail lint deps           # Workspace inheritance
cargo rail lint versions       # Duplicate versions
cargo rail lint manifest       # Quality checks
```

**Flags:** `--fix --apply`, `--json`, `--strict`

### Release

```bash
cargo rail release plan        # Analyze commits
cargo rail release apply       # Bump, tag, sync
```

**Flags:** `--all`, `--json`, `--dry-run`

### Inspect

```bash
cargo rail status              # Show all splits
cargo rail doctor              # Health checks
cargo rail mappings <name>     # Commit mappings
```

---

## Configuration

`rail.toml`:

```toml
[workspace]
root = "."

# Split config
[[splits]]
name = "my-crate"
remote = "git@github.com:you/my-crate.git"
branch = "main"
mode = "single"  # or "combined"
paths = [{ crate = "crates/my-crate" }]

# Policy enforcement
[policy]
edition = "2024"
msrv = "1.76.0"
resolver = "2"
forbid_multiple_versions = ["tokio", "serde"]
forbid_patch_replace = true

# Release tracking
[[releases]]
name = "my-crate"
crate = "crates/my-crate"
split = "my-crate"  # optional link to [[splits]]
last_version = "0.3.1"
last_sha = "abc123"
last_date = "2025-01-15T00:00:00Z"
```

---

## Architecture

- **WorkspaceGraph** - cargo_metadata + petgraph
- **AffectedAnalysis** - File changes → crate impact
- **SystemGit** - Zero-dependency git via system binary
- **Plan** - Auditable dry-run with SHA IDs
- **MappingStore** - Git-notes commit mapping (rebase-safe)

**Dependencies:** cargo_metadata, petgraph, toml_edit, clap, serde. No libgit2/gitoxide by design; no guppy by design.

---

## Security

**Split → Mono:** Creates PR branch `rail/sync/{name}/{timestamp}`. Never commits to main directly.

**Mono → Split:** Direct push with SSH auth. Use deploy keys + branch protection.

---

## Use Cases

**Open-source subset:** Work in monorepo, auto-sync to public repos.

**CI optimization:** Test 5 affected crates instead of 50. 10x faster.

**Policy enforcement:** Uniform edition, MSRV, dependency versions across 50+ crates.

**Release coordination:** Version bump + tag + changelog across mono + split repos.
