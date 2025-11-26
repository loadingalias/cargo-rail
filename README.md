# cargo-rail

**Zero-waste orchestration for Rust monorepos.**

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust Version](https://img.shields.io/badge/rust-1.89%2B-orange.svg)](https://www.rust-lang.org)

Stop wasting CI time. Stop fighting workspace dependencies. Stop copy-pasting git history.

```bash
cargo install cargo-rail
```

---

## What It Does

**cargo-rail** solves the three hardest problems in Rust monorepos:

| Problem | Solution | Impact |
|---------|----------|--------|
| **Testing everything on every PR** | Graph-aware change detection | **10x faster CI** |
| **Dependency fragmentation** | Resolution-based unification | **Zero workspace-hack crates** |
| **Distributing crates** | History-preserving split/sync | **One command, full git history** |

### Replaces

| Tool | What cargo-rail does instead |
|------|------------------------------|
| **cargo-hakari** | Native `workspace.dependencies` unification—no workspace-hack crate |
| **cargo-release / release-plz** | Dependency-ordered releases with native changelog generation |
| **git-cliff** | Built-in conventional commit parsing (zero-copy, no external dep) |
| **Copybara** | Deterministic split/sync with git-notes mapping |
| **CI shell scripts** | Graph-aware `cargo rail affected` with nextest/bacon integration |

---

## Quick Start

### 1. Smart Testing (10x faster CI)

```bash
# Only test what changed
cargo rail test

# Explain why each crate is tested
cargo rail test --explain
```

**Before:**

```yaml
- run: cargo test --workspace  # Tests everything (12 minutes)
```

**After:**

```yaml
- run: cargo rail test --nextest  # Tests only affected crates (1 minute)
```

### 2. Dependency Unification (replaces cargo-hakari)

```bash
# Preview unification changes
cargo rail unify --check

# Apply unification to workspace.dependencies
cargo rail unify
```

**What it does:**

- Uses Cargo's actual dependency **resolution**, not just syntax
- Merges feature sets intelligently from resolved features
- No workspace-hack crate needed
- Lossless TOML editing (preserves comments & formatting)

### 3. Crate Distribution (replaces Copybara)

```bash
# Split a crate to standalone repo with full git history
cargo rail split my-crate

# Bidirectional sync
cargo rail sync my-crate
```

**What it does:**

- Preserves complete git history (commits, tags, blame)
- Deterministic: same input = same commit SHAs
- Uses git-notes for rebase-safe commit mapping
- No libgit2/gitoxide dependency (pure system git)

---

## Why cargo-rail?

### Resolution-Based Unification

Unlike cargo-hakari's syntax-only approach, cargo-rail uses Cargo's actual resolver:

```toml
# crate-a
tokio = "1.0"

# crate-b
tokio = "^1.5"

# Both resolve to tokio 1.5.3 → compatible!
# cargo-rail merges them intelligently:
[workspace.dependencies]
tokio = { version = "1.5", features = ["rt", "net", "sync"] }
```

**No workspace-hack crate.** Just native `workspace.dependencies`.

### Graph-Aware Change Detection

```bash
$ cargo rail affected --explain

Changed files:
  crates/core/src/lib.rs (source)
  docs/guide.md (docs)

Affected crates:
  ✓ core (direct change)
  ✓ api (depends on core)
  ✓ cli (depends on api)

Skipping:
  ✗ examples (no dependency path)
  ✗ docs (docs-only change)

Running: cargo nextest run -p core -p api -p cli
```

**Real impact:** 70-90% reduction in test time on large workspaces.

### Deterministic Git Operations

Uses system git directly for maximum fidelity:

```bash
# Split preserves full history
git log --oneline
abc123 feat: add new feature
def456 fix: critical bug
...

# Same commits, same SHAs in split repo
cd ../my-crate-standalone
git log --oneline
abc123 feat: add new feature  # Same SHA!
def456 fix: critical bug      # Same SHA!
```

---

## Architecture

### Zero Abstraction

- **System git only** – No libgit2/gitoxide dependency overhead
- **Direct graph construction** – `cargo_metadata` + `petgraph` (no guppy)
- **11 core dependencies** – Minimal, auditable supply chain

### Design Principles

1. **Single workspace load** – Build context once, reuse everywhere
2. **Graph-first** – All operations use the dependency graph
3. **Lossless editing** – Preserve formatting, comments, structure
4. **Deterministic** – Same input always produces same output

---

## Configuration

Create `.config/rail.toml`:

```toml
# Toolchain management
[toolchain]
channel = "stable"
components = ["clippy", "rustfmt"]

# Dependency unification
[unify]
consolidate_transitive_features = false  # Consolidate transitive deps
transitive_feature_host = "auto"         # Smart auto-selection
validate_targets = []

# Split/sync (optional)
[[splits]]
name = "my-crate"
remote = "git@github.com:org/my-crate.git"
mode = "single"
```

See [`.config/rail.toml.example`](.config/rail.toml.example) for all options.

---

## Commands

### Change Analysis

```bash
cargo rail affected                    # Show affected crates
cargo rail affected --since main       # Compare against branch
cargo rail test                        # Test affected crates
cargo rail test --nextest --watch      # Watch mode with nextest
```

### Dependency Unification

```bash
cargo rail unify --check             # Preview changes
cargo rail unify                       # Apply unification
```

### Crate Distribution

```bash
cargo rail split my-crate              # Split to standalone repo
cargo rail sync my-crate               # Bidirectional sync
cargo rail status                      # Show sync status
```

### Release Automation (replaces cargo-release + git-cliff)

```bash
# Plan releases with dependency-aware ordering
cargo rail release --all --bump minor --check

# Execute: bump versions, generate changelogs, tag, publish
cargo rail release --all --bump patch --execute
```

**What it does:**

- Native changelog generation from conventional commits (no git-cliff dependency)
- Dependency-ordered publishing (deps before dependents)
- Configurable delays between publishes (avoids registry race conditions)
- GitHub release creation via `gh` CLI (optional)
- GPG/SSH tag signing support

### Watch Mode (bacon/cargo-watch integration)

```bash
# Auto-detect best watcher and run smart tests
cargo rail test --watch

# Explicit watcher selection
cargo rail test --watch --watch-mode bacon
```

---

## Comparison

### vs Dependency Management

| Feature | cargo-rail | cargo-hakari |
|---------|-----------|-------------|
| Dependency unification | ✅ Resolution-based | ✅ Syntax-only |
| Feature selection | Intersection (minimal) | Union (maximal) |
| Workspace-hack crate | ❌ Not needed | ✅ Required |
| TOML preservation | ✅ Lossless | ⚠️ Regenerates |
| Dependencies | 10 core | ~40+ (via guppy) |

### vs Release Automation

| Feature | cargo-rail | cargo-release | release-plz |
|---------|-----------|--------------|-------------|
| Version bumping | ✅ | ✅ | ✅ |
| Changelog generation | ✅ Native | ❌ External | ✅ |
| Dependency ordering | ✅ Graph-aware | ⚠️ Manual | ✅ |
| GitHub releases | ✅ via gh | ❌ | ✅ |
| Publish delay | ✅ Configurable | ❌ | ✅ |
| Tag signing | ✅ | ✅ | ✅ |
| Dependencies | 90 |-| 600+|

### vs Monorepo Sync

| Feature | cargo-rail | Copybara | git-filter-repo |
|---------|-----------|----------|-----------------|
| Git history preservation | ✅ Full | ⚠️ Partial | ✅ Full |
| Bidirectional sync | ✅ | ✅ | ❌ One-way |
| Deterministic SHAs | ✅ | ❌ | ✅ |
| Conflict resolution | ✅ 3-way merge | ✅ | ❌ |
| Commit mapping | ✅ git-notes | Custom DB | ❌ |
| Language | Rust | Go | Python |

### vs CI Optimization

| Feature | cargo-rail | Custom scripts | nx/turborepo |
|---------|-----------|---------------|--------------|
| Change detection | ✅ Graph-aware | ⚠️ Path-based | ✅ Graph-aware |
| Rust-native | ✅ | ⚠️ Shell | ❌ JS ecosystem |
| Nextest integration | ✅ Auto-detect | Manual | ❌ |
| Watch mode | ✅ bacon/cargo-watch | Manual | ✅ |
| Dependency resolution | ✅ Cargo's resolver | ❌ | ❌ |

---

## Real-World Impact

### Before

```bash
# CI takes 12 minutes testing entire workspace
# Maintain workspace-hack crate manually
# Use git-filter-repo scripts for splits
```

### After

```bash
# CI takes 1-2 minutes testing only affected crates
# cargo rail unify (one command)
# cargo rail split my-crate (one command, full history)
```

---

## Installation

```bash
# From crates.io
cargo install cargo-rail

# From source
git clone https://github.com/loadingalias/cargo-rail
cd cargo-rail
cargo install --path .
```

---

## Development

```bash
# Build
cargo build --release

# Test
cargo test

# Quality checks
just check
```

---

## Contributing

Contributions welcome! Please:

1. Open an issue first for major changes
2. Run `just check` before submitting
3. Add tests for new features
4. Update docs as needed

---

## License

MIT License - see [LICENSE](LICENSE) for details.

---

## Credits

Built with [cargo_metadata](https://crates.io/crates/cargo_metadata), [petgraph](https://crates.io/crates/petgraph), [toml_edit](https://crates.io/crates/toml_edit), and [clap](https://crates.io/crates/clap).

Inspired by cargo-hakari's unification approach and Copybara's monorepo concepts.

---

**Ready to stop wasting time?** `cargo install cargo-rail`
