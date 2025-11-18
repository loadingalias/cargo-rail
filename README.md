# cargo-rail

**Hyper-efficient Rust monorepo orchestration.**

---

## Status

**NOT READY FOR PUBLIC USE.** Active development. Internal testing only.

---

## What It Does

### 1. Workspace Dependency Unification

**Replaces cargo-hakari and workspace-hack crates.**

- Native Cargo `[workspace.dependencies]` unification
- No fake crates, no guppy, no runtime overhead
- CI-enforceable via `cargo rail unify check`
- Invisible after initial application

```bash
cargo rail unify analyze              # Dry-run analysis
cargo rail unify apply --backup       # Apply unification
cargo rail unify check                # CI verification (exit 0 = unified)
```

### 2. Split & Sync

**Split crates from monorepo to standalone repos. Sync bidirectionally.**

```bash
# Split: Monorepo → Standalone Repo (with full git history)
cargo rail split my-crate --apply

# Sync: Monorepo → Split Repo (direct push to main)
cargo rail sync my-crate --to-remote --apply

# Sync: Split Repo → Monorepo (creates PR branch, never commits to main)
cargo rail sync my-crate --from-remote --apply
```

**Modes:**
- **Single crate** → Single repo
- **Multiple crates** → Single repo
- **Multiple crates** → Separate monorepo

### 3. Centralized Configuration Management

**Enforce uniform policies across all workspace crates.**

- Edition, MSRV, resolver version
- Dependency version consistency
- Workspace inheritance validation

```toml
# rail.toml
[policy]
edition = "2024"
msrv = "1.76.0"
resolver = "2"
forbid_multiple_versions = ["tokio", "serde"]
```

---

## Architecture

- **Git + Cargo + Petgraph** - Core foundation
- **Zero libgit2/gitoxide** - System git binary only
- **Zero guppy** - Direct cargo_metadata usage
- **Workspace-aware graph** - Dependency + reverse-dependency tracking

---

## Configuration

`rail.toml`:

```toml
[workspace]
root = "."

# Split configuration
[[splits]]
name = "my-crate"
remote = "git@github.com:user/my-crate.git"
branch = "main"
mode = "single"
paths = [{ crate = "crates/my-crate" }]

# Unification configuration (optional)
[unify]
strategy = "all"
normal_only = false
exclude = ["some-special-dep"]

# Policy enforcement
[policy]
edition = "2024"
msrv = "1.76.0"
resolver = "2"
forbid_multiple_versions = ["tokio"]
```

---

## Security

**Split → Mono:** Creates PR branch `rail/sync/{name}/{timestamp}`. Never commits to main.

**Mono → Split:** Direct push. Use SSH keys + branch protection.

---

## License

MIT OR Apache-2.0
