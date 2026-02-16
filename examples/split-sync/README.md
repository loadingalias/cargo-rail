# Split/Sync

Extract crates from monorepos to standalone repositories with full git history, then keep them in sync bidirectionally.

## What This Replaces

| Tool | What It Does | Limitation |
|------|--------------|------------|
| **Google Copybara** | Bidirectional sync between repos | Java dependency, complex config, heavy runtime |
| **git subtree** | One-way extraction | Manual, no bidirectional sync |
| **git filter-repo** | History rewriting | One-time operation, no ongoing sync |
| **git subrepo** | Subtree management | Limited conflict resolution |

**cargo-rail split/sync** provides:
- Bidirectional sync with 3-way merge conflict resolution
- Full git history preservation (deterministic SHAs via system git)
- Zero Java/Python dependencies (pure Rust)
- PR-based workflow (never merges to main without review)

## Split Modes

| Mode | Use Case | Example |
|------|----------|---------|
| `single` | One crate → one repo | `tokio-macros` → standalone repo |
| `combined` | Multiple crates → one repo | Shared utilities bundle |
| `workspace` | Multiple crates → workspace structure | Mirrors monorepo layout |

## Quick Start

```bash
# 1. Initialize split config for a crate
cargo rail split init my-crate --check    # preview config
cargo rail split init my-crate            # write config to rail.toml

# 2. Extract crate to standalone repo
cargo rail split run my-crate --check     # preview extraction
cargo rail split run my-crate             # execute extraction

# 3. Ongoing sync (after initial split)
cargo rail sync my-crate --to-remote      # push monorepo changes to split repo
cargo rail sync my-crate --from-remote    # pull split repo changes (creates PR branch)
```

## Configuration

```toml
# rail.toml
[crates.my-crate.split]
remote = "git@github.com:org/my-crate.git"
branch = "main"
mode = "single"
paths = [{ crate = "crates/my-crate" }]

[crates.my-crate.release]
publish = true
```

## Safety

- **Check mode first**: Always run with `--check` before applying
- **Sandbox testing**: Test on throwaway repos before production
- **Clean worktree**: Keep source repo clean before split/sync
- **PR workflow**: `--from-remote` creates a PR branch, never merges directly

## See Also

- [Configuration Reference](../../docs/config.md) — Full `[crates.<name>.split]` options
- [Architecture](../../docs/architecture.md) — How split/sync engine works
