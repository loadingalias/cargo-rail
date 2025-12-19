# Configuration Reference

> Complete reference for all rail.toml configuration options. Kept in sync with source defaults and CLI behavior.

## Overview

cargo-rail uses `rail.toml` for workspace-level configuration. This file controls:

- **Dependency unification** (`cargo rail unify`)
- **Release automation** (`cargo rail release`)
- **Change detection** (`cargo rail affected`)
- **Crate splitting** (`cargo rail split`)

### Configuration File Location

Configuration files are searched in order:

1. `rail.toml` (workspace root)
2. `.rail.toml` (workspace root, hidden)
3. `.cargo/rail.toml` (cargo directory)
4. `.config/rail.toml` (config directory)

The first file found is used. All paths are relative to the workspace root.

### Generating Configuration

Generate a default configuration file:

```bash
cargo rail init                    # Creates .config/rail.toml
cargo rail init -o rail.toml       # Creates rail.toml
cargo rail init --check            # Preview without writing
cargo rail init --force            # Overwrite existing config
```

## Quick Start

### Minimal Configuration

cargo-rail works with sensible defaults. An empty file is valid; add options only when you need them:

```toml
# Minimal config (optional): set targets if you want multi-target validation.
# (`cargo rail init` can auto-detect targets from *.toml and .github/workflows.)
targets = ["x86_64-unknown-linux-gnu"]
```

### Typical Configuration

```toml
# Multi-target workspace with unify enabled
targets = [
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
]

[unify]
msrv = true                      # Compute MSRV from dependencies
prune_dead_features = true       # Remove unused features
detect_unused = true             # Find unused dependencies
remove_unused = true             # Auto-remove them

[release]
tag_format = "{crate}-{prefix}{version}"
create_github_release = false

[change-detection]
infrastructure = [".github/**", "justfile"]
```

## Complete Reference

### Top-Level Options

Configuration options at the workspace root level.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `targets` | `string[]` | `[]` | Target triples for multi-platform validation. Used by `unify` and other commands to run `cargo metadata --filter-platform` for each target. Auto-detected by `cargo rail init`. |
| `unify` | `table` | `{}` | Dependency unification settings (see below) |
| `release` | `table` | `{}` | Release management settings (see below) |
| `change-detection` | `table` | `{}` | Change detection settings (see below) |
| `crates` | `table` | `{}` | Per-crate configuration (see below) |

**Example:**

```toml
targets = [
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
]
```

---

### [unify] Configuration

Controls workspace dependency unification behavior. All options are optional with sensible defaults.

#### Core Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `msrv` | `bool` | `true` | Compute and write MSRV to `[workspace.package].rust-version` (written as `major.minor.patch`). The MSRV is determined by `msrv_source`. |
| `enforce_msrv_inheritance` | `bool` | `false` | Ensure every workspace member inherits MSRV by setting `[package].rust-version = { workspace = true }` in each member's `Cargo.toml`. This makes `[workspace.package].rust-version` actually apply across the workspace. |
| `msrv_source` | `enum` | `"max"` | How to compute the final MSRV:<br>• `"deps"` - Use maximum from dependencies only (original behavior)<br>• `"workspace"` - Preserve existing rust-version, warn if deps need higher<br>• `"max"` - Take max(workspace, deps) - your explicit setting wins if higher |
| `detect_unused` | `bool` | `true` | Detect dependencies declared in manifests but absent from the resolved cargo graph. |
| `remove_unused` | `bool` | `true` | Automatically remove unused dependencies during unification. Requires `detect_unused = true`. |
| `prune_dead_features` | `bool` | `true` | Remove features that are never enabled in the resolved dependency graph across all targets. Only prunes empty no-ops (`feature = []`). Features with actual dependencies are preserved. |
| `preserve_features` | `string[]` | `[]` | Features to preserve from dead feature pruning. Supports glob patterns (e.g., `"unstable-*"`, `"bench*"`). Use this to keep features intended for future use or external consumers. |
| `detect_undeclared_features` | `bool` | `true` | Detect crates that rely on Cargo's feature unification to "borrow" features from other workspace members. These crates will fail when built standalone after unification. Reports as warnings (or auto-fixes if `fix_undeclared_features` is enabled). |
| `fix_undeclared_features` | `bool` | `true` | Auto-fix undeclared feature dependencies by adding missing features to each crate's Cargo.toml. Produces a cleaner graph where standalone builds work correctly. Requires `detect_undeclared_features = true`. |
| `skip_undeclared_patterns` | `string[]` | `["default", "std", "alloc", "*_backend", "*_impl"]` | Patterns for features to skip in undeclared feature detection. Supports glob patterns. Default patterns filter out features that are typically not actionable (standard library features, internal implementation details). |
| `max_backups` | `usize` | `3` | Maximum number of backup archives to keep. Older backups are automatically cleaned up after successful operations. Set to `0` to disable backup creation entirely. |

**Example:**

```toml
[unify]
msrv = true
msrv_source = "max"  # "deps" | "workspace" | "max"
enforce_msrv_inheritance = false
detect_unused = true
remove_unused = true
prune_dead_features = true
preserve_features = ["future-api", "unstable-*"]  # Keep these from pruning
detect_undeclared_features = true  # Catch borrowed features
fix_undeclared_features = true    # Auto-fix them (default)
skip_undeclared_patterns = ["default", "std", "alloc", "*_backend", "*_impl"]  # Features to skip
max_backups = 5
```

#### Version Handling

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `strict_version_compat` | `bool` | `true` | When `true`, version mismatches between member manifests and existing `workspace.dependencies` are blocking errors. When `false`, they are warnings only. |
| `exact_pin_handling` | `enum` | `"warn"` | How to handle exact version pins like `=0.8.0`:<br>• `"skip"` - Exclude exact-pinned deps from unification<br>• `"preserve"` - Keep the exact pin operator in workspace.dependencies<br>• `"warn"` - Convert to caret (`^`) but emit a warning |
| `major_version_conflict` | `enum` | `"warn"` | How to handle major version conflicts (e.g., `serde = "1.0"` and `serde = "2.0"`):<br>• `"warn"` - Skip unification, emit warning (both versions stay in graph)<br>• `"bump"` - Force unify to highest resolved version (may break code) |

**Example:**

```toml
[unify]
strict_version_compat = false
exact_pin_handling = "preserve"
major_version_conflict = "bump"
```

**Notes:**

- In my experience, `major_version_conflict = "bump"` works in most cases; some may require code fixes
- Use `"warn"` for safety, `"bump"` for the leanest build graph
- If `[workspace.package].rust-version` is missing but root `[package].rust-version` is present, `unify` uses it as the baseline and writes it to `[workspace.package].rust-version` (consider enabling `enforce_msrv_inheritance` to avoid drift)

#### Dependency Selection

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `include_paths` | `bool` | `true` | Include path dependencies in unification. If `false`, path dependencies are excluded. |
| `include_renamed` | `bool` | `false` | Include renamed dependencies (`package = "..."`). When enabled, features are aggregated across all variants using union. Opt-in due to complexity. |
| `exclude` | `string[]` | `[]` | Dependencies to skip from unification (safety hatch). Useful for platform-specific or problematic dependencies. |
| `include` | `string[]` | `[]` | Force-include specific dependencies in unification, even if they're single-use. |

**Example:**

```toml
[unify]
include_paths = true
include_renamed = false
exclude = ["openssl", "windows-sys"]  # Platform-specific
include = ["my-special-dep"]          # Force include
```

#### Transitive Pinning

Advanced feature for replacing workspace-hack crates. Only enable if you currently use `cargo-hakari`.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `pin_transitives` | `bool` | `false` | Pin transitive-only dependencies with fragmented features. This is cargo-rail's workspace-hack replacement. When enabled, transitive deps with multiple feature sets are pinned in `workspace.dependencies`. |
| `transitive_host` | `string` | `"root"` | Where to put pinned transitive dev-dependencies:<br>• `"root"` - Use workspace root `Cargo.toml`<br>• `"crates/foo"` - Use specific member crate (relative path from workspace root) |

**Example:**

```toml
[unify]
pin_transitives = true
transitive_host = "root"
```

**Complete Example:**

```toml
[unify]
# Core options (defaults shown)
msrv = true
msrv_source = "max"  # "deps" | "workspace" | "max"
enforce_msrv_inheritance = false
detect_unused = true
remove_unused = true
prune_dead_features = true
preserve_features = []  # Glob patterns to preserve from pruning
max_backups = 3

# Version handling
strict_version_compat = true
exact_pin_handling = "warn"
major_version_conflict = "warn"

# Dependency selection
include_paths = true
include_renamed = false
exclude = []
include = []

# Transitive pinning (workspace-hack replacement)
pin_transitives = false
transitive_host = "root"
```

---

### [release] Configuration

Release automation settings for versioning, tagging, and publishing.

#### Core Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `tag_prefix` | `string` | `"v"` | Git tag prefix. Used via `{prefix}` placeholder in `tag_format`. |
| `tag_format` | `string` | `"{crate}-{prefix}{version}"` | Tag template. Available variables:<br>• `{crate}` - Crate name<br>• `{version}` - Version number<br>• `{prefix}` - Value of `tag_prefix` |
| `require_clean` | `bool` | `true` | Require clean working directory before release operations. |
| `publish_delay` | `u64` | `5` | Delay between crate publishes in seconds. Allows crates.io to propagate dependencies. |
| `create_github_release` | `bool` | `false` | Automatically create GitHub releases via `gh` CLI after tagging. Requires `gh` to be installed and authenticated. |
| `sign_tags` | `bool` | `false` | Sign git tags with GPG or SSH. Requires git signing to be configured. |

**Example:**

```toml
[release]
tag_prefix = "v"
tag_format = "{crate}-{prefix}{version}"    # Produces: my-crate-v1.0.0
require_clean = true
publish_delay = 10
create_github_release = true
sign_tags = true
```

#### Changelog Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `changelog_path` | `string` | `"CHANGELOG.md"` | Default changelog filename for all crates. |
| `changelog_relative_to` | `enum` | `"crate"` | What changelog paths are relative to:<br>• `"crate"` - Relative to each crate's directory<br>• `"workspace"` - Relative to workspace root |
| `skip_changelog_for` | `string[]` | `[]` | Crate names that should not generate changelog entries. |
| `require_changelog_entries` | `bool` | `false` | If `true`, error when there are no changelog entries for a crate being released. |

**Example:**

```toml
[release]
changelog_path = "CHANGELOG.md"
changelog_relative_to = "crate"
skip_changelog_for = ["internal-utils"]
require_changelog_entries = true
```

**Complete Example:**

```toml
[release]
# Core
tag_prefix = "v"
tag_format = "{crate}-{prefix}{version}"
require_clean = true
publish_delay = 5
create_github_release = false
sign_tags = false

# Changelog
changelog_path = "CHANGELOG.md"
changelog_relative_to = "crate"
skip_changelog_for = []
require_changelog_entries = false
```

**Notes:**

- In monorepos, use `{crate}` in `tag_format` to avoid tag collisions
- For single-crate workspaces, use `tag_format = "v{version}"`
- `changelog_relative_to = "workspace"` is useful for unified changelogs

---

### [change-detection] Configuration

Settings for the `affected` and `test` commands. Controls how changes are classified and which crates are affected.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `infrastructure` | `string[]` | see below | Glob patterns for infrastructure files that trigger `rebuild_all` when modified. Changes to these files require testing all crates. |
| `custom` | `table<string, string[]>` | `{}` | Custom path patterns and their categories. Keys are category names, values are glob pattern arrays. Used for conditional CI logic. |

**Default Infrastructure Patterns:**

```toml
infrastructure = [
    ".github/**",
    "scripts/**",
    "justfile",
    "Justfile",
    "Makefile",
    "makefile",
    "GNUmakefile",
    "*.sh",
    "Taskfile.yml",
    "Taskfile.yaml",
    ".pre-commit-config.yaml",
    "deny.toml",
    "cliff.toml",
    "release.toml",
    "release-plz.toml",
]
```

**Example:**

```toml
[change-detection]
infrastructure = [
    ".github/**",
    "justfile",
    "Cargo.lock",        # Add lock file
    "rust-toolchain.toml"
]

[change-detection.custom]
verify = ["verify/**/*.rs"]           # Stateright models
benchmarks = ["benches/**", "perf/**"]
docs = ["docs/**", "*.md"]
```

#### GitHub Actions Integration

Use [`loadingalias/cargo-rail-action`](https://github.com/loadingalias/cargo-rail-action) for CI:

```yaml
- uses: actions/checkout@v4
  with: { fetch-depth: 0 }

- uses: loadingalias/cargo-rail-action@v1
  id: affected

- name: Test affected
  if: steps.affected.outputs.docs-only != 'true'
  run: |
    if [[ "${{ steps.affected.outputs.rebuild-all }}" == "true" ]]; then
      cargo test --workspace
    else
      cargo test ${{ steps.affected.outputs.cargo-args }}
    fi
```

**Action Outputs:**

| Output | Description |
|--------|-------------|
| `docs-only` | `"true"` if only documentation changed |
| `rebuild-all` | `"true"` if infrastructure files changed |
| `crates` | Space-separated affected crates |
| `cargo-args` | Ready-to-use `-p crate1 -p crate2` flags |
| `matrix` | JSON array for `strategy.matrix` |
| `count` | Number of affected crates |
| `custom-categories` | JSON object of custom category matches |

**Conditional Jobs with Custom Categories:**

```yaml
jobs:
  detect:
    outputs:
      run-bench: ${{ contains(steps.affected.outputs.custom-categories, 'benchmarks') }}
    steps:
      - uses: actions/checkout@v4
        with: { fetch-depth: 0 }
      - uses: loadingalias/cargo-rail-action@v1
        id: affected

  bench:
    needs: detect
    if: needs.detect.outputs.run-bench == 'true'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo bench
```

#### Output Formats

The `affected` command supports multiple output formats via `--format`:

| Format | Use Case | Example |
|--------|----------|---------|
| `text` | Human debugging | `direct: 2\n  lib-a` |
| `json` | Scripting | `{"impact": {"direct": ["lib-a"]}}` |
| `github` | `$GITHUB_OUTPUT` | `crates=lib-a lib-b` |
| `github-matrix` | `strategy.matrix` | `{"include": [{"crate": "lib-a"}]}` |
| `names-only` | Shell loops | `lib-a\nlib-b` |
| `cargo-args` | Direct cargo use | `-p lib-a -p lib-b` |

---

### [crates.NAME] Configuration

Per-crate configuration. Replace `NAME` with the actual crate name from `Cargo.toml`.

#### [crates.NAME.split]

Crate splitting and syncing configuration. Enables extracting crates to separate repositories.

| Option | Type | Required | Description |
|--------|------|----------|-------------|
| `remote` | `string` | yes | Remote repository URL (git) or local path (for testing). |
| `branch` | `string` | yes | Git branch to sync with. |
| `mode` | `enum` | yes | Split mode:<br>• `"single"` - One crate per repository<br>• `"combined"` - Multiple crates in one repository |
| `workspace_mode` | `enum` | no | For `mode = "combined"` only:<br>• `"standalone"` - Multiple standalone crates<br>• `"workspace"` - Workspace structure with root Cargo.toml |
| `paths` | `CratePath[]` | yes | Crate paths to include. Format: `[{ crate = "path/to/crate" }]`<br>• `mode = "single"` requires exactly 1 path<br>• `mode = "combined"` requires 2+ paths |
| `include` | `string[]` | no | Additional files/directories to include in the split (e.g., `["LICENSE", "README.md"]`) |
| `exclude` | `string[]` | no | Files/directories to exclude from the split |

**Choosing a Mode:**

| Scenario | Mode | Result |
|----------|------|--------|
| Publish one crate independently | `single` | Files at repo root, standalone Cargo.toml |
| Group related utility crates | `combined` + `standalone` | Preserves directory structure, independent crates |
| Extract as sub-workspace | `combined` + `workspace` | Root Cargo.toml with `[workspace]` |

**Single Crate Example:**

```toml
[crates.my-lib.split]
remote = "git@github.com:org/my-lib.git"
branch = "main"
mode = "single"
paths = [
    { crate = "crates/my-lib" }
]
include = ["LICENSE", "README.md"]
exclude = ["*.tmp"]
```

**Combined Workspace Example:**

```toml
[crates.utils.split]
remote = "git@github.com:org/utils-mono.git"
branch = "main"
mode = "combined"
workspace_mode = "workspace"
paths = [
    { crate = "crates/string-utils" },
    { crate = "crates/io-utils" },
    { crate = "crates/math-utils" }
]
include = ["LICENSE"]
```

**Local Testing:**

```toml
[crates.test-crate.split]
remote = "/tmp/test-split-repo"  # Local path for testing
branch = "main"
mode = "single"
paths = [{ crate = "crates/test-crate" }]
```

#### [crates.NAME.release]

Per-crate release configuration. Overrides workspace-level release defaults.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `publish` | `bool` | `true` | Enable/disable publishing for this crate. Overrides `Cargo.toml` `publish` field. |

**Example:**

```toml
[crates.internal-utils.release]
publish = false  # Never publish to crates.io
```

#### [crates.NAME.changelog]

Per-crate changelog configuration.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `path` | `PathBuf` | | Custom changelog path for this crate. Overrides workspace-level `changelog_path`. Interpreted according to `release.changelog_relative_to`. |
| `skip` | `bool` | `false` | Exclude this crate from changelog generation entirely. |

**Example:**

```toml
[crates.my-lib.changelog]
path = "CHANGES.md"       # Use CHANGES.md instead of CHANGELOG.md
skip = false

[crates.private-crate.changelog]
skip = true               # No changelog for internal crates
```

#### Syncing Split Repositories

After initial split, use `cargo rail sync` for bidirectional synchronization:

```bash
cargo rail sync my-lib                # Auto-detect direction
cargo rail sync my-lib --to-remote    # Monorepo → split repo
cargo rail sync my-lib --from-remote  # Split repo → monorepo (PR branch)
```

**Key behaviors:**

- **Idempotent**: Uses git-notes to track synced commits; re-running only processes new commits
- **PR branch protection**: `--from-remote` creates `cargo-rail-sync-<crate>` branch, never commits to main
- **Conflict resolution**: `--strategy` controls merge behavior (`manual`, `ours`, `theirs`, `union`)

---

## Complete Configuration Example

```toml
# Complete rail.toml showing all available options

# Top-level: Multi-target support
targets = [
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
]

# Dependency unification
[unify]
# Core
msrv = true
msrv_source = "max"  # "deps" | "workspace" | "max"
enforce_msrv_inheritance = false
detect_unused = true
remove_unused = true
prune_dead_features = true
preserve_features = []  # Glob patterns: ["unstable-*", "future-api"]
detect_undeclared_features = true  # Catch borrowed features
fix_undeclared_features = true     # Auto-fix them
skip_undeclared_patterns = ["default", "std", "alloc", "*_backend", "*_impl"]
max_backups = 3

# Version handling
strict_version_compat = true
exact_pin_handling = "warn"
major_version_conflict = "warn"

# Dependency selection
include_paths = true
include_renamed = false
exclude = ["openssl", "windows-sys"]
include = []

# Transitive pinning (workspace-hack replacement)
pin_transitives = false
transitive_host = "root"

# Release automation
[release]
tag_prefix = "v"
tag_format = "{crate}-{prefix}{version}"
require_clean = true
publish_delay = 5
create_github_release = true
sign_tags = true

# Changelog
changelog_path = "CHANGELOG.md"
changelog_relative_to = "crate"
skip_changelog_for = []
require_changelog_entries = true

# Change detection
[change-detection]
infrastructure = [
    ".github/**",
    "scripts/**",
    "justfile",
    "Makefile",
    "*.sh",
    "Cargo.lock",
]

[change-detection.custom]
verify = ["verify/**/*.rs"]
benchmarks = ["benches/**"]
docs = ["docs/**", "*.md"]

# Per-crate configuration
[crates.my-lib]

[crates.my-lib.split]
remote = "git@github.com:org/my-lib.git"
branch = "main"
mode = "single"
paths = [
    { crate = "crates/my-lib" }
]
include = ["LICENSE", "README.md"]
exclude = []

[crates.my-lib.release]
publish = true

[crates.my-lib.changelog]
path = "CHANGELOG.md"
skip = false

[crates.internal-utils]

[crates.internal-utils.release]
publish = false

[crates.internal-utils.changelog]
skip = true
```

## Configuration Recipes

### Minimal (Defaults)

Let cargo-rail handle everything with sensible defaults:

```toml
targets = ["x86_64-unknown-linux-gnu"]
```

### Workspace-Hack Replacement

Replace `cargo-hakari` with cargo-rail's transitive pinning:

```toml
[unify]
pin_transitives = true
transitive_host = "root"
```

### Aggressive Version Unification

Force unification to highest versions, accept breaking changes:

```toml
[unify]
major_version_conflict = "bump"
strict_version_compat = false
exact_pin_handling = "preserve"
```

### Conservative (Minimal Changes)

Disable automatic cleanup and MSRV management:

```toml
[unify]
prune_dead_features = false
remove_unused = false
msrv = false
detect_unused = true  # Still detect, just don't remove
```

### Multi-Platform with Exclusions

Handle platform-specific dependencies:

```toml
targets = [
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
]

[unify]
exclude = [
    "openssl",      # Linux-specific
    "windows-sys",  # Windows-specific
    "core-foundation"  # macOS-specific
]
```

### Full CI Setup

Complete configuration for automated releases and testing:

```toml
targets = ["x86_64-unknown-linux-gnu"]

[unify]
pin_transitives = true
msrv = true
detect_unused = true
remove_unused = true
prune_dead_features = true

[release]
tag_prefix = "v"
tag_format = "{crate}-{prefix}{version}"
require_clean = true
require_changelog_entries = true
create_github_release = true
sign_tags = true

[change-detection]
infrastructure = [".github/**", "justfile", "Cargo.lock"]

[change-detection.custom]
benchmarks = ["benches/**"]

[crates.my-lib.split]
remote = "git@github.com:org/my-lib.git"
branch = "main"
mode = "single"
paths = [{ crate = "crates/my-lib" }]
include = ["LICENSE", "README.md"]
```

### Split Repository Sync

Bidirectional sync between monorepo and split repositories:

```toml
[crates.frontend.split]
remote = "git@github.com:org/frontend.git"
branch = "main"
mode = "combined"
workspace_mode = "workspace"
paths = [
    { crate = "crates/ui" },
    { crate = "crates/components" }
]
include = ["assets/**", "LICENSE"]
exclude = ["*.tmp", ".DS_Store"]

[crates.backend.split]
remote = "git@github.com:org/backend.git"
branch = "main"
mode = "single"
paths = [{ crate = "crates/server" }]
```

## Validation

cargo-rail provides comprehensive configuration validation via `cargo rail config validate`:

```bash
cargo rail config validate              # Validate rail.toml
cargo rail config validate --strict     # Treat warnings as errors
cargo rail config validate --no-strict  # Force warnings-only mode
cargo rail config validate -f json      # JSON output for CI integration
```

### What Gets Validated

1. **Syntax** - TOML parse errors with line/column information
2. **Unknown keys** - Typos like `mrsv_source` instead of `msrv_source`
3. **Semantic validation** - Split config requirements, target triple formats
4. **Deprecation warnings** - Future-proofing for config migrations

### CI Auto-Strict Mode

By default, validation runs in **strict mode** when CI is detected (via `CI`, `GITHUB_ACTIONS`, `GITLAB_CI`, or `CIRCLECI` environment variables):

- **In CI**: Unknown keys and other warnings become errors (exit code 2)
- **Locally**: Unknown keys are warnings only

Override with `--strict` or `--no-strict` flags.

### Example CI Usage

```yaml
# .github/workflows/ci.yml
- name: Validate config
  run: cargo rail config validate
  # Auto-strict in CI - fails on unknown keys
```

### Common Validation Errors

| Error | Cause |
|-------|-------|
| TOML parse error at line X | Syntax error (missing quotes, invalid structure) |
| Unknown top-level key 'foo' | Typo in section name |
| Unknown key 'bar' in [unify] | Typo in field name or deprecated option |
| Missing required field: remote | Split config without remote URL |
| 'foo' doesn't look like a valid target | Target triple missing architecture separator |

## Migration

### From cargo-hakari

Replace `cargo hakari generate` with cargo-rail:

```toml
# Before (cargo-hakari)
# [workspace.dependencies]
# hakari = { version = "0.1.0", path = "hakari" }

# After (cargo-rail)
[unify]
pin_transitives = true
transitive_host = "root"  # or a path to a workspace member crate (relative to workspace root)
```

Then run:

```bash
cargo rail unify
```

### From release-plz

cargo-rail provides similar functionality with tighter integration:

```toml
# release-plz.toml → rail.toml
[release]
tag_format = "{crate}-{prefix}{version}"
require_changelog_entries = true
create_github_release = true
```

## Environment Variables

No cargo-rail-specific environment variables are required. For reproducibility, configuration is file-based.

Note: `cargo rail config validate` defaults to strict mode in CI (detected via `CI`, `GITHUB_ACTIONS`, `GITLAB_CI`, or `CIRCLECI`).

## See Also

- [Commands Reference](./commands.md) - All cargo-rail commands
- [Migration Guide](./migrate-hakari.md) - Migrating from cargo-hakari
- [README](../README.md) - Project overview and quick start
