# cargo-rail

**Rust monorepo orchestration. Zero waste.**

---

## What It Does

### 1. Replace cargo-hakari

```bash
cargo rail unify
```

Native `[workspace.dependencies]` unification. No workspace-hack crates. No guppy. No maintenance.

### 2. Distribute Crates

```bash
cargo rail split my-crate
cargo rail sync my-crate
```

Split monorepo crates to standalone repos. Sync bidirectionally. Full git history preserved.

**Replaces:** Copybara, git-filter-repo, bash scripts

### 3. Test Only What Changed

```bash
cargo rail test
```

Graph-aware change detection. Docs-only changes skip tests. Source changes test affected crates only.

**Nextest/Cargo native. Saves CI costs.**

### 4. Release & Changelog

```bash
cargo rail release my-crate
```

Validates edition, MSRV, dependencies. Publishes to crates.io. Tags releases. Generates changelogs.

---

## Why It Matters

| Problem | cargo-rail |
|---------|------------|
| Fragmented features → slow builds | Native workspace unification |
| Distributing crates from monorepo | Split/sync with full history |
| Testing everything on every PR | Graph-aware test runner |
| Version drift across workspace | Enforced policies |

---

## Install

```bash
cargo install cargo-rail
```

---

## Quick Start

```bash
# Unify dependencies
cargo rail unify

# Split a crate (one-time)
cargo rail split my-crate

# Sync changes
cargo rail sync my-crate

# Test what changed
cargo rail test

# Release a crate
cargo rail release my-crate
```

---

## Configuration

```toml
# rail.toml
[[splits]]
name = "my-crate"
remote = "git@github.com:user/my-crate.git"
paths = [{ crate = "crates/my-crate" }]

[policy]
edition = "2024"
msrv = "1.83.0"
forbid_multiple_versions = ["tokio"]
```

---

## How It Works

- **Zero libgit2/gitoxide** - System git only
- **Zero guppy** - cargo_metadata + petgraph
- **Graph-first** - Dependency graph drives everything
- **Minimal deps** - Supply-chain security matters
- **Deterministic** - Same input = same git SHAs

---

## Safety

- Syncs from remote create PR branches, never commit to main
- Destructive operations require explicit flags
- Backups created automatically

---

## Status

**Beta:**

- ✅ Dependency unification
- ✅ Split/sync with history preservation
- ✅ Graph-aware change detection

**Coming soon:**

- 🚧 Release/publish/changelog automation

---

## License

MIT OR Apache-2.0
