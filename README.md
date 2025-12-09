<p align="center">
  <img src="https://socialify.git.ci/loadingalias/cargo-rail/image?font=Jost&language=1&name=1&owner=1&pattern=Solid&theme=Auto" alt="cargo-rail" width="640" height="320" />
</p>

<h3 align="center">Graph-aware monorepo tooling for Rust</h3>

<p align="center">
  <strong>Test only what changed. Keep dependencies unified. Split crates cleanly. Release in order.</strong>
</p>

<p align="center">
  <a href="https://crates.io/crates/cargo-rail"><img src="https://img.shields.io/crates/v/cargo-rail.svg" alt="Crates.io"></a>
  <a href="https://crates.io/crates/cargo-rail"><img src="https://img.shields.io/crates/d/cargo-rail.svg" alt="Downloads"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.91%2B-orange.svg" alt="Rust 1.91+"></a>
</p>

<p align="center">
  <a href="#install">Install</a> •
  <a href="#why-cargo-rail">Why</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#workflows">Workflows</a> •
  <a href="#real-world-results">Results</a> •
  <a href="https://github.com/loadingalias/cargo-rail-action">GitHub Action</a>
</p>

---

## What It Does

cargo-rail is a single tool that replaces a fragmented ecosystem:

| Problem | Before | After |
|---------|--------|-------|
| **Build graph drift** | `cargo-hakari`, workspace-hack crates | `cargo rail unify` |
| **Unused deps** | `cargo-udeps`, `cargo-machete`, `cargo-shear` | `cargo rail unify` |
| **MSRV guessing** | `cargo-msrv`, compile-and-fail loops | `cargo rail unify` |
| **CI waste** | `paths-filter` + 1k LoC shell | `cargo rail test` |
| **Crate extraction** | `git subtree`, Copybara (Java) | `cargo rail split` |
| **Release chaos** | `release-plz` (600+ deps), `git-cliff` | `cargo rail release` |

**11 core dependencies. 77 resolved. One config file.**

---

## Install

```bash
cargo install cargo-rail
```

Pre-built binaries: [Releases](https://github.com/loadingalias/cargo-rail/releases) • `cargo binstall cargo-rail`

---

## Quick Start

Drop into any Rust workspace:

```bash
cargo rail init              # Generate .config/rail.toml
cargo rail unify --check     # Preview dependency unification
cargo rail unify             # Apply (auto-backups on first run)
cargo rail test              # Test only affected crates
```

That's it. Your workspace is now unified, and you're only testing what changed.

---

## Why cargo-rail

Most Rust monorepo tools solve one problem and introduce their own complexity. You end up with:

- 5+ config files across different tools
- Each tool parses `cargo metadata` differently
- No single source of truth for your dependency graph
- Shell scripts gluing everything together

cargo-rail takes a different approach:

- **One tool, one config** — `rail.toml` describes everything
- **Resolution-based** — Uses what Cargo actually resolved, not syntax parsing
- **Multi-target aware** — Runs `cargo metadata --filter-platform` per target in parallel
- **Minimal footprint** — 11 core deps vs 200-600+ in competing toolchains

---

## Workflows

### Change Detection

Graph-aware. Only test what's affected by your changes:

```bash
cargo rail affected              # Show affected crates
cargo rail affected -f names-only  # Just names (for scripting)
cargo rail test                  # Run tests for affected crates only
cargo rail test --all            # Override: test everything
```

**For CI:** Use [`cargo-rail-action`](https://github.com/loadingalias/cargo-rail-action):

```yaml
- uses: loadingalias/cargo-rail-action@v1
  id: rail
  with:
    since: ${{ github.event.before }}

- run: cargo nextest run -p ${{ steps.rail.outputs.crates }}
  if: steps.rail.outputs.docs-only != 'true'
```

### Dependency Unification

Keep your workspace lean. Per target triple:

```bash
cargo rail unify --check    # Preview (CI-safe, exits 1 if changes needed)
cargo rail unify            # Apply changes
cargo rail unify sync       # Re-detect targets after adding new platforms
cargo rail unify undo       # Restore from backup
```

What it does:

- **Unifies versions** based on Cargo's resolver output
- **Computes MSRV** from the dependency graph
- **Prunes dead features** that are never enabled
- **Detects unused deps** (opt-in)
- **Pins transitives** — replaces `cargo-hakari` without a workspace-hack crate

### Split & Sync

Extract crates with full git history. Keep them in sync:

```bash
cargo rail split init my_crate      # Configure
cargo rail split run my_crate       # Extract with history
cargo rail sync my_crate            # Bidirectional sync
cargo rail sync my_crate --to-remote    # Push to split repo
cargo rail sync my_crate --from-remote  # Pull (creates PR branch)
```

Three modes: single crate → new repo, multiple crates → new repo, or multiple crates → new workspace.

### Release

Dependency-order publishing with changelogs:

```bash
cargo rail release init my_crate        # Configure
cargo rail release check my_crate       # Validate
cargo rail release run my_crate --bump minor  # Execute
```

---

## Real-World Results

Tested on production Rust workspaces:

| Repo | Members | Deps Unified | Notes |
|------|---------|--------------|-------|
| **[tikv](https://github.com/loadingalias/tikv)** | 72 | 61 | Largest stress test |
| **[meilisearch](https://github.com/loadingalias/meilisearch)** | 19 | 46 | Significant unification |
| **[helix-db](https://github.com/loadingalias/helix-db)** | 6 | 18 | Growing project |
| **[helix](https://github.com/loadingalias/helix)** | 12 | 16 | Editor workspace |
| **[tokio](https://github.com/loadingalias/tokio)** | 10 | 10 | Core ecosystem |
| **[ripgrep](https://github.com/loadingalias/ripgrep)** | 10 | 9 | CLI baseline |
| **[polars](https://github.com/loadingalias/polars)** | 8 | 2 | Already clean |
| **[ruff](https://github.com/loadingalias/ruff)** | 43 | 0 | Already unified |
| **[codex](https://github.com/loadingalias/codex)** | 49 | 0 | Already unified |

Each link above points to a fork with cargo-rail configured. Clone and compare.

**Demo videos:** [`examples/`](examples/)

---

## Migrating from cargo-hakari?

5 minutes:

```bash
git checkout -b migrate-to-rail
rm -rf crates/workspace-hack    # Remove the hack crate
cargo rail init
# Edit rail.toml: set pin_transitives = true
cargo rail unify
cargo check --workspace
```

Full guide: [docs/migrate-hakari.md](docs/migrate-hakari.md)

---

## Design Decisions

**Multi-Target Resolution** — Most tools run `cargo metadata` once. cargo-rail runs it per target in parallel, computes feature *intersections* (not unions). If something is marked unused, it's unused across all your targets.

**System Git** — Uses your `git` binary directly. No `libgit2`, no `gitoxide`. Real git, real history, deterministic SHAs.

**Lossless TOML** — Uses `toml_edit` to preserve comments and formatting. Your manifests stay readable.

**Supply-Chain Safety** — 11 core dependencies. I built the release workflow specifically because I was uncomfortable with 600+ deps for release automation.

---

## Documentation

- [Command Reference](docs/commands.md)
- [Config Reference](docs/config.md)
- [Migration Guide](docs/migrate-hakari.md)
- [Demo Videos](examples/)

---

## Contributing

Issues, PRs, and feedback welcome. This is built for the Rust community.

If cargo-rail helps your workflow, consider [starring the repo](https://github.com/loadingalias/cargo-rail) — it helps others find it.

---

<p align="center">
  <a href="https://github.com/loadingalias/cargo-rail">GitHub</a> •
  <a href="https://crates.io/crates/cargo-rail">Crates.io</a> •
  <a href="https://github.com/loadingalias/cargo-rail-action">GitHub Action</a>
</p>

<p align="center">
  <sub>Built by <a href="https://github.com/loadingalias">@loadingalias</a></sub>
</p>
