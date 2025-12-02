# Configuration

cargo-rail is configured via `rail.toml`. Generate one with:

```bash
cargo rail init
```

## Location

Searched in order:
1. `rail.toml`
2. `.rail.toml`
3. `.cargo/rail.toml`
4. `.config/rail.toml`

---

## Top-level

```toml
targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `targets` | string[] | auto-detected | Target triples for multi-target resolution |

---

## [unify]

Dependency unification settings.

```toml
[unify]
pin_transitives = true   # Enable for hakari/workspace-hack users
exclude = ["problem-dep"]
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `include_paths` | bool | `true` | Unify path dependencies |
| `include_renamed` | bool | `false` | Include `package = "..."` deps |
| `pin_transitives` | bool | `false` | Pin fragmented transitives (enable for hakari users) |
| `transitive_host` | string | `"root"` | Where to put pinned dev-deps |
| `exclude` | string[] | `[]` | Dependencies to skip |
| `include` | string[] | `[]` | Dependencies to force-unify |
| `max_backups` | int | `3` | Backups to keep |
| `msrv` | bool | `true` | Compute and write MSRV |
| `strict_version_compat` | bool | `true` | Error on version conflicts |
| `exact_pin_handling` | enum | `"warn"` | `"skip"`, `"preserve"`, `"warn"` |
| `detect_unused` | bool | `true` | Detect unused dependencies |
| `remove_unused` | bool | `true` | Auto-remove unused (requires detect_unused) |
| `prune_dead_features` | bool | `true` | Remove features never enabled in graph |

---

## [release]

Release automation settings.

```toml
[release]
tag_format = "{crate}-{prefix}{version}"
create_github_release = true
publish_delay = 5
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `tag_prefix` | string | `"v"` | Git tag prefix (used via `{prefix}`) |
| `tag_format` | string | `"{crate}-{prefix}{version}"` | Tag template (`{crate}`, `{version}`, `{prefix}`) |
| `require_clean` | bool | `true` | Require clean working directory |
| `publish_delay` | int | `5` | Seconds between publishes |
| `create_github_release` | bool | `false` | Create GitHub release via `gh` |
| `sign_tags` | bool | `false` | GPG/SSH sign tags |
| `changelog_path` | string | `"CHANGELOG.md"` | Default changelog path |
| `changelog_relative_to` | enum | `"crate"` | `"crate"` or `"workspace"` |
| `skip_changelog_for` | string[] | `[]` | Crates to skip changelog |
| `require_changelog_entries` | bool | `false` | Error if no entries |

---

## [change-detection]

Settings for the `affected` command.

```toml
[change-detection]
infrastructure = [".github/**", "justfile"]

# Custom categories for specialized file patterns
[change-detection.custom]
verify = ["verify/**/*.rs"]
benchmarks = ["benches/**", "perf/**"]
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `infrastructure` | string[] | see below | Patterns triggering full rebuild |
| `custom` | table | `{}` | Custom category patterns (name → glob patterns) |

Default infrastructure patterns:
```
.github/**, scripts/**, justfile, Makefile, *.sh, deny.toml
```

Custom categories appear in the `affected` output and can be used for conditional CI workflows.

---

## [crates.NAME]

Per-crate configuration.

### [crates.NAME.split]

```toml
[crates.my-crate.split]
remote = "git@github.com:org/my-crate.git"
branch = "main"
mode = "single"
paths = [{ crate = "crates/my-crate" }]
include = ["LICENSE"]
```

| Option | Type | Required | Description |
|--------|------|----------|-------------|
| `remote` | string | yes | Remote repository URL |
| `branch` | string | yes | Git branch |
| `mode` | enum | yes | `"single"` or `"combined"` |
| `workspace_mode` | enum | no | For combined: `"standalone"` (default) or `"workspace"` |
| `paths` | array | yes | Crate paths: `[{ crate = "path/to/crate" }]` |
| `include` | string[] | no | Additional files to include |
| `exclude` | string[] | no | Files to exclude |

### [crates.NAME.sync]

> **Note:** Reserved for future use. Currently has no effect.
> Conflict strategy is specified via CLI: `cargo rail sync --strategy <ours|theirs|manual|union>`

### [crates.NAME.release]

```toml
[crates.my-crate.release]
publish = false
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `publish` | bool | `true` | Enable publishing to crates.io |

### [crates.NAME.changelog]

```toml
[crates.my-crate.changelog]
path = "CHANGES.md"
skip = true
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `path` | string | | Custom changelog path |
| `skip` | bool | `false` | Skip changelog generation |

---

## Examples

### Minimal

```toml
# Just run `cargo rail init` - defaults are sensible
# msrv, detect_unused, remove_unused, prune_dead_features all default to true
```

### Hakari/workspace-hack users

```toml
[unify]
pin_transitives = true  # Enable transitive pinning (replaces workspace-hack)
```

### With renamed dependencies

```toml
[unify]
include_renamed = true  # Handle package = "..." renames
```

### CI-optimized monorepo

```toml
targets = ["x86_64-unknown-linux-gnu"]

[release]
require_changelog_entries = true
create_github_release = true

[change-detection]
infrastructure = [".github/**", "justfile", "Cargo.lock"]
```

### With split crates

```toml
[crates.my-lib.split]
remote = "git@github.com:org/my-lib.git"
branch = "main"
mode = "single"
include = ["LICENSE", "README.md"]

[crates.my-lib.release]
publish = true

[crates.internal-utils.release]
publish = false
```

### Multi-target workspace

```toml
targets = [
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
]

[unify]
exclude = ["openssl"]  # platform-specific, skip
```
