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
pin_transitives = true
detect_unused = false
remove_unused = false
msrv = false
exclude = ["problem-dep"]
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `include_paths` | bool | `true` | Unify path dependencies |
| `include_renamed` | bool | `false` | Include `package = "..."` deps |
| `pin_transitives` | bool | `true` | Pin fragmented transitives (hakari replacement) |
| `transitive_host` | string | `"root"` | Where to put pinned dev-deps |
| `exclude` | string[] | `[]` | Dependencies to skip |
| `include` | string[] | `[]` | Dependencies to force-unify |
| `max_backups` | int | `3` | Backups to keep |
| `msrv` | bool | `false` | Compute and write MSRV |
| `strict_version_compat` | bool | `true` | Error on version conflicts |
| `exact_pin_handling` | enum | `"warn"` | `"skip"`, `"preserve"`, `"warn"` |
| `detect_unused` | bool | `false` | Detect unused dependencies |
| `remove_unused` | bool | `false` | Auto-remove unused (requires detect_unused) |

---

## [release]

Release automation settings.

```toml
[release]
tag_format = "{crate}@{version}"
create_github_release = true
publish_delay = 5
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `tag_prefix` | string | `""` | Git tag prefix |
| `tag_format` | string | `"{crate}@{version}"` | Tag template (`{crate}`, `{version}`, `{prefix}`) |
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
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `infrastructure` | string[] | see below | Patterns triggering full rebuild |

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
include = ["LICENSE"]
```

| Option | Type | Required | Description |
|--------|------|----------|-------------|
| `remote` | string | yes | Remote repository URL |
| `branch` | string | yes | Git branch |
| `mode` | enum | yes | `"single"`, `"multi"`, `"workspace"` |
| `paths` | array | no | Crate paths for combined mode |
| `include` | string[] | no | Additional files to include |
| `exclude` | string[] | no | Files to exclude |

### [crates.NAME.sync]

```toml
[crates.my-crate.sync]
conflict_strategy = "manual"
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `conflict_strategy` | enum | `"manual"` | `"ours"`, `"theirs"`, `"manual"`, `"union"` |

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
# Just enable unification with defaults
[unify]
```

### CI-optimized monorepo

```toml
targets = ["x86_64-unknown-linux-gnu"]

[unify]
pin_transitives = true
detect_unused = true
remove_unused = true

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
pin_transitives = true
msrv = true
exclude = ["openssl"]  # platform-specific, skip
```
