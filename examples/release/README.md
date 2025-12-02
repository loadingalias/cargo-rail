# Release Workflow

Demonstrates `cargo rail check` and `cargo rail release` for automated versioning, changelog generation, and publishing.

## What This Shows

1. **Release readiness validation** - Pre-flight checks before release
2. **Semantic versioning** - Automatic version bumping (patch/minor/major)
3. **Changelog generation** - From conventional commits
4. **Multi-crate coordination** - Release multiple workspace crates in order

## Demo

[release demo](./demo.mp4)

## How It Works

```
┌─────────────────────────────────────────────────────────────────┐
│  Release Pipeline                                               │
│  ────────────────                                               │
│                                                                 │
│  1. Check         2. Bump           3. Changelog                │
│  ───────────      ──────────        ────────────                │
│  clean working?   1.2.3 → 1.2.4     Parse commits               │
│  on main/tag?     Update Cargo.toml Generate entries            │
│  all deps ok?     (all members)     Insert into file            │
│                                                                 │
│  4. Tag           5. Publish (optional)                         │
│  ─────────        ─────────────────────                         │
│  my-crate-v1.2.4  cargo publish (with delay)                   │
│  git push --tags  Respects dep order                           │
└─────────────────────────────────────────────────────────────────┘
```

## Commands

```bash
# Validate release readiness
cargo rail check                      # All crates
cargo rail check my-crate             # Specific crate

# Preview release (dry run)
cargo rail release run my-crate --check

# Release with version bump
cargo rail release run my-crate           # Patch bump (default)
cargo rail release run my-crate --bump minor
cargo rail release run my-crate --bump major
cargo rail release run my-crate --bump 2.0.0  # Explicit version

# Release all workspace crates
cargo rail release run --all --bump patch

# Skip steps
cargo rail release run my-crate --skip-publish  # Tag only, no crates.io
cargo rail release run my-crate --skip-tag      # Publish only, no tag
```

## Configuration

### Workspace-Wide Settings

```toml
# rail.toml
[release]
# Tag format (default: "{crate}-{prefix}{version}")
tag_prefix = "v"                      # Default: "v"
tag_format = "{crate}-{prefix}{version}"  # e.g., "my-crate-v1.2.3"

# Changelog settings
changelog_path = "CHANGELOG.md"       # Default per-crate location
changelog_relative_to = "crate"       # "crate" or "workspace"

# Publishing settings
publish_delay = 5                     # Seconds between crate publishes
require_clean = true                  # Require clean git working directory

# Optional features
create_github_release = false         # Create GitHub release via gh CLI
sign_tags = false                     # GPG/SSH sign tags

# Skip changelog for internal crates
skip_changelog_for = ["internal-tools", "test-helpers"]

# Fail if no changelog entries found
require_changelog_entries = false
```

### Per-Crate Overrides

```toml
# Disable publishing for internal crates
[crates.internal-utils.release]
publish = false

# Custom changelog location
[crates.my-public-crate.changelog]
path = "docs/CHANGELOG.md"
skip = false

# Skip changelog entirely
[crates.test-fixtures.changelog]
skip = true
```

### Tag Format Variables

| Variable | Example | Description |
|----------|---------|-------------|
| `{crate}` | `my-crate` | Crate name |
| `{version}` | `1.2.3` | New version |
| `{prefix}` | `v` | Value of `tag_prefix` |

**Examples:**
- `{crate}-{prefix}{version}` → `my-crate-v1.2.3` (default, monorepo)
- `{prefix}{version}` → `v1.2.3` (single crate repo)
- `{crate}@{version}` → `my-crate@1.2.3` (npm style)

## Changelog Configuration

### Per-Crate Changelogs (Default)

```toml
[release]
changelog_path = "CHANGELOG.md"
changelog_relative_to = "crate"
```

Creates: `crates/my-crate/CHANGELOG.md`

### Unified Workspace Changelog

```toml
[release]
changelog_path = "CHANGELOG.md"
changelog_relative_to = "workspace"
```

Creates: `./CHANGELOG.md` (at workspace root)

## Release Check Validation

`cargo rail check` validates:

- Clean git working directory (if `require_clean = true`)
- On a branch that can be tagged
- All workspace dependencies resolved
- Version numbers are valid semver
- Changelog exists (if `require_changelog_entries = true`)

## Multi-Crate Release Order

When releasing `--all`, crates are published in dependency order:
1. Crates with no workspace dependencies first
2. Then their dependents
3. With `publish_delay` seconds between each

```
my-core         → published first
my-derive       → depends on core, published second
my-runtime      → depends on both, published last
```

## Use Cases

- **Consistent releases** - Standardized version bumping across workspace
- **Changelog automation** - Generate changelogs from conventional commits
- **Multi-crate releases** - Coordinate releases across workspace members
- **CI/CD integration** - Automated releases from main branch

## GitHub Actions Integration

```yaml
name: Release
on:
  push:
    tags: ['*-v*']

jobs:
  publish:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo install cargo-rail
      - run: cargo rail release run --all --skip-tag
        env:
          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}
```

Details in [`summary.toml`](./summary.toml).
