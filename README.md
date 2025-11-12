# cargo-rail

**Split Rust crates from monorepos, keep them in sync**

Split Rust crates from Cargo workspaces into standalone repos with full git history. Bidirectional sync keeps monorepo and split repos in line. Release automation with semver checks and topological publishing.

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/loadingalias/cargo-rail/blob/main/LICENSE)

---

## Status: Pre-Release (v0.1)

**Split & Sync:** Production-ready
**Release Commands:** Coming in v1.0 (3 days)
**Documentation:** In progress

Use split/sync features today. Release automation coming soon.

---

## Why cargo-rail?

| Feature | cargo-rail | cargo-workspaces | release-plz | git-subtree | Copybara |
|---------|------------|------------------|-------------|-------------|----------|
| Split crates | Full history | ❌ | ❌ | One-way | Complex |
| Bidirectional sync | ✓ | ❌ | ❌ | ❌ | ✓ |
| Cargo-aware | ✓ | ✓ | ✓ | ❌ | ❌ |
| Release automation | v1.0 | Basic | ✓ | ❌ | ❌ |
| Semver checks | v1.0 | ❌ | ✓ | ❌ | ❌ |
| Dry-run by default | ✓ | ❌ | Partial | ❌ | ❌ |
| Setup | One TOML | Easy | Easy | Complex | Very complex |

**cargo-rail = cargo-workspaces + release-plz + git-subtree**

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

**Release Automation (v1.0):**

- Semver enforcement with breaking change detection
- Topological publishing (dependencies first)
- Changelog generation from conventional commits
- Tag management across monorepo and split repos
- GitHub release integration

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

## Modes Explained

### Single Mode

- **One crate → One repo**
- Use when: Publishing independent crates to crates.io
- Example: Split `crates/http-client` → `http-client` standalone repo

### Combined Mode

- **Multiple crates → One repo**
- Use when: Related crates that should live together
- Example: Split `crates/{client,server,common}` → `my-project` workspace repo
- Maintains workspace structure in split repo
- All crates keep their workspace dependencies

---

## Sync Workflow

### Monorepo → Split Repo

```bash
cd your-workspace/
# Make changes in crates/my-crate/
git commit -am "feat: Add feature"

# Sync to split repo (pushes automatically)
cargo rail sync my-crate --apply
```

### Split Repo → Monorepo

```bash
cd my-crate-split/
# Make changes
git commit -am "fix: Bug fix"
git push origin main

cd ../your-workspace/
# Sync from split repo (creates PR branch)
cargo rail sync my-crate --apply
# Creates branch: rail/sync/my-crate/2025-11-11-143022
# Review and merge PR manually
```

**Security:** Split→mono syncs NEVER commit directly to main. Always creates PR branch for review.

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

# Release commands (v1.0)
cargo rail release plan              # Preview releases
cargo rail release prepare --apply   # Update versions, changelogs
cargo rail release publish --apply   # Publish to crates.io
cargo rail release finalize --apply  # Create tags, sync to splits
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

[release]
strategy = "conventional"  # or "manual"
tag_format = "{name}@v{version}"
publish_delay = 30  # seconds between dependent publishes
create_github_releases = true
auto_sync_split_repos = true

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

```
┌─────────────────────────────────────────┐
│        MONOREPO (main)                  │
│  crates/a/  crates/b/  crates/c/        │
└─────────────────────────────────────────┘
     │              │              │
     │ split/sync   │ split/sync   │ split/sync
     ↓              ↓              ↓
 ┌───────┐      ┌───────┐      ┌────────────┐
 │ crate-a│      │ crate-b│      │ project    │  (combined)
 │        │      │        │      │ ├─ crate-c1│
 └───────┘      └───────┘      │ └─ crate-c2│
                               └────────────┘
     │              │              │
     │ publish      │ publish      │ publish
     ↓              ↓              ↓
  ┌─────────────────────────────────┐
  │      crates.io registry         │
  └─────────────────────────────────┘
```

**Key concepts:**

- **Git-notes mapping:** `refs/notes/rail/{split}` tracks commit relationships
- **Transform pipeline:** Rewrites Cargo.toml during sync
- **PR-only branch:** Remote→mono creates `rail/sync/{name}/{timestamp}`
- **Topological publish:** Dependencies published before dependents

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

See [SECURITY.md](https://github.com/loadingalias/cargo-rail/blob/main/docs/SECURITY.md) for threat model.

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

### Release on Tag

`.github/workflows/rail-release.yml`:

```yaml
name: Release
on:
  push:
    tags: ['v*']

jobs:
  release:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install cargo-rail
      - run: |
          cargo rail release prepare --apply
          cargo rail release publish --apply --yes
          cargo rail release finalize --apply
        env:
          CARGO_REGISTRY_TOKEN: '${{ secrets.CARGO_TOKEN }}'
```

---

## Documentation

- [USER_GUIDE.md](https://github.com/loadingalias/cargo-rail/blob/main/docs/USER_GUIDE.md) - Complete walkthrough
- [SECURITY.md](https://github.com/loadingalias/cargo-rail/blob/main/docs/SECURITY.md) - Threat model and mitigations
- [RELEASE_GUIDE.md](https://github.com/loadingalias/cargo-rail/blob/main/docs/RELEASE_GUIDE.md) - Release workflow
- [ARCHITECTURE.md](https://github.com/loadingalias/cargo-rail/blob/main/docs/ARCHITECTURE.md) - System design
- [PERFORMANCE.md](https://github.com/loadingalias/cargo-rail/blob/main/docs/PERFORMANCE.md) - Benchmarks
- [API.md](https://github.com/loadingalias/cargo-rail/blob/main/docs/API.md) - JSON formats, exit codes

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

**vs cargo-workspaces:** cargo-rail adds split/sync + release (complete workflow)
**vs release-plz:** cargo-rail adds split/sync to release automation
**vs git-subtree:** cargo-rail is bidirectional + Cargo-aware
**vs Copybara:** cargo-rail is simpler (one TOML vs Starlark)
**vs git-filter-repo:** cargo-rail preserves monorepo (non-destructive)

---

## Roadmap

### v1.0 (Late November 2025)

- ✓ Split & sync (both modes)
- Release automation (in progress)
  - Semver checking (cargo-semver-checks)
  - Conventional commits
  - Changelog generation
  - Topological publishing
  - Tag management
- Complete documentation
- CI templates (GitHub + GitLab)

### v1.1+

- Watch mode (`cargo rail watch`)
- Performance optimizations
- Homebrew formula

See [TODO.md](https://github.com/loadingalias/cargo-rail/blob/main/TODO.md) for details.

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
- Semver checking via [cargo-semver-checks](https://github.com/obi1kenobi/cargo-semver-checks)

---

## FAQ

**Q: Single vs combined mode - when to use each?**
A: Single mode for independent crates published to crates.io. Combined mode for related crates that should stay together (e.g., client+server).

**Q: Can I use this in production today?**
A: Split/sync features are stable. Release automation coming in v1.0 (days).

**Q: Does this work with private repos?**
A: Yes, use SSH authentication with deploy keys.

**Q: What if my split repo gets compromised?**
A: cargo-rail never commits directly to monorepo main. All syncs create PR branches for review.

**Q: Can I sync multiple splits at once?**
A: Yes: `cargo rail sync --all --apply`

**Q: Large monorepos (100+ crates)?**
A: v1.0 focuses on 5-50 crates. Larger workspaces need performance tuning (v1.2).

---

**Made with Rust** • [Star this repo ⭐](https://github.com/loadingalias/cargo-rail)
