# Configuration Reference

`rail.toml` configures dependency analysis, releases, change detection, execution profiles, and split repositories.

The file controls:

- **Dependency unification** (`cargo rail unify`)
- **Release automation** (`cargo rail release`)
- **Change planning + execution** (`cargo rail plan`, `cargo rail run`)
- **Crate splitting** (`cargo rail split`)

## File discovery

Configuration files are searched in order:

1. `rail.toml` (workspace root)
2. `.rail.toml` (workspace root, hidden)
3. `.cargo/rail.toml` (cargo directory)
4. `.config/rail.toml` (config directory)

The first file found is used. All paths are relative to the workspace root.

## Generate a file

Generate a default configuration file:

```bash
cargo rail init                    # Creates .config/rail.toml
cargo rail init -o rail.toml       # Creates rail.toml
cargo rail init --check            # Preview without writing
cargo rail init --force            # Overwrite existing config
```

## Upgrade an existing file

Run the synchronizer after every cargo-rail upgrade:

```bash
cargo rail config sync --check     # Preview new fields and detected targets
cargo rail config sync             # Materialize them without replacing user values
cargo rail config validate --strict
```

Review the sync diff before running mutation commands. Explicit policy keeps an
older workspace from silently inheriting a newly introduced default.

## Start with defaults

### Minimal configuration

An empty file is valid. Add targets when analysis must include platforms unavailable on the current host:

```toml
# Minimal config (optional): set targets if you want multi-target validation.
# (`cargo rail init` can auto-detect targets from *.toml and .github/workflows.)
targets = ["x86_64-unknown-linux-gnu"]
```

### Workspace configuration

```toml
# Multi-target workspace with unify enabled
targets = [
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
]

[unify]
msrv = true                      # Compute MSRV from dependencies
prune_dead_features = true       # Analyze unused features
consumer_scope = "open"          # Use "workspace" to authorize closed-world cleanup
detect_unused = true             # Find unused dependencies
compiler_diag_cache = true       # Reuse rustc diagnostics across runs
remove_unused = true             # Auto-remove them

[release]
tag_format = "{crate}-{prefix}{version}"
create_github_release = false
forge = "auto"

[change-detection]
infrastructure = [".github/**", "justfile"]
```

## Option reference

### Top-level options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `targets` | `string[]` | `[]` | Target triples for multi-platform validation. Used by `unify` and other commands to run `cargo metadata --filter-platform` for each target. Auto-detected by `cargo rail init`. |
| `unify` | `table` | `{}` | Dependency unification settings (see below) |
| `release` | `table` | `{}` | Release management settings (see below) |
| `change-detection` | `table` | `{}` | Change detection settings (see below) |
| `run` | `table` | `{}` | Run profile settings for `cargo rail run` (see below) |
| `crates` | `table` | `{}` | Per-crate configuration (see below) |

Example:

```toml
targets = [
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
]
```

---

### `[unify]`

Controls workspace dependency analysis and manifest rewrites. Every option is optional.

#### Core Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `msrv` | `bool` | `true` | Compute and write MSRV to `[workspace.package].rust-version` (written as `major.minor.patch`). The MSRV is determined by `msrv_source`. |
| `enforce_msrv_inheritance` | `bool` | `false` | Ensure every workspace member inherits MSRV by setting `[package].rust-version = { workspace = true }` in each member's `Cargo.toml`. This makes `[workspace.package].rust-version` actually apply across the workspace. |
| `msrv_source` | `enum` | `"max"` | How to compute the final MSRV:<br>• `"deps"` - Use maximum from dependencies only (original behavior)<br>• `"workspace"` - Preserve existing rust-version, warn if deps need higher<br>• `"max"` - Take max(workspace, deps) - your explicit setting wins if higher |
| `detect_unused` | `bool` | `true` | Detect unused dependencies from the resolved Cargo graph and workspace-only rustc `unused_crate_dependencies` evidence. Checks cover configured targets plus default, no-default, all-feature, and source-derived conditional feature selections. Removal requires complete evidence for the declaration's exact kind and target scope. |
| `compiler_diag_cache` | `bool` | `true` | Cache target- and feature-aware rustc evidence in `target/cargo-rail/cache/compiler-diags-v1.json`. Entries bind the compiler, source, manifest, target, feature selection, and Cargo freshness, so stale compilation units are recollected. Disable to force fresh compiler checks each run. |
| `remove_unused` | `bool` | `true` | Automatically remove unused dependencies during unification. Requires `detect_unused = true`. |
| `prune_dead_features` | `bool` | `true` | Analyze feature reachability. Destructive pruning also requires `consumer_scope = "workspace"`. |
| `consumer_scope` | `enum` | `"open"` | Consumer boundary for `publish = false` packages. `"open"` preserves dormant configuration; `"workspace"` asserts that workspace consumers are complete and permits verified removal. Published packages always remain open-world. |
| `preserve_features` | `string[]` | `[]` | Features to preserve from dead feature pruning. Supports glob patterns (e.g., `"unstable-*"`, `"bench*"`). Use this to keep features intended for future use or external consumers. |
| `detect_undeclared_features` | `bool` | `true` | Detect crates that rely on Cargo's feature unification to "borrow" features from other workspace members. These crates will fail when built standalone after unification. Reports as warnings (or auto-fixes if `fix_undeclared_features` is enabled). |
| `fix_undeclared_features` | `bool` | `true` | Add borrowed features to the member manifest that uses them, allowing the crate to build without another workspace member enabling those features. Requires `detect_undeclared_features = true`. |
| `skip_undeclared_patterns` | `string[]` | `["default", "std", "alloc", "*_backend", "*_impl"]` | Patterns for features to skip in undeclared feature detection. Supports glob patterns. Default patterns filter out features that are typically not actionable (standard library features, internal implementation details). |
| `max_backups` | `usize` | `3` | Maximum number of backup archives to keep. Older backups are automatically cleaned up after successful operations. Set to `0` to disable backup creation entirely. |

Example:

```toml
[unify]
msrv = true
msrv_source = "max"  # "deps" | "workspace" | "max"
enforce_msrv_inheritance = false
detect_unused = true
compiler_diag_cache = true
remove_unused = true
prune_dead_features = true
consumer_scope = "open"  # Set to "workspace" only for a closed monorepo consumer graph
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

Example:

```toml
[unify]
strict_version_compat = false
exact_pin_handling = "preserve"
major_version_conflict = "bump"
```

Operational notes:

- `major_version_conflict = "warn"` preserves incompatible major versions.
- `major_version_conflict = "bump"` selects the highest resolved major and may require source changes.
- If `[workspace.package].rust-version` is missing but root `[package].rust-version` is present, `unify` uses it as the baseline and writes it to `[workspace.package].rust-version` (consider enabling `enforce_msrv_inheritance` to avoid drift)

#### Dependency Selection

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `include_paths` | `bool` | `true` | Include path dependencies in unification. If `false`, path dependencies are excluded. |
| `include_renamed` | `bool` | `false` | Include renamed dependencies (`package = "..."`). When enabled, features are aggregated across all variants using union. Opt-in due to complexity. |
| `exclude` | `string[]` | `[]` | Dependencies to skip from unification (safety hatch). Useful for platform-specific or problematic dependencies. For workspace-member dependency cohorts, excluding one member excludes the full cohort atomically to prevent local-vs-registry splits. |
| `include` | `string[]` | `[]` | Force-include specific dependencies in unification, even if they're single-use. Workspace-member cohorts are auto-included by cargo-rail to avoid threshold-based cohort splits. |

Example:

```toml
[unify]
include_paths = true
include_renamed = false
exclude = ["openssl", "windows-sys"]  # Platform-specific
include = ["my-special-dep"]          # Force include
```

Workspace-member cohorts are unified atomically. cargo-rail rejects a result that would leave some connected members local and resolve sibling members from a registry.

#### Transitive Pinning

Enable transitive pinning only when replacing `cargo-hakari` or another workspace-hack crate.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `pin_transitives` | `bool` | `false` | Pin transitive-only dependencies with fragmented features. This is cargo-rail's workspace-hack replacement. When enabled, transitive deps with multiple feature sets are pinned in `workspace.dependencies`. |
| `transitive_host` | `string` | `"root"` | Where to put pinned transitive dev-dependencies:<br>• `"root"` - Use workspace root `Cargo.toml`<br>• `"crates/foo"` - Use specific member crate (relative path from workspace root) |

Example:

```toml
[unify]
pin_transitives = true
transitive_host = "root"
```

All `[unify]` options:

```toml
[unify]
# Core options (defaults shown)
msrv = true
msrv_source = "max"  # "deps" | "workspace" | "max"
enforce_msrv_inheritance = false
detect_unused = true
compiler_diag_cache = true
remove_unused = true
prune_dead_features = true
consumer_scope = "open"  # "workspace" authorizes closed-world cleanup for private packages
preserve_features = []  # Glob patterns to preserve from pruning
detect_undeclared_features = true
fix_undeclared_features = true
skip_undeclared_patterns = ["default", "std", "alloc", "*_backend", "*_impl"]
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

### `[release]`

Release automation settings for versioning, tagging, and publishing.

#### Core Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `tag_prefix` | `string` | `"v"` | Git tag prefix. Used via `{prefix}` placeholder in `tag_format`. |
| `tag_format` | `string` | `"{crate}-{prefix}{version}"` | Tag template. Available variables:<br>• `{crate}` - Crate name<br>• `{version}` - Version number<br>• `{prefix}` - Value of `tag_prefix` |
| `require_clean` | `bool` | `true` | Require clean working directory before release operations. |
| `publish_delay` | `u64` | `5` | Maximum registry-convergence polling interval in seconds. |
| `create_github_release` | `bool` | `false` | Create forge releases via `gh` or `glab`. Requires `push = true`; cargo-rail pushes the tag first. GitHub releases are created as drafts and published after crates publish; GitLab releases are created directly. |
| `forge` | `enum` | `"auto"` | Release-creation provider when release creation is enabled: `"auto"`, `"github"`, or `"gitlab"`. Auto detects GitHub/GitLab from `origin`; Gitea release creation is not supported. |
| `push` | `bool` | `false` | Push release commits and tags to `origin` before public publishing. Uses an atomic push for the branch and release tags. |
| `sign_tags` | `bool` | `false` | Sign git tags with GPG or SSH. Requires git signing to be configured. |

Example:

```toml
[release]
tag_prefix = "v"
tag_format = "{crate}-{prefix}{version}"    # Produces: my-crate-v1.0.0
require_clean = true
publish_delay = 10
push = true
create_github_release = true
forge = "auto"
sign_tags = true
```

#### Release Hook Context

cargo-rail never bypasses Git hooks. Git commands inherit the caller environment, except repository-redirection variables such as `GIT_DIR`, `GIT_WORK_TREE`, and `GIT_INDEX_FILE`, which cargo-rail removes to preserve the selected workspace boundary.

For every cargo-rail-owned release push, including release PR branches, cargo-rail adds:

```text
CARGO_RAIL_OPERATION=release
CARGO_RAIL_RELEASE_PUSH=1
```

Release commits receive `CARGO_RAIL_OPERATION=release`. Hooks can use these variables to distinguish a release operation from an ordinary developer commit or push. The final branch-and-tag push remains one atomic Git invocation, so `pre-push` runs exactly once.

#### Changelog Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `require_changelog_entries` | `bool` | `false` | If `true`, error when there are no changelog entries for a crate being released. |
| `require_release_notes` | `bool` | `true` | If `true`, preflight fails release apply when the target version has no release notes (`## [<version>]`) and changelog generation produces no entries. Set to `false` to allow note-less releases. |
| `release_notes_dir` | `string` | `"release-notes"` | Directory for manual release body overrides. `v<version>.md` or `<tag>.md` takes precedence over generated notes. |
| `change_dir` | `string` | `".changes"` | Workspace-relative directory for pending release intent files. Set `change_dir = "changes"` if the team prefers a visible directory. |
| `pre_1_breaking_bump` | `enum` | `"minor"` | How `--bump auto` maps breaking changes for `0.x` crates: `"minor"` or `"major"`. |
| `unconventional_commits` | `enum` | `"warn"` | Policy for commits that do not parse as conventional commits: `"allow"`, `"warn"`, or `"deny"`. |
| `semver_check` | `enum` | `"warn"` | Optional `cargo-semver-checks` policy: `"off"`, `"warn"`, or `"deny"`. Only publishable library crates are checked. A confirmed breaking verdict escalates `--bump auto` to major and fails `release check --extended` under `"deny"`; inconclusive runs (no published baseline, network or build failure) report as skipped and never escalate or fail. |
| `require_change_files` | `bool` or `string[]` | `false` | Require `.changes/*.md` coverage, or the configured `change_dir`, for all crates or selected crates. Coverage honors `[release.changelog.filters]`, so crates whose only changes sit in excluded paths are not gated. Consumption is all-or-nothing: a release plan covering only some of a change file's crates is rejected so no pending intent is lost. |
| `version_groups` | `table` | `{}` | Named lockstep groups under `[release.version_groups]`. Group members must be workspace crates and may belong to at most one group. |

Use `cargo rail change check --merge-base` to run the same coverage gate before
release planning; add `--required` to require a change file for every changed
crate regardless of `require_change_files`.

#### [release.version_groups]

Use version groups when a set of crates must always release together at the
same bump level:

```toml
[release.version_groups]
core = ["rail-core", "rail-graph", "rail-git"]
```

With `--bump auto`, cargo-rail computes each member's signal, uses the maximum
level for the whole group, and plans group-only members with
`version group <name> -> <level>`. Explicit partial releases are rejected by
default; pass `--include-dependents` to expand the selection to the whole group.

#### [release.changelog]

Workspace changelog defaults. Per-crate overrides live under
`[crates.NAME.changelog]`.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `path` | `string` | `"CHANGELOG.md"` | Default changelog filename. |
| `relative_to` | `enum` | `"crate"` | Path base: `"crate"` or `"workspace"`. |
| `entry_format` | `string` | `"- {scope}{breaking}{description}{prs} ({sha_link})"` | Entry placeholders: `{scope}`, `{breaking}`, `{description}`, `{prs}`, `{sha}`, `{sha_link}`, `{type}`. |
| `emoji` | `bool` | `true` | Render emoji in section headers. |
| `group_order` | `string[]` | built-ins | Commit type section order. |
| `fallback` | `string` | `"other"` | Type key from `group_order`, or `"skip"`. |
| `commit_url` | `string?` | inferred | Commit URL template with `{sha}`. |
| `pr_url` | `string?` | inferred | Pull-request URL template with `{pr}`. |

GitHub remotes infer `commit_url` and `pr_url` automatically. For GitLab or
self-hosted forges, set explicit templates:

```toml
[release.changelog]
commit_url = "https://gitlab.com/org/repo/-/commit/{sha}"
pr_url = "https://gitlab.com/org/repo/-/merge_requests/{pr}"
```

#### [release.changelog.filters]

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `skip_types` | `string[]` | `[]` | Commit types omitted from changelog output. Breaking commits are exempt — they always render. |
| `skip_scopes` | `string[]` | `[]` | Commit scopes omitted from changelog output. |
| `include_paths` | `string[]` | `[]` | Optional attribution include globs. Empty means all resolver-owned crate paths. |
| `exclude_paths` | `string[]` | `[]` | Optional attribution exclude globs. |

Path filters are authoritative: a commit scope naming a workspace crate can
claim an otherwise unattributed commit (for example one that only touches
workspace infrastructure), but never one whose files were excluded by
`include_paths`/`exclude_paths`.

Example:

```toml
[release]
require_changelog_entries = true
release_notes_dir = "release-notes"
change_dir = ".changes"
pre_1_breaking_bump = "minor"
unconventional_commits = "warn"
semver_check = "warn"
require_change_files = false

[release.changelog]
path = "CHANGELOG.md"
relative_to = "crate"
group_order = ["breaking", "feat", "fix", "perf", "docs", "deps", "other"]
fallback = "other"

[release.changelog.filters]
skip_types = ["chore", "ci"]
skip_scopes = []
include_paths = []
exclude_paths = []
```

Workspace release defaults:

```toml
[release]
# Core
tag_prefix = "v"
tag_format = "{crate}-{prefix}{version}"
require_clean = true
publish_delay = 5
push = false
create_github_release = false
forge = "auto"
sign_tags = false

# Changelog
require_changelog_entries = false
require_release_notes = true
release_notes_dir = "release-notes"
change_dir = ".changes"
pre_1_breaking_bump = "minor"
unconventional_commits = "warn"
semver_check = "warn"
require_change_files = false

[release.changelog]
path = "CHANGELOG.md"
relative_to = "crate"
entry_format = "- {scope}{breaking}{description}{prs} ({sha_link})"
emoji = true
group_order = ["breaking", "feat", "fix", "build", "chore", "ci", "deps", "docs", "other", "perf", "refactor", "style", "test"]
fallback = "other"

[release.changelog.filters]
skip_types = []
skip_scopes = []
include_paths = []
exclude_paths = []
```

Operational notes:

- In monorepos, use `{crate}` in `tag_format` to avoid tag collisions
- For single-crate workspaces, use `tag_format = "v{version}"`
- `relative_to = "workspace"` under `[release.changelog]` is useful for unified changelogs
- `create_github_release = true` with `push = false` is rejected because forges may create a tag from the wrong commit.
- With `forge = "gitlab"`, release creation uses `glab release create`. Gitea release creation is unsupported; tags and registry publish still work.
- Put curated release notes in `release-notes/v1.2.3.md` when generated notes are too large or too noisy.

---

### `[change-detection]`

Settings for planner path classification.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `infrastructure` | `string[]` | see below | Path patterns treated as infra changes. |
| `unknown_file_policy` | `enum` | `"strict"` | Unknown-file policy:<br>• `"docs"` - keep unknown files docs-only<br>• `"owned_build_test"` - crate-owned unknown files enable `build` + `test`<br>• `"workspace_infra"` - non-crate unknown files enable `infra`<br>• `"strict"` - crate-owned unknown files enable `build` + `test`; everything else enables `infra` |
| `confidence_profile` | `enum` | `"balanced"` | Planner confidence profile:<br>• `"strict"` - expands crate-owned changes to conservative `build` + `test` with transitive seeding<br>• `"balanced"` - default behavior<br>• `"fast"` - disables conservative transitive surface seeding for speed |
| `bot_pr_confidence_profile` | `enum?` | `unset` | Optional profile override applied only for bot-authored GitHub pull requests (for example set to `"strict"`). |
| `custom` | `table<string, string[]>` | `{}` | Custom path patterns. Emits `custom:<name>` surfaces for CI gating. Custom surfaces are invalid in `[run.profile.X].surfaces`. Names accept ASCII letters, digits, `_`, and `-`. |

Default infrastructure patterns:

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

Example:

```toml
[change-detection]
infrastructure = [
    ".github/**",
    "justfile",
    "Cargo.lock",
    "rust-toolchain.toml"
]
confidence_profile = "balanced"
bot_pr_confidence_profile = "strict"  # optional; only active for bot PRs

[change-detection.custom]
verify = ["verify/**/*.rs"]
benchmarks = ["benches/**", "perf/**"]
docs = ["docs/**", "*.md"]
```

#### Built-in and custom surfaces

Custom surfaces serve a different purpose than built-in surfaces:

| Aspect | Built-in Surfaces | Custom Surfaces |
|--------|-------------------|-----------------|
| Values | `build`, `test`, `bench`, `docs`, `infra` | `custom:<name>` (user-defined) |
| Use in `[run.profile.X].surfaces` | ✅ Yes | ❌ No |
| Appears in plan output | ✅ Yes | ✅ Yes |
| Use in CI job gating | ✅ Yes | ✅ Yes (via `scope_json.surfaces` or action `custom_<name>` outputs) |

Built-in surfaces map to Cargo commands. Custom surfaces classify repository-specific work for CI gates and have no command for `cargo rail run` to execute.

Custom surfaces are additive. If a path also matches a built-in classification such as
`infra`, `docs`, or `bench`, `cargo rail plan` enables both the built-in surface and the
matching `custom:<name>` surface(s). Custom routing is an overlay, not a replacement for
core planner semantics.

Custom-surface gate:

```yaml
- uses: loadingalias/cargo-rail-action@v5.1.0
  id: rail
  with:
    version: 0.17.1

- name: Run benchmark suite
  if: steps.rail.outputs.custom_benchmarks == 'true' || steps.rail.outputs.infra == 'true'
  run: cargo bench
```

#### GitHub Actions outputs

Write planner outputs directly to `$GITHUB_OUTPUT`:

```yaml
- uses: actions/checkout@v6
  with:
    fetch-depth: 0

- name: Build plan outputs
  id: plan
  run: cargo rail plan --merge-base -f github >> "$GITHUB_OUTPUT"

- name: Test selected crates
  if: steps.plan.outputs.test == 'true'
  run: cargo rail run --merge-base --profile ci

- name: Docs pipeline
  if: steps.plan.outputs.docs == 'true'
  run: cargo rail run --merge-base --surface docs
```

See [Change Detection](change-detection.md) for planner behavior and validation commands.

`plan -f github` outputs:

| Output | Description |
|--------|-------------|
| `build` | `"true"` when build surface is enabled |
| `test` | `"true"` when test surface is enabled |
| `bench` | `"true"` when bench surface is enabled |
| `docs` | `"true"` when docs surface is enabled |
| `infra` | `"true"` when infra surface is enabled |
| `base_ref` | Resolved baseline ref used for change detection |
| `cargo_args` | Cargo package selection from `scope` (`--workspace`, `-p crate ...`, or empty) |
| `scope_json` | Compact execution handoff emitted by the planner |

Use `cargo rail plan -f github-debug` when you also need `plan_json` for debugging or incident review.

#### Decision receipts

`cargo rail run` writes a decision receipt under `target/cargo-rail/receipts/`. Upload receipts when CI investigations need the exact plan and command selection:

```yaml
- name: Run targeted surfaces
  run: cargo rail run --merge-base --profile ci

- name: Upload rail receipts
  if: always()
  uses: actions/upload-artifact@v6
  with:
    name: cargo-rail-receipts
    path: target/cargo-rail/receipts/*.json
    if-no-files-found: ignore
```

#### Replace coarse outputs

| Legacy pattern | Planner-first replacement |
|----------------|---------------------------|
| `docs-only == true` | `docs == true` and `test == false` |
| `rebuild-all == true` | `infra == true` |
| surface dispatch | `cargo rail run --surface test` |
| `custom-categories` checks | Parse `scope_json.surfaces["custom:<name>"]` or use action `custom_<name>` outputs |

#### Output Formats

Planner outputs support four formats:

| Format | Use Case | Example |
|--------|----------|---------|
| `text` | Human-readable summary | `plan\nsurfaces: build, test` |
| `json` | Full machine contract | `{"files":[...],"surfaces":{...}}` |
| `github` | GitHub key/value outputs | `test=true` |
| `github-debug` | GitHub outputs plus `plan_json` | `plan_json={...}` |

---

### `[run]`

Execution profile configuration for `cargo rail run`.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `default_profile` | `string?` | `unset` | Default profile when no `--surface`/`--profile` is passed. |
| `profile` | `table` | `{}` | User-defined profile map: `[run.profile.<name>]`. |
| `workflow` | `table<string,string>` | `{}` | Optional workflow-name to profile-name mapping for CI wrappers. |

Built-in profiles:

- `local` -> `["test"]`
- `ci` -> `["build", "test"]`
- `nightly` -> `["build", "test", "docs"]`

Precedence:

1. `--surface` overrides all profile selection.
2. `--profile` overrides `run.default_profile`.
3. `run.default_profile` overrides built-in fallback (`local`).

User-defined profile schema:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `surfaces` | `string[]` | yes | Built-in run surfaces to execute: `build`, `test`, `bench`, `docs`. **Note:** `infra` and custom surfaces (defined in `[change-detection.custom]`) are planner outputs for CI job gating, not profile inputs. |
| `run_args` | `string[]` | no | Args prepended before CLI `RUN_ARGS`. For test surfaces these are test-binary arguments; cargo-rail inserts `--`. |
| `since` | `string?` | no | Default `--since` baseline when CLI does not pass `--since`/`--merge-base`. |
| `merge_base` | `bool?` | no | Default merge-base mode when CLI does not pass `--since`/`--merge-base`. |

Token substitutions (stable expansion order):

1. `{workspace_root}`
2. `{base_ref}`
3. `{cargo_args}` (only valid in `run_args`)

Allowed tokens by field:

- `run.profile.<name>.run_args`: `{workspace_root}`, `{base_ref}`, `{cargo_args}`
- `run.profile.<name>.since`: `{workspace_root}`, `{base_ref}`

Example:

```toml
[run]
default_profile = "ci"

[run.workflow]
commit = "ci"
nightly = "nightly"
bench-weekly = "bench_weekly"

[run.profile.bench]
surfaces = ["bench"]
run_args = ["--", "--bench", "core"]
since = "origin/main"

[run.profile.bench_weekly]
surfaces = ["bench"]
run_args = ["--", "--bench", "critical", "{cargo_args}"]
since = "{base_ref}"
```

Test runner options are not inferred from `run_args`. Pass one portable `--test-filter`, Cargo-only options
with repeated `--cargo-test-arg`, and nextest-only options with repeated `--nextest-arg`. Trailing `RUN_ARGS` are always
test-binary arguments for the test surface. Backend-specific options select their matching backend in `auto` mode and fail
before execution if they conflict with `--test-runner` or the backend is unavailable.

---

### `[crates.NAME]`

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

Split modes:

| Scenario | Mode | Result |
|----------|------|--------|
| Publish one crate independently | `single` | Files at repo root, standalone Cargo.toml |
| Group related utility crates | `combined` + `standalone` | Preserves directory structure, independent crates |
| Extract as sub-workspace | `combined` + `workspace` | Root Cargo.toml with `[workspace]` |

Single-crate example:

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

Combined-workspace example:

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

Local-path example:

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

Example:

```toml
[crates.internal-utils.release]
publish = false  # Never publish to crates.io
```

#### [crates.NAME.changelog]

Per-crate changelog configuration.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `path` | `PathBuf` | | Custom changelog path for this crate. Overrides `[release.changelog] path`. |
| `relative_to` | `enum` | inherited | Path base: `"crate"` or `"workspace"`. |
| `skip` | `bool` | `false` | Exclude this crate from changelog generation entirely. |
| `entry_format` | `string` | inherited | Override entry rendering for this crate. |
| `emoji` | `bool` | inherited | Override section-header emoji rendering. |
| `group_order` | `string[]` | inherited | Override section order. |
| `fallback` | `string` | inherited | Override fallback section or `"skip"`. |
| `filters` | `table` | inherited | Override changelog filters for this crate. |
| `commit_url` | `string?` | inherited | Override commit URL template. |
| `pr_url` | `string?` | inherited | Override pull-request URL template. |

Example:

```toml
[crates.my-lib.changelog]
path = "CHANGES.md"       # Use CHANGES.md instead of CHANGELOG.md
relative_to = "crate"
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

Sync behavior:

- Git notes track mapped commits; repeated runs process only new commits.
- `--from-remote` creates `cargo-rail-sync-<crate>` instead of committing to the current main branch.
- `--strategy` selects `manual`, `ours`, `theirs`, or `union` three-way merge behavior.
- Unresolved content exits `1` without committing. Resolve the receipt paths and run `cargo rail sync --resume <receipt>`.

---

## Full configuration example

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
compiler_diag_cache = true
remove_unused = true
prune_dead_features = true
consumer_scope = "open"
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
push = true
create_github_release = true
forge = "auto"
sign_tags = true

# Changelog
require_changelog_entries = true
require_release_notes = true
release_notes_dir = "release-notes"
change_dir = ".changes"
pre_1_breaking_bump = "minor"
unconventional_commits = "warn"
semver_check = "warn"
require_change_files = false

[release.changelog]
path = "CHANGELOG.md"
relative_to = "crate"
entry_format = "- {scope}{breaking}{description}{prs} ({sha_link})"
emoji = true
group_order = ["breaking", "feat", "fix", "build", "chore", "ci", "deps", "docs", "other", "perf", "refactor", "style", "test"]
fallback = "other"

[release.changelog.filters]
skip_types = []
skip_scopes = []
include_paths = []
exclude_paths = []

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

## Recipes

### Minimal defaults

Set only the target platforms that must be analyzed:

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

### Cross-major version unification

Select the highest resolved major. Review and fix source compatibility after apply:

```toml
[unify]
major_version_conflict = "bump"
strict_version_compat = false
exact_pin_handling = "preserve"
```

### Report without cleanup

Keep unused-dependency detection while disabling removal, feature pruning, and MSRV writes:

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

### CI and release setup

CI and release configuration:

```toml
targets = ["x86_64-unknown-linux-gnu"]

[unify]
pin_transitives = true
msrv = true
detect_unused = true
compiler_diag_cache = true
remove_unused = true
prune_dead_features = true
consumer_scope = "open"

[release]
tag_prefix = "v"
tag_format = "{crate}-{prefix}{version}"
require_clean = true
require_changelog_entries = true
require_release_notes = true
create_github_release = true
forge = "auto"
sign_tags = true

[change-detection]
infrastructure = [".github/**", "justfile", "Cargo.lock"]
confidence_profile = "strict"
bot_pr_confidence_profile = "strict"

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

`cargo rail config validate` checks syntax, known keys, and cross-field constraints:

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
4. **Deprecation warnings** - Options scheduled for removal or replacement

### CI strict mode

By default, validation runs in **strict mode** when CI is detected (via `CI`, `GITHUB_ACTIONS`, `GITLAB_CI`, or `CIRCLECI` environment variables):

- **In CI**: Unknown keys and other warnings become errors (exit code 2)
- **Locally**: Unknown keys are warnings only

Override with `--strict` or `--no-strict` flags.

### GitHub Actions example

```yaml
# .github/workflows/ci.yml
- name: Validate config
  run: cargo rail config validate
  # Auto-strict in CI - fails on unknown keys
```

### Validation errors

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

Map release-plz workspace settings to cargo-rail release defaults:

```toml
# release-plz.toml → rail.toml
[release]
tag_format = "{crate}-{prefix}{version}"
require_changelog_entries = true
require_release_notes = true
create_github_release = true
forge = "auto"

[release.changelog]
path = "CHANGELOG.md"
relative_to = "crate"
```

For git-cliff parser/group migration, see `docs/migrate-git-cliff.md`.

## Environment Variables

No cargo-rail-specific environment variables are required. For reproducibility, configuration is file-based.

`cargo rail config validate` defaults to strict mode when `CI`, `GITHUB_ACTIONS`, `GITLAB_CI`, or `CIRCLECI` is set.

## See Also

- [Commands Reference](./commands.md) - All cargo-rail commands
- [Migration Guide](./migrate-hakari.md) - Migrating from cargo-hakari
- [git-cliff/release-plz Migration](./migrate-git-cliff.md) - Migrating changelog and release config
- [Troubleshooting](./troubleshooting.md) - Diagnose planner and executor decisions
- [README](../README.md) - Project overview and quick start
