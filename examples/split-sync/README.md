# Split & Sync

Demonstrates `cargo rail split` and `cargo rail sync` for crate extraction with history preservation and bidirectional synchronization.

## What This Shows

1. **History-preserving split** - Extract crate(s) with full git history
2. **Three split modes** - Single crate, combined crates, workspace structure
3. **Bidirectional sync** - Monorepo ↔ split repo with 3-way merge
4. **Conflict resolution** - Multiple strategies for handling conflicts

## Demo

[split-sync demo](./demo.mp4)

## How It Works

```
┌─────────────────────────────────────────────────────────────────┐
│  Split: Extract with History                                    │
│  ──────────────────────────                                     │
│  monorepo/                                                      │
│    ├── crates/core/         ──┐                                │
│    ├── crates/api/            │  git filter-repo                │
│    └── crates/utils/        ──┘                                │
│                               ▼                                 │
│                         standalone-repo/                        │
│                           └── (full git history preserved)      │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│  Sync: Bidirectional 3-Way Merge                               │
│  ────────────────────────────                                   │
│  monorepo          split-repo                                   │
│     │                  │                                        │
│     │   last-sync      │                                        │
│     │      ▼           │                                        │
│     ├──── A ───────────┤   (common ancestor)                   │
│     │      \          /│                                        │
│     ▼       \        / ▼                                        │
│  mono-B      ──────▶  split-C                                  │
│     │          3-way    │                                       │
│     ▼          merge    ▼                                       │
│  mono-D ◀───────────▶ split-D   (merged result)                │
└─────────────────────────────────────────────────────────────────┘
```

## Split Modes

| Mode | Use Case | Config |
|------|----------|--------|
| **Single** | One crate per repo | `mode = "single"` |
| **Combined** | Multiple crates, flat structure | `mode = "combined"` |
| **Workspace** | Multiple crates, workspace structure | `mode = "combined"` + `workspace_mode = "workspace"` |

## Commands

```bash
# Configure a split (interactive)
cargo rail split init my-crate

# Preview split (dry run)
cargo rail split my-crate --check

# Execute split
cargo rail split my-crate

# Split all configured crates
cargo rail split --all

# Bidirectional sync (default)
cargo rail sync my-crate

# Push monorepo → split repo only
cargo rail sync my-crate --to-remote

# Pull split repo → monorepo only (creates PR branch)
cargo rail sync my-crate --from-remote

# Conflict resolution strategies
cargo rail sync my-crate --strategy ours    # Prefer monorepo
cargo rail sync my-crate --strategy theirs  # Prefer split repo
cargo rail sync my-crate --strategy manual  # Stop for manual resolution
cargo rail sync my-crate --strategy union   # Combine both changes

# Preview sync without applying
cargo rail sync my-crate --check
```

## Configuration

### Single Crate Split

```toml
# rail.toml
[crates.tokio-util.split]
remote = "git@github.com:user/tokio-util.git"
branch = "main"
mode = "single"
paths = [{ crate = "tokio-util" }]
include = ["LICENSE", "README.md"]
exclude = ["tests/fixtures/**"]
```

### Combined Multi-Crate Split

```toml
# rail.toml - Multiple crates in one split repo
[crates.my-ecosystem.split]
remote = "git@github.com:user/my-ecosystem.git"
branch = "main"
mode = "combined"
workspace_mode = "workspace"  # or "standalone"
paths = [
    { crate = "crates/core" },
    { crate = "crates/derive" },
    { crate = "crates/runtime" },
]
include = ["LICENSE", "README.md", "CONTRIBUTING.md"]
```

### Per-Crate Release Settings

```toml
# Disable crates.io publish for internal crates
[crates.internal-tools.release]
publish = false

# Custom changelog location
[crates.my-crate.changelog]
path = "docs/CHANGELOG.md"
```

## Workflow Examples

### Open Source Extraction

```bash
# 1. Configure the split
cargo rail split init my-public-crate

# 2. Preview what will be extracted
cargo rail split my-public-crate --check

# 3. Execute the split (creates standalone repo)
cargo rail split my-public-crate

# 4. Push to GitHub
cd ../my-public-crate
git remote add origin git@github.com:user/my-public-crate.git
git push -u origin main
```

### Accepting External PRs

```bash
# 1. External contributor submits PR to split repo
# 2. Merge the PR in split repo
# 3. Sync changes back to monorepo
cargo rail sync my-crate --from-remote

# 4. This creates a branch "sync/my-crate" in monorepo
# 5. Review and merge the sync branch
```

### Monorepo → Split Repo Updates

```bash
# After making changes in monorepo
cargo rail sync my-crate --to-remote

# Or sync all configured crates
cargo rail sync --all --to-remote
```

## Conflict Resolution

When both repos have divergent changes, the `--strategy` option controls resolution:

| Strategy | Behavior |
|----------|----------|
| `manual` (default) | Stop and let you resolve conflicts |
| `ours` | Prefer monorepo changes |
| `theirs` | Prefer split repo changes |
| `union` | Attempt to combine both (best for additive changes) |

## Use Cases

- **Open source extraction** - Publish internal crates as standalone packages
- **Contribution workflow** - Accept PRs to split repos, sync back to monorepo
- **Modular distribution** - Maintain both monorepo and standalone releases
- **Gradual migration** - Split crates incrementally as they stabilize

Details in [`summary.toml`](./summary.toml).
