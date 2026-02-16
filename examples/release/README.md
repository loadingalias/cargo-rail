# Release

Automated release workflow: version bumps, changelogs, tags, and crates.io publishing.

## What This Replaces

| Tool | Dependencies | Limitation |
|------|--------------|------------|
| **release-plz** | ~500 deps | Heavy dependency graph, large attack surface |
| **cargo-release** | ~150 deps | Single-crate focus, limited changelog |
| **git-cliff** | ~200 deps | Changelog only, no publishing |
| **cargo-smart-release** | ~300 deps | Complex workspace handling |

**cargo-rail release** provides:
- Release checks, versioning, changelogs, tags, publishing in one command
- 14 core dependencies total (entire cargo-rail)
- Dependency-order publishing for workspace crates
- Conventional commit changelog generation (built-in parser, no git-cliff-core)

## Quick Start

```bash
# Check release readiness
cargo rail release check my-crate

# Preview release (no publish)
cargo rail release run my-crate --bump patch --check

# Execute release
cargo rail release run my-crate --bump patch --yes
git push origin main --follow-tags
```

## Bump Types

| Type | Example | When |
|------|---------|------|
| `patch` | 1.0.0 → 1.0.1 | Bug fixes |
| `minor` | 1.0.0 → 1.1.0 | New features (backward compatible) |
| `major` | 1.0.0 → 2.0.0 | Breaking changes |

## Configuration

```toml
# rail.toml
[release]
tag_prefix = "v"
tag_format = "{prefix}{version}"
require_clean = true
publish_delay = 5
create_github_release = false
sign_tags = true
changelog_path = "CHANGELOG.md"
changelog_relative_to = "crate"
skip_changelog_for = []
require_changelog_entries = false

[crates.my-crate.release]
publish = true
```

## Workflow

1. **Check**: `cargo rail release check my-crate` — validates prerequisites
2. **Plan**: `cargo rail release run my-crate --bump patch --check` — shows what would happen
3. **Execute**: `cargo rail release run my-crate --bump patch --yes` — bumps, changelog, tag, publish
4. **Push**: `git push origin main --follow-tags` — push tag to trigger CI release

## Safety

- **Check mode**: Always preview with `--check` first
- **Clean worktree**: `require_clean = true` enforces no uncommitted changes
- **Publish delay**: `publish_delay = 5` waits between crates for crates.io index propagation
- **No force push**: Never alters published history

## Changelog Format

Generated from conventional commits:

```markdown
## [1.0.1] - 2026-02-16

### Fixed
- Fix authentication bug in login flow

### Changed
- Update dependency versions
```

## See Also

- [Configuration Reference](../../docs/config.md) — Full `[release]` and `[crates.<name>.release]` options
- [Architecture](../../docs/architecture.md) — How release planner/publisher works
