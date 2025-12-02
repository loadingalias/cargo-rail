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
| `targets` | string[] | auto-detected | Target triples for multi-target resolution. cargo-rail runs `cargo metadata --filter-platform` for each. |

---

## [unify]

Dependency unification settings. Controls how `cargo rail unify` analyzes and modifies your workspace.

```toml
[unify]
pin_transitives = true
exclude = ["problem-dep"]
major_version_conflict = "warn"
```

### Core Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `msrv` | bool | `true` | Compute MSRV from resolved graph and write to `[workspace.package].rust-version`. The MSRV is the maximum `rust-version` declared by any package in your dependency tree - your buildable floor. |
| `detect_unused` | bool | `true` | Detect dependencies declared but absent from resolved graph. |
| `remove_unused` | bool | `true` | Auto-remove unused deps (requires `detect_unused`). |
| `prune_dead_features` | bool | `true` | Remove features never enabled in resolved graph. Only prunes empty no-ops (`feature = []`). Features that enable something are preserved. |
| `pin_transitives` | bool | `false` | Pin transitive-only deps with fragmented features. Replaces workspace-hack crates. Enable if you use cargo-hakari. |

### Version Handling

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `major_version_conflict` | enum | `"warn"` | How to handle multiple major versions of the same dep. `"warn"`: skip unification for that dep. `"bump"`: force unify to highest resolved version (may require code fixes). |
| `strict_version_compat` | bool | `true` | Treat version mismatches between member manifests and existing `workspace.dependencies` as errors. Set `false` for warnings only. |
| `exact_pin_handling` | enum | `"warn"` | How to handle `=x.y.z` pins. `"skip"`: exclude from unification. `"preserve"`: keep exact pin. `"warn"`: convert to caret with warning. |

### Dependency Selection

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `include_paths` | bool | `true` | Include path dependencies in unification. |
| `include_renamed` | bool | `false` | Include renamed deps (`package = "..."`). When enabled, features are aggregated across all variants of the same package using union. |
| `exclude` | string[] | `[]` | Dependencies to skip from unification. |
| `include` | string[] | `[]` | Force-include single-use dependencies. |

### Transitive Pinning

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `transitive_host` | string | `"root"` | Where to put pinned transitive dev-deps. `"root"` for workspace root, or a path like `"crates/foo"`. |

### Other

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `max_backups` | int | `3` | Number of backups to keep. Older backups auto-pruned. |

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
| `tag_format` | string | `"{crate}-{prefix}{version}"` | Tag template. Variables: `{crate}`, `{version}`, `{prefix}` |
| `require_clean` | bool | `true` | Require clean working directory |
| `publish_delay` | int | `5` | Seconds between publishes |
| `create_github_release` | bool | `false` | Create GitHub release via `gh` CLI |
| `sign_tags` | bool | `false` | GPG/SSH sign tags |
| `changelog_path` | string | `"CHANGELOG.md"` | Default changelog path |
| `changelog_relative_to` | enum | `"crate"` | `"crate"` or `"workspace"` |
| `skip_changelog_for` | string[] | `[]` | Crates to skip changelog generation |
| `require_changelog_entries` | bool | `false` | Error if no changelog entries |

---

## [change-detection]

Settings for the `affected` and `test` commands.

```toml
[change-detection]
infrastructure = [".github/**", "justfile"]

[change-detection.custom]
verify = ["verify/**/*.rs"]
benchmarks = ["benches/**", "perf/**"]
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `infrastructure` | string[] | see below | Glob patterns triggering full rebuild |
| `custom` | table | `{}` | Custom category patterns for conditional CI |

Default infrastructure patterns:
```
.github/**, scripts/**, justfile, Makefile, *.sh, deny.toml
```

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
| `workspace_mode` | enum | no | For combined: `"standalone"` or `"workspace"` |
| `paths` | array | yes | Crate paths: `[{ crate = "path/to/crate" }]` |
| `include` | string[] | no | Additional files to include |
| `exclude` | string[] | no | Files to exclude |

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
# Run `cargo rail init` - defaults are sensible
# msrv, detect_unused, remove_unused, prune_dead_features all default to true
```

### Workspace-hack replacement

```toml
[unify]
pin_transitives = true
```

### Aggressive version unification

```toml
[unify]
major_version_conflict = "bump"  # Force highest version even across majors
strict_version_compat = false    # Allow version mismatches
```

### Conservative (minimal changes)

```toml
[unify]
prune_dead_features = false
remove_unused = false
msrv = false
```

### Multi-target with exclusions

```toml
targets = [
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
]

[unify]
exclude = ["openssl"]  # Platform-specific, skip
```

### Full CI setup

```toml
targets = ["x86_64-unknown-linux-gnu"]

[unify]
pin_transitives = true

[release]
require_changelog_entries = true
create_github_release = true

[change-detection]
infrastructure = [".github/**", "justfile", "Cargo.lock"]

[crates.my-lib.split]
remote = "git@github.com:org/my-lib.git"
branch = "main"
mode = "single"
include = ["LICENSE", "README.md"]
```
