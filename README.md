# cargo-rail

**Split Rust crates from monorepos, keep them in sync**

Split Rust crates from Cargo workspaces into standalone repos with full git history. Bidirectional sync keeps monorepo and split repos in line.

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/loadingalias/cargo-rail/blob/main/LICENSE)

---

## Status: Production Ready (v0.1)

**Split & Sync:** ✅ Production-ready
**Documentation:** ✅ Complete
**CI Coverage:** ✅ 4 platforms (Linux/Windows x86_64/ARM64) + macOS local

All features stable. Focused on split/sync only - use cargo-release or release-plz for publishing.

---

## Why cargo-rail?

| Feature | cargo-rail | git-subtree | Copybara |
|---------|------------|-------------|----------|
| Split crates | Full history | One-way | Complex |
| Bidirectional sync | ✓ | ❌ | ✓ |
| Cargo-aware transforms | ✓ | ❌ | ❌ |
| Dry-run by default | ✓ | ❌ | ❌ |
| Setup | One TOML | Complex | Very complex |

**cargo-rail = git-subtree + cargo transforms + bidirectional sync**

---

## Features

**Split & Sync:**

- Full git history preservation (author, date, message)
- Bidirectional sync: monorepo ↔ split repo
- Dry-run by default (`--apply` required)
- Protected branch safety (creates PR branches for remote→mono)
- Cargo.toml transforms (path deps ↔ version deps)
- Git-notes commit mapping (rebase-tolerant)
- Conflict resolution (ours, theirs, manual, union)
- Two modes: single crate → repo, or multiple crates → combined repo

---

## Installation

```bash
# From crates.io
cargo install cargo-rail

# From source
git clone https://github.com/loadingalias/cargo-rail
cd cargo-rail
cargo install --path crates/cargo-rail

# From GitHub Releases (binaries for Linux, macOS, Windows)
# See: https://github.com/loadingalias/cargo-rail/releases
```

---

## Quick Start

### Single Mode (One Crate → One Repo)

```bash
# 1. Initialize
cd your-workspace/
cargo rail init
```

Edit `rail.toml`:

```toml
[workspace]
root = "/path/to/workspace"

[[splits]]
name = "my-crate"
remote = "git@github.com:you/my-crate.git"
branch = "main"
mode = "single"
paths = [
  { crate = "crates/my-crate" }
]
```

```bash
# 2. Split (dry-run first)
cargo rail split my-crate
cargo rail split my-crate --apply

# 3. Sync changes
cargo rail sync my-crate --apply
```

### Combined Mode (Multiple Crates → One Repo)

Use combined mode when you want to split multiple related crates into a single repository (e.g., a client + server, or a core + plugins).

```toml
[[splits]]
name = "my-project"
remote = "git@github.com:you/my-project.git"
branch = "main"
mode = "combined"
paths = [
  { crate = "crates/my-client" },
  { crate = "crates/my-server" },
  { crate = "crates/my-common" }
]
```

```bash
# Split all three crates into one repo
cargo rail split my-project --apply

# Sync changes from any of the three crates
cargo rail sync my-project --apply
```

**Result:** Creates a single repo with:

```
my-project/
├── my-client/      (from crates/my-client)
├── my-server/      (from crates/my-server)
├── my-common/      (from crates/my-common)
└── Cargo.toml      (workspace manifest)
```

---

## Workflow Example: 25-Crate Monorepo

**Scenario:** 5 proprietary crates, 20 OSS (5 split to separate repos, 15 stay in monorepo)

### Development (Monorepo)

```bash
cd monorepo/
# ... make changes to crates/my-oss-lib ...
git commit -am "feat: Add feature"

# Sync to split repo
cargo rail sync my-oss-lib --apply
```

### Publishing

**Option 1: Monorepo-only crates** (20 crates)

```bash
cd monorepo/
cargo release --workspace  # or release-plz
```

**Option 2: Split crates** (5 crates)

```bash
# Ensure synced
cargo rail sync my-oss-lib --apply

# Publish from split repo
cd ../my-oss-lib-split/
cargo release
# Contributors see a normal standalone crate
```

### External Contributions

```bash
# PR comes into split repo
cd ../monorepo/
cargo rail sync my-oss-lib --apply
# Creates PR branch: rail/sync/my-oss-lib/1763010755
# Review and merge manually
```

---

## Commands

```bash
cargo rail init                      # Initialize configuration
cargo rail split <name>              # Split crate(s) (dry-run)
cargo rail split <name> --apply      # Actually split
cargo rail sync <name>               # Sync changes (dry-run)
cargo rail sync <name> --apply       # Actually sync
cargo rail sync --all --apply        # Sync all configured splits
cargo rail doctor                    # Run health checks
cargo rail status                    # Show configured splits
cargo rail mappings <name>           # Inspect git-notes mappings
```

---

## Configuration

Full `rail.toml` example:

```toml
[workspace]
root = "/absolute/path/to/workspace"

[security]
ssh_key_path = "~/.ssh/id_ed25519"
require_signed_commits = false
pr_branch_pattern = "rail/sync/{crate}/{timestamp}"
protected_branches = ["main", "master"]

# Single mode example
[[splits]]
name = "http-client"
remote = "git@github.com:you/http-client.git"
branch = "main"
mode = "single"
paths = [{ crate = "crates/http-client" }]

# Combined mode example
[[splits]]
name = "my-tools"
remote = "git@github.com:you/my-tools.git"
branch = "main"
mode = "combined"
paths = [
  { crate = "crates/tool-a" },
  { crate = "crates/tool-b" },
  { crate = "crates/tool-common" }
]
```

---

## Conflict Resolution

```bash
# Show conflict
cargo rail sync my-crate
# Conflict detected: both sides modified src/lib.rs

# Choose strategy
cargo rail sync my-crate --apply --conflict=ours    # keep monorepo
cargo rail sync my-crate --apply --conflict=theirs  # keep split repo
cargo rail sync my-crate --apply --conflict=manual  # pause for manual resolution
cargo rail sync my-crate --apply --conflict=union   # combine both (risky)
```

---

## Architecture

### System Overview

```
┌──────────────────────────────────────────────────────────────┐
│                    MONOREPO (Source of Truth)                │
│  workspace-root/                                             │
│  ├── crates/my-core/                                         │
│  ├── crates/my-client/                                       │
│  └── crates/my-server/                                       │
└──────────────────────────────────────────────────────────────┘
                           │
           ┌───────────────┼───────────────┐
           │ split         │ split         │ split
           │ (single)      │ (single)      │ (combined)
           ↓               ↓               ↓
    ┌──────────┐    ┌──────────┐    ┌─────────────────┐
    │ my-core  │    │ my-client│    │ my-server       │
    │          │    │          │    │ (standalone)    │
    └──────────┘    └──────────┘    └─────────────────┘
           │               │               │
           │ cargo release │ cargo release │ cargo release
           ↓               ↓               ↓
    ┌────────────────────────────────────────────────┐
    │            crates.io registry                  │
    └────────────────────────────────────────────────┘
```

### Sync Flow

```
MONOREPO → SPLIT (Direct Push)
┌──────────────────────────────────────────────┐
│ 1. Detect new commits                        │
│ 2. Filter commits (git log --path)           │
│ 3. Transform Cargo.toml (path → version)     │
│ 4. Apply commits to split repo               │
│ 5. Update git-notes mapping                  │
│ 6. Push to split repo                        │
└──────────────────────────────────────────────┘

SPLIT → MONOREPO (PR Branch - Security)
┌──────────────────────────────────────────────┐
│ 1. Detect new commits in split               │
│ 2. Create PR branch: rail/sync/{name}/{ts}   │
│ 3. Transform Cargo.toml (version → path)     │
│ 4. Apply commits to PR branch                │
│ 5. Update git-notes mapping                  │
│ 6. Print review instructions (NO AUTO-MERGE) │
└──────────────────────────────────────────────┘
```

---

## Security Model

### Monorepo → Split Repo

- Automatic push to configured branch
- Requires SSH key authentication
- Recommended: Branch protection + CI checks

### Split Repo → Monorepo

- **NEVER commits directly to main/master**
- Creates PR branch: `rail/sync/{name}/{timestamp}`
- Requires manual review before merging
- Protects against malicious changes

**Checklist:**

1. Enable branch protection on monorepo main
2. Require PR reviews for `rail/sync/*` branches
3. Use deploy keys (write access to split repos only)
4. Optional: Enable signed commits
5. Run `cargo rail doctor` to verify

See [SECURITY.md](docs/SECURITY.md) for full threat model.

---

## CI Integration

### GitHub Actions (Monorepo → Split)

`.github/workflows/rail-sync.yml`:

```yaml
name: Sync to Split Repos
on:
  push:
    branches: [main]
    paths: ['crates/my-crate/**']

jobs:
  sync:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with: { fetch-depth: 0 }
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install cargo-rail
      - run: cargo rail sync my-crate --apply
        env: { SSH_PRIVATE_KEY: '${{ secrets.DEPLOY_KEY }}' }
```

---

## Publishing Workflow

cargo-rail focuses on split/sync. Use existing tools for releases:

**Recommended:**

- [cargo-release](https://github.com/crate-ci/cargo-release) - Simple, battle-tested
- [release-plz](https://github.com/MarcoIeni/release-plz) - Automated releases from CI
- [git-cliff](https://github.com/orhun/git-cliff) - Changelog generation

```bash
# Monorepo-only crates
cd monorepo/
cargo release --workspace

# Split crates
cargo rail sync my-crate --apply  # Ensure synced
cd ../my-crate-split/
cargo release  # Publish from split repo
```

---

## Documentation

- [USER_GUIDE.md](docs/USER_GUIDE.md) - Complete walkthrough
- [SECURITY.md](docs/SECURITY.md) - Threat model and mitigations
- [E2E_TESTING_SETUP.md](E2E_TESTING_SETUP.md) - End-to-end testing guide
- [STATUS.md](STATUS.md) - Development status

---

## Troubleshooting

**"SSH key validation failed"**

```bash
chmod 600 ~/.ssh/id_ed25519
ssh -T git@github.com
cargo rail doctor --thorough
```

**"Git-notes conflict detected"**

```bash
cargo rail sync my-crate --apply  # Uses union merge strategy
```

**"Non-fast-forward push"**

```bash
cd ../my-crate-split
git pull origin main
cd ../monorepo
cargo rail sync my-crate --apply
```

---

## Comparison

**vs git-subtree:** cargo-rail is bidirectional + Cargo-aware
**vs Copybara:** cargo-rail is simpler (one TOML vs Starlark)
**vs git-filter-repo:** cargo-rail preserves monorepo (non-destructive)
**vs cargo-workspaces:** cargo-rail adds split/sync
**vs release-plz:** cargo-rail adds split/sync (use release-plz for releases!)

---

## Contributing

Contributions welcome! See [TODO.md](https://github.com/loadingalias/cargo-rail/blob/main/TODO.md) for planned features.

1. Check roadmap
2. Open issue to discuss
3. Submit PR with tests

---

## License

MIT - see [LICENSE](https://github.com/loadingalias/cargo-rail/blob/main/LICENSE)

---

## Acknowledgments

- Inspired by Google's Copybara and Meta's Sapling
- Built on [gitoxide](https://github.com/Byron/gitoxide) (pure Rust git)

---

## FAQ

**Q: Single vs combined mode - when to use each?**
A: Single mode for independent crates published to crates.io. Combined mode for related crates that should stay together (e.g., client+server).

**Q: Can I use this in production today?**
A: Yes. All features are stable and production-ready.

**Q: Does this work with private repos?**
A: Yes, use SSH authentication with deploy keys.

**Q: What if my split repo gets compromised?**
A: cargo-rail never commits directly to monorepo main. All syncs create PR branches for review.

**Q: Can I sync multiple splits at once?**
A: Yes: `cargo rail sync --all --apply`

**Q: How do I publish crates?**
A: Use cargo-release or release-plz. cargo-rail focuses on split/sync only.

**Q: Large monorepos (100+ crates)?**
A: Current focus is 5-50 crates. Larger workspaces may need performance tuning.

---

**Made with Rust** • [Star this repo ⭐](https://github.com/loadingalias/cargo-rail)
