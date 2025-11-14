# cargo-rail

**The opinionated Rust workspace orchestration tool for teams building at scale.**

Manage large Cargo workspaces with confidence: split crates to standalone repos, sync bidirectionally, run graph-aware CI, enforce workspace policies, and orchestrate releases—all from one tool.

---

## Status: Active Development

**NOT READY FOR PUBLIC USE** - Internal tooling in active development.

| Pillar | Status | Description |
|--------|--------|-------------|
| **1. Graph Orchestration** | ✅ **Complete** | Affected analysis, smart test/check/clippy |
| **2. Split/Sync** | ✅ **Production** | Bidirectional mono↔split with history preservation |
| **3. Policy & Linting** | 🟡 **Planned** | Workspace consistency, dependency linting |
| **4. Release Orchestration** | 🟡 **Planned** | Coordinated releases, changelogs, publishing |

**Current Focus:** Building Pillar 3 (Policy & Linting)

---

## Philosophy

**Problem:** Large Rust workspaces require bespoke bash scripts for:
- Determining which crates changed (CI optimization)
- Splitting crates for OSS while maintaining mono workflow
- Enforcing dependency policies across 50+ crates
- Coordinating releases between mono and split repos

**Solution:** One opinionated tool that does it right.

**Principles:**
- **Own your abstractions** - Build on cargo_metadata + petgraph, not wrappers
- **Dry-run first** - Show plan, require `--apply` for execution
- **Graph-aware** - Workspace dependencies drive all decisions
- **Zero magic** - Transparent git operations, auditable plans
- **Team-first** - Built for CI, designed for multiple developers

---

## Four Pillars

### 1. Graph-Aware Work Orchestration ✅

Run tests, checks, and lints only on affected crates. Stop wasting CI time.

```bash
# See what's affected since origin/main
cargo rail graph affected --since origin/main --format json

# Run tests only for affected crates + dependents
cargo rail graph test --since origin/main

# Or everything
cargo rail graph test --workspace
```

**Features:**
- Dependency graph analysis (direct + transitive)
- Change detection via git integration
- Smart test targeting (minimal or full workspace)
- Cycle detection (via `cargo rail doctor`)
- Multiple output formats (text, JSON, names-only)

**Impact:** 10x faster CI for workspaces with 20+ crates.

---

### 2. Split/Sync ✅

Split crates from monorepo to standalone repos with full git history. Sync bidirectionally.

```bash
# One-time split with history
cargo rail split my-crate --apply

# Ongoing sync (mono → split)
cargo rail sync my-crate --apply

# External contributions (split → mono via PR branch)
cargo rail sync my-crate --apply --from-remote
```

**Features:**
- Full git history preservation
- Two modes: single crate or combined multi-crate
- Cargo.toml transforms (path deps ↔ version deps)
- Git-notes commit mapping (rebase-tolerant)
- Security model: PR branches for split→mono (no direct commits)
- Conflict resolution (ours, theirs, manual, union)

**Use Case:** Open-source 5 crates from 25-crate proprietary monorepo while maintaining mono workflow.

---

### 3. Workspace Policy & Linting 🟡 (Planned)

Enforce consistency and prevent dependency drift.

```bash
# Detect workspace deps not using inheritance
cargo rail lint deps

# Find duplicate dependency versions
cargo rail lint versions

# Auto-fix issues
cargo rail lint deps --fix
```

**Planned Features:**
- Policy enforcement (edition, MSRV, resolver version)
- Workspace dependency linting
- Duplicate version detection
- Manifest quality checks (patch/replace, feature bleed)
- Auto-fix support

---

### 4. Release & Publishing Orchestration 🟡 (Planned)

Coordinate releases across monorepo and split repos.

```bash
# Plan releases for all channels
cargo rail release plan --all

# Apply release: version bump, changelog, tags
cargo rail release apply my-crate

# Publish to crates.io
cargo rail release apply my-crate --publish
```

**Planned Features:**
- Release channels (crates vs products)
- Conventional commit parsing
- Changelog generation + sync
- Dependency-aware publish order
- Split repo coordination

---

## Installation

**Internal use only - not on crates.io yet**

```bash
# From source
git clone <internal-repo>
cd cargo-rail
cargo install --path .
```

---

## Quick Start

### 1. Initialize Configuration

```bash
cd your-workspace/
cargo rail init
```

Creates `rail.toml`:

```toml
[workspace]
root = "/absolute/path/to/workspace"

# Optional: configure splits
[[splits]]
name = "my-crate"
remote = "git@github.com:you/my-crate.git"
branch = "main"
mode = "single"
paths = [{ crate = "crates/my-crate" }]
```

---

### 2. Graph-Aware CI (Pillar 1)

```bash
# What changed since last deploy?
cargo rail graph affected --since origin/main

# Test only affected crates
cargo rail graph test --since origin/main --dry-run
cargo rail graph test --since origin/main  # execute

# CI usage: names-only for matrix builds
cargo rail graph affected --since origin/main --format names-only > affected.txt
```

**CI Integration:**

```yaml
# .github/workflows/test.yml
- name: Test affected crates
  run: |
    cargo rail graph test --since origin/main --format json > test-plan.json
    # Use test-plan.json for matrix builds
```

---

### 3. Split/Sync Workflow (Pillar 2)

**Initial Split:**

```bash
# Dry-run first (shows plan)
cargo rail split my-crate

# Execute split
cargo rail split my-crate --apply
```

**Ongoing Sync (Monorepo → Split):**

```bash
# After making changes in monorepo
git commit -am "feat: Add feature"
cargo rail sync my-crate --apply
# Pushes to split repo automatically
```

**External Contributions (Split → Monorepo):**

```bash
# PR comes into split repo
cargo rail sync my-crate --apply --from-remote
# Creates PR branch: rail/sync/my-crate/1234567890
# Review and merge manually (security)
```

**Combined Mode (Multiple Crates → One Repo):**

```toml
[[splits]]
name = "my-tools"
mode = "combined"
paths = [
  { crate = "crates/tool-a" },
  { crate = "crates/tool-b" }
]
```

---

## Commands Reference

### Graph Orchestration

```bash
cargo rail graph affected    # Show affected crates
cargo rail graph test        # Run tests (affected or --workspace)
cargo rail graph check       # Run cargo check
cargo rail graph clippy      # Run clippy
```

### Split/Sync

```bash
cargo rail init              # Initialize configuration
cargo rail split <name>      # Split crate(s) with history
cargo rail sync <name>       # Sync changes (bi-directional)
cargo rail sync --all        # Sync all configured splits
```

### Inspection & Health

```bash
cargo rail status            # Show all configured splits
cargo rail doctor            # Health checks (git, SSH, workspace, cycles)
cargo rail mappings <name>   # Inspect git-notes commit mappings
```

### Common Flags

- `--apply` - Execute action (default: dry-run)
- `--dry-run` - Show plan without executing
- `--json` - Output structured JSON
- `--workspace` - Run on entire workspace (graph commands)
- `--since <ref>` - Git reference for change detection

---

## Architecture

### System Design

```
┌─────────────────────────────────────────────────────────┐
│                    CARGO WORKSPACE                       │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  │
│  │  crate-core  │  │ crate-client │  │ crate-server │  │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘  │
│         │                 │                  │           │
│         └─────────────────┴──────────────────┘           │
│                           │                              │
│                    WorkspaceGraph                        │
│                  (petgraph + cargo_metadata)             │
└─────────────────────────────────────────────────────────┘
         │                  │                  │
    ┌────┴────┐        ┌────┴────┐       ┌────┴────┐
    │ Pillar 1│        │ Pillar 2│       │ Pillar 3│
    │  Graph  │        │  Split/ │       │ Policy/ │
    │  Aware  │        │  Sync   │       │  Lint   │
    └─────────┘        └─────────┘       └─────────┘
         │                  │                  │
    CI Optimization    OSS Workflow     Consistency
    (test/check)      (git operations)  (enforcement)
```

### Core Abstractions

- **WorkspaceGraph** - Dependency graph built from cargo_metadata
- **AffectedAnalysis** - File changes → crate impact + dependents
- **SystemGit** - Zero-dependency git operations via system binary
- **Plan** - Auditable dry-run with SHA-based IDs
- **MappingStore** - Git-notes based commit mapping (rebase-safe)

---

## Development Roadmap

See [TODO.md](TODO.md) for detailed roadmap.

**Completed:**
- ✅ Pillar 1: Graph-Aware Work Orchestration (affected, test, check, clippy)
- ✅ Core split/sync functionality
- ✅ Bidirectional sync with conflict resolution
- ✅ Combined mode (multi-crate splits)
- ✅ Cycle detection
- ✅ Git-notes mapping

**In Progress:**
- 🟡 Pillar 3: Policy & Linting

**Planned:**
- ⏳ Pillar 4: Release & Publishing Orchestration
- ⏳ Enhanced split/sync workflows (branch awareness, CI primitives)

---

## Technical Details

### Dependencies

**Minimal, intentional:**
- `cargo_metadata` - Workspace introspection
- `petgraph` - Graph algorithms
- `toml_edit` - Lossless TOML manipulation
- `clap` - CLI parsing
- `serde` - Config serialization
- System git binary (no libgit2/gitoxide)

**Why system git?**
- Zero git library dependencies (lighter, faster builds)
- Direct control over operations
- Easier debugging (can see exact git commands)
- Leverages user's git config (SSH keys, credentials)

### Code Quality

- **10,200+ lines of Rust**
- **71 tests** (52 unit + 19 integration)
- **Zero compiler warnings** (strict clippy)
- **cargo deny** passing (license + security audit)
- **Documentation** builds without warnings

---

## Security Model

### Split → Monorepo (External Contributions)

**NEVER commits directly to main/master.**

1. Creates PR branch: `rail/sync/{name}/{timestamp}`
2. Applies commits from split repo
3. Prints review instructions
4. Requires manual merge

**Protection:**
- Enable branch protection on monorepo main
- Require PR reviews for `rail/sync/*` branches
- Use deploy keys (write access to split repos only)

### Monorepo → Split

- Direct push to configured branch
- SSH key authentication required
- Recommended: branch protection + CI on split repo

---

## Use Cases

### 1. Open-Source Subset of Monorepo

**Scenario:** 25-crate proprietary monorepo, want to open-source 5 crates

```bash
# Split to standalone repos
cargo rail split http-client --apply
cargo rail split json-parser --apply

# Continue working in monorepo
cd monorepo/crates/http-client
# ... make changes ...
git commit -am "feat: Add retry logic"

# Sync to public repo
cargo rail sync http-client --apply
```

**Result:** Public sees normal standalone crate. You work in monorepo.

---

### 2. CI Optimization

**Scenario:** 50-crate workspace, CI runs all tests on every PR (slow)

```bash
# In CI: test only affected crates
cargo rail graph test --since origin/main --format json
```

**Impact:** 10x faster CI (5 crates tested instead of 50)

---

### 3. Coordinated Releases (Future)

**Scenario:** Release a product that spans 3 workspace crates

```toml
[release.channels.my-product]
kind = "product"
root_crate = "crates/my-app"
exports = ["http-client", "json-parser"]
```

```bash
cargo rail release plan my-product
cargo rail release apply my-product --publish
```

**Result:** Coordinated version bumps, changelogs, and publishes.

---

## Why cargo-rail?

**vs git-subtree:** Bidirectional + Cargo-aware transforms
**vs Copybara:** Simpler (one TOML vs Starlark), Rust-native
**vs cargo-workspaces:** Adds split/sync + graph orchestration
**vs Nx/Turborepo:** Rust-native, Cargo-first, opinionated
**vs Manual bash scripts:** Type-safe, tested, comprehensive

**cargo-rail is the missing piece for Rust teams working at scale.**

---

## Contributing

**Internal use only** - not accepting external contributions yet.

See [TODO.md](TODO.md) for planned features and roadmap.

---

## License

MIT - see [LICENSE](LICENSE)

---

## Acknowledgments

- Inspired by Google's Copybara and Nx's graph-aware CI
- Built on Rust's excellent ecosystem (cargo_metadata, petgraph, clap)
- Philosophy: Own your abstractions, minimize dependencies, be opinionated

---

**Built with Rust for Rust teams working at scale.**
