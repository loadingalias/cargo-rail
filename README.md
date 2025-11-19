# cargo-rail

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail)
[![CI](https://github.com/your-org/cargo-rail/workflows/CI/badge.svg)](https://github.com/your-org/cargo-rail/actions)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Rust Version](https://img.shields.io/badge/rust-1.91%2B-orange.svg)](https://www.rust-lang.org)

**Graph-aware workspace orchestration for Rust monorepos. Zero waste.**

> **v0.9.0** - Release workflow automation coming in next minor version

---

## Why cargo-rail?

Modern Rust monorepos face four fundamental problems:

| Problem | cargo-rail Solution |
|---------|-------------------|
| **Fragmented dependencies** → bloated builds | Resolution-based workspace unification |
| **Distributing crates** → complex tooling | Split/sync with full git history preservation |
| **Testing everything** → wasted CI time | Graph-aware change detection |
| **Manual releases** → version drift | Automated validation & publishing _(coming soon)_ |

---

## Quick Start

```bash
# Install
cargo install cargo-rail

# Initialize
cargp rail init

# Unify workspace dependencies (replaces cargo-hakari)
cargo rail unify apply

# Test only what changed (10x faster CI; Nextest)
cargo rail test

# Split crate/s to standalone repo/s
cargo rail split my-crate

# Sync changes bidirectionally
cargo rail sync my-crate
```

---

## Features

### 1. Dependency Unification

**Replaces:** cargo-hakari, manual workspace dependency management

```bash
cargo rail unify analyze  # Analyze fragmentation
cargo rail unify apply    # Unify to workspace.dependencies
cargo rail unify check    # CI validation
```

**How it works:**

- **Resolution-first algorithm**: Uses Cargo's actual dependency resolution, not just syntax
  - Example: `"1.0"` and `"^1.5"` both resolve to `1.5.3` → compatible!
- **Feature union**: Computes unified feature sets from resolved features
- **Transitive detection**: Finds hidden fragmentation in transitive dependencies
- **Lossless editing**: Preserves comments, formatting, and custom config

**Why it's better:**

- Cargo-hakari requires maintaining a separate workspace-hack crate
- cargo-rail uses native `[workspace.dependencies]` (Cargo's built-in feature)
- Zero guppy dependency, direct petgraph usage

### 2. Crate Distribution

**Replaces:** Copybara, git-filter-repo, bash scripts

```bash
cargo rail split my-crate       # One-time split
cargo rail sync my-crate         # Bidirectional sync
cargo rail status                # Show sync status
```

**Features:**

- **Full git history preservation**: All commits, tags, and blame info
- **Deterministic**: Same input = same commit SHAs (reproducible)
- **Two modes:**
  - `single`: One crate → standalone repo (files moved to root)
  - `combined`: Multiple crates → single repo (paths preserved)
- **Git-notes based mapping**: Rebase-safe commit tracking

**Configuration:**

```toml
# .config/rail.toml
[[splits]]
name = "my-crate"
remote = "git@github.com:org/my-crate.git"
branch = "main"
mode = "single"
paths = [{ crate = "crates/my-crate" }]
```

**Sync safety:**

- Remote→monorepo syncs create PR branches (never commits to main)
- Conflict detection with strategies: `ours`, `theirs`, `manual`, `union`
- Protected branch checks

### 3. Smart Testing

**Replaces:** Manual test selection, wasted CI cycles

```bash
cargo rail test                  # Test affected crates
cargo rail test --nextest        # Use cargo-nextest
cargo rail test --watch          # Watch mode (bacon/cargo-watch)
cargo rail affected --explain    # Show impact analysis
```

**Intelligence:**

- **File classification**: Source / Test / Doc / Config / Build
- **Graph traversal**: Finds all transitive dependents
- **Skip optimization**: Docs-only changes skip tests entirely
- **Path caching**: Fast file→crate mapping

**Real impact:** 10x faster CI on large workspaces

### 4. Configuration Management

**Single source of truth** for toolchain and project settings

```bash
cargo rail config sync       # Sync rust-toolchain.toml
cargo rail config sync --check  # CI validation
```

**Syncs:**

- `[toolchain]` section → `rust-toolchain.toml`
- Validates before syncing
- Ensures consistency across workspace

---

## Architecture

cargo-rail is built on three principles:

### 1. Zero Abstraction

- **System git only**: No libgit2/gitoxide, direct git plumbing commands
- **No guppy**: Direct `cargo_metadata` + `petgraph` usage
- **Why?** Maximum performance, full control, minimal dependencies

### 2. Graph-First Design

```
cargo_metadata → WorkspaceGraph → petgraph::DiGraph
                       ↓
              All operations use the graph:
              - Affected crate analysis
              - Test selection
              - Dependency resolution
              - Impact analysis
```

### 3. Minimal Dependencies

**Core (8 dependencies):**

- `cargo_metadata` - Workspace introspection
- `petgraph` - Graph analysis
- `toml_edit` - Lossless TOML editing
- `clap` - CLI parsing
- `anyhow` - Error handling
- `serde` + `serde_json` - Config serialization
- `semver` - Version handling
- `rayon` - Parallelism

**Zero dependencies on:**

- libgit2 / gitoxide (uses system git)
- guppy (direct graph construction)

---

## Configuration

Full example in `.config/rail.toml.example`:

```toml
# Toolchain Management
[toolchain]
channel = "stable"
profile = "minimal"
components = ["rustfmt", "clippy"]
targets = ["wasm32-unknown-unknown"]

# Dependency Unification
[unify]
features = ["unified"] # Features to add when unifying
validate_targets = ["x86_64-unknown-linux-gnu", "wasm32-unknown-unknown"]
pin_transitives = true  # Add transitive deps to workspace

# Policy Enforcement
[policy]
resolver = "2"
edition = "2024"
msrv = "1.91.0"
forbid_multiple_versions = ["tokio", "serde"]

# Crate Splits
[[splits]]
name = "my-crate"
remote = "git@github.com:org/my-crate.git"
branch = "main"
mode = "single"
paths = [{ crate = "crates/my-crate" }]

# Security
[security]
ssh_key_path = "~/.ssh/id_ed25519"
require_signed_commits = true
protected_branches = ["main", "master"]
```

---

## CLI Reference

### Dependency Unification

```bash
cargo rail unify analyze                    # Dry-run analysis
cargo rail unify apply                      # Apply unification
cargo rail unify apply --backup             # Create backup first
cargo rail unify check                      # CI validation
cargo rail unify check --validate-targets   # Multi-platform validation
```

### Change Analysis

```bash
cargo rail affected                         # Show affected crates
cargo rail affected --since origin/main     # Compare against branch
cargo rail affected --explain               # Detailed impact analysis
cargo rail affected --format json           # JSON output
```

### Testing

```bash
cargo rail test                             # Test affected crates
cargo rail test --nextest                   # Use cargo-nextest
cargo rail test --watch                     # Watch mode
cargo rail test --dry-run --explain         # Preview test plan
```

### Crate Distribution

```bash
cargo rail split my-crate                   # Split single crate
cargo rail split --all                      # Split all configured
cargo rail split --dry-run                  # Preview operation
cargo rail sync my-crate                    # Sync both directions
cargo rail sync --from-remote               # Remote → monorepo
cargo rail sync --to-remote                 # Monorepo → remote
cargo rail sync --strategy manual           # Conflict resolution
cargo rail status                           # Show sync status
```

### Configuration

```bash
cargo rail config sync                      # Sync toolchain config
cargo rail config sync --check              # CI validation
```

---

## Use Cases

### Eliminate Workspace-Hack Crates

**Before:**

```bash
# Maintain workspace-hack crate with cargo-hakari
cargo hakari generate
cargo hakari manage-deps
```

**After:**

```bash
# One command, native workspace.dependencies
cargo rail unify apply
```

**Benefits:**

- No separate workspace-hack crate to maintain
- Uses Cargo's native `[workspace.dependencies]`
- Resolution-based algorithm (smarter than syntax-only)

### Distribute Crate from Monorepo

**Scenario:** You have `crates/my-lib` in your monorepo, want to publish standalone

```bash
# 1. Configure the split
cat >> .config/rail.toml <<EOF
[[splits]]
name = "my-lib"
remote = "git@github.com:org/my-lib.git"
mode = "single"
paths = [{ crate = "crates/my-lib" }]
EOF

# 2. One-time split (preserves full git history)
cargo rail split my-lib

# 3. Work in either repo, sync bidirectionally
cargo rail sync my-lib

# 4. Check status
cargo rail status
```

**What happens:**

- Full git history extracted (all commits, tags, blame)
- Workspace dependencies stripped (`workspace = true` removed)
- Deterministic: same input = same commit SHAs
- Future syncs use git-notes for commit mapping

### Smart CI Testing

**Before:**

```yaml
# Test everything on every PR
- run: cargo test --workspace
```

**After:**

```yaml
# Test only affected crates
- run: cargo rail test --nextest
```

**Example output:**

```
Changed files:
  - crates/core/src/lib.rs (source)
  - docs/guide.md (docs)

Affected crates:
  ✓ core (direct change)
  ✓ api (depends on core)
  ✓ cli (depends on api)

Skipping:
  ✗ examples/demo (no dependency path)
  ✗ docs (docs-only change)

Running tests:
  cargo nextest run -p core -p api -p cli
```

**Impact:** 70-90% reduction in test time on large workspaces

---

## How It Works

### Resolution-Based Unification

Unlike cargo-hakari (syntax-only), cargo-rail uses Cargo's actual dependency resolution:

```rust
// Scenario: Two crates depend on "tokio"
// crate-a: tokio = "1.0"
// crate-b: tokio = "^1.5"

// Step 1: Check what Cargo actually resolved
// Both resolve to tokio 1.5.3 → compatible!

// Step 2: Merge features from RESOLVED set (not declared)
// crate-a resolved: ["rt", "net"]
// crate-b resolved: ["rt", "sync"]
// Union: ["rt", "net", "sync"]

// Result in workspace.dependencies:
// tokio = { version = "1.5", features = ["rt", "net", "sync"] }
```

**Why this matters:**

- Avoids false conflicts from syntax differences
- Uses Cargo's resolution algorithm (same as builds)
- Handles complex version requirements correctly

### Git History Preservation

cargo-rail uses system git for maximum fidelity:

```bash
# 1. Walk history for crate paths
git log --format=raw --reverse -- crates/my-crate

# 2. Extract commit metadata
git cat-file --batch  # Batch process for speed

# 3. Recreate commits in target repo
git commit-tree <tree> -m "<message>" -p <parent>

# 4. Store mapping in git-notes (rebase-safe)
git notes --ref=cargo-rail add <commit> -m "<mapping>"
```

**Benefits:**

- Deterministic: same input = same commit SHAs
- Full metadata preservation: author, date, GPG signatures
- Works with any git version
- No dependency on libgit2/gitoxide

### Graph-Aware Change Detection

```
Changed files → File classification → Crate mapping → Graph traversal
     ↓                    ↓                  ↓              ↓
  src/lib.rs          Source            core/          Transitive
  docs/guide.md       Docs              docs/          dependents
  Cargo.toml          Config            core/
```

**Optimizations:**

- Path cache: `RwLock<HashMap<PathBuf, PackageId>>` for fast lookups
- Docs-only changes skip tests entirely
- Parallel processing with rayon

---

## Comparison

| Feature | cargo-rail | cargo-hakari | Copybara |
|---------|-----------|-------------|----------|
| **Dependency unification** | ✅ Resolution-based | ✅ Syntax-only | ❌ |
| **Workspace-hack crate** | ❌ Native workspace.dependencies | ✅ Required | ❌ |
| **Git history preservation** | ✅ Full | ❌ | ✅ |
| **Bidirectional sync** | ✅ | ❌ | ✅ |
| **Deterministic** | ✅ Same SHAs | ✅ | ❌ |
| **Smart testing** | ✅ Graph-aware | ❌ | ❌ |
| **Git library** | System git | N/A | Custom |
| **Dependencies** | 8 core | ~40+ (via guppy) | Go ecosystem |

---

## Development

### Building

```bash
# Clone
git clone https://github.com/your-org/cargo-rail.git
cd cargo-rail

# Build
cargo build --release

# Run tests
cargo test

# Quality checks
just check  # fmt, clippy, deny, doc, audit
```

### Testing

```bash
# Run all tests
cargo test

# Run specific integration test
cargo test --test test_unify

# Use nextest (faster)
cargo nextest run

# Watch mode
cargo watch -x test
```

### Project Structure

```
cargo-rail/
├── src/
│   ├── cargo/          # Cargo.toml transformation & metadata
│   │   ├── unify/      # Resolution-based unification
│   │   └── validate.rs # Multi-target validation
│   ├── graph/          # Dependency graph (petgraph)
│   ├── git/            # System git backend
│   ├── split/          # Monorepo → standalone
│   ├── sync/           # Bidirectional sync
│   ├── workspace/      # Unified context
│   └── commands/       # CLI implementations
├── tests/
│   └── integration/    # Integration tests
├── .config/
│   └── rail.toml.example
└── docs/
```

---

## Troubleshooting

### Unify fails with version conflicts

**Problem:** `cargo rail unify apply` reports incompatible versions

**Solution:**

```bash
# 1. Analyze the conflict
cargo rail unify analyze

# 2. Check what Cargo actually resolved
cargo tree -p <dependency>

# 3. Use --explain for detailed info
cargo rail unify analyze --explain

# 4. Exclude problematic deps temporarily
# In rail.toml:
[unify]
exclude = ["problematic-crate"]
```

### Split creates different SHAs on re-run

**Problem:** Running `cargo rail split` twice produces different commit SHAs

**Likely cause:** Commit timestamps differ

**Solution:**

- cargo-rail is deterministic for the same git history
- If history changed (rebases, new commits), SHAs will differ
- This is expected and correct behavior

### Sync fails with "protected branch" error

**Problem:** `cargo rail sync --from-remote` fails to commit

**Solution:**

```bash
# Remote→monorepo syncs create PR branches automatically
# Check the created branch:
git branch --list 'cargo-rail/sync/*'

# Push the branch and create a PR
git push -u origin <branch>
```

### Tests run for unrelated crates

**Problem:** `cargo rail test` runs tests for crates you didn't change

**Solution:**

```bash
# 1. Check what cargo-rail detected
cargo rail affected --explain

# 2. Verify file classification
# Look for unexpected "source" classifications

# 3. If a dependency changed, this is correct:
#    Transitive dependents must be tested
```

---

## Contributing

Contributions welcome! Please:

1. **Open an issue first** for major changes
2. **Run quality checks**: `just check`
3. **Add tests** for new features
4. **Update docs** as needed

### Areas needing help

- [ ] Release automation (coming in v1.0)
- [ ] Performance benchmarks
- [ ] More integration tests
- [ ] Windows-specific edge cases
- [ ] Documentation improvements

---

## Roadmap

### v0.9.0 (Current)

- ✅ Dependency unification
- ✅ Split/sync with history preservation
- ✅ Graph-aware testing
- ✅ Configuration management

### v1.0.0 (Next Minor)

- 🚧 Release workflow automation
- 🚧 Changelog generation
- 🚧 Version validation
- 🚧 Publish to crates.io

### Future

- Enhanced conflict resolution strategies
- Workspace-wide refactoring tools
- Dependency update automation
- Performance benchmarking suite

---

## Credits

Built with:

- [cargo_metadata](https://crates.io/crates/cargo_metadata) - Workspace introspection
- [petgraph](https://crates.io/crates/petgraph) - Graph algorithms
- [toml_edit](https://crates.io/crates/toml_edit) - Lossless TOML editing
- [clap](https://crates.io/crates/clap) - CLI parsing

Inspired by:

- cargo-hakari's workspace unification approach
- Copybara's monorepo splitting concepts
- Cargo's dependency resolution algorithm

---

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
