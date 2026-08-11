# Configuration Reference

`rail.toml` supplies repository policy to Cargo-Rail's captured workspace view; it does not mirror Cargo-Rail's internal defaults.
An empty file is valid. Omitted fields use coded defaults, and upgrades do not require copying new fields into the
repository.

## File discovery

Cargo-Rail uses the first file found in this order:

1. `rail.toml`
2. `.rail.toml`
3. `.cargo/rail.toml`
4. `.config/rail.toml`

`--config PATH` bypasses discovery. Relative paths are resolved from `--workspace-root`.

```bash
cargo rail config locate
cargo rail --config config/ci.toml config validate --strict
```

## Create, inspect, and migrate

```bash
cargo rail init --dry-run            # Preview the sparse file
cargo rail init                      # Write detected non-default choices
cargo rail config print              # Print the fully effective config
cargo rail config explain            # Explain value, default, source, and behavior
cargo rail config validate --strict  # Reject warnings and invalid policy
cargo rail config migrate --check    # Read-only; exit 1 when migration is pending
cargo rail config migrate            # Apply explicit semantic migrations
```

`config migrate` is deliberately not a synchronization command. It preserves unrelated TOML and never materializes coded defaults. Deprecated inputs warn while they remain accepted.

`config explain -f json` and text output are built from the same field records. Each record contains the configured value, effective value, default, source, classification, behavioral reason, and any deprecation guidance.

## Minimal configuration

No policy is required:

```toml
# Empty rail.toml is valid.
```

A typical repository should contain only choices that differ from defaults:

```toml
targets = ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"]

[unify]
include_renamed = true
major_version_conflict = "bump"

[release]
remote_effects = "auto"

[change-detection.custom]
verification = ["verification/**"]
```

## Top-level fields

| Field | Default | Behavior |
|---|---:|---|
| `targets` | `[]` | Additional target triples used for target-aware resolution and compiler evidence. `init` can detect these from repository configuration. |
| `cache` | defaults | Local and shared native compiler-result cache policy. |
| `unify` | defaults | Dependency and manifest policy. |
| `release` | defaults | Release, changelog, and remote-effect policy. |
| `change-detection` | defaults | Planner classification policy. |
| `run` | defaults | Named execution profiles. |
| `crates` | `{}` | Per-crate split, release, and changelog policy. |

The old empty `[workspace]`, `[toolchain]`, and `[crates.NAME.sync]` tables had no behavior and are deprecated.

## `[cache]`

Transparent local compiler-result reuse is installed as machine state with `cargo rail cache setup`; repository policy
does not own that wrapper or CAS. The retained cache table controls Cargo-Rail command behavior and validates remote
target configuration, but does not activate remote transfer:

| Field | Default | Behavior |
|---|---:|---|
| `enabled` | `true` | Permit cache use for Cargo-Rail-owned actions. `--no-cache` also delegates child compiler work with `CARGO_RAIL_CACHE=off`. It does not uninstall transparent machine setup. |
| `l2` | unset | Select and validate a target alias from `CARGO_RAIL_CACHE_TARGETS_FILE`. Status schema 8 reports it as configuration-only; transparent caching remains local. |

```toml
[cache]
l2 = "team"
```

The alias is at most 64 bytes, starts with a lowercase ASCII letter, and contains only lowercase ASCII letters, digits,
`-`, or `_`. The target map is an absolute JSON file outside the checkout and contains no credentials. See
[Caching](caching.md#shared-native-cache-l2) for the retained validation schema. Do not treat it as an active remote
data plane.

## `[unify]`

`cargo rail unify --check` always performs safe diagnostics without mutating manifests. It may update compiler-evidence
cache and report files under `target/cargo-rail/`, and it exits 1 when proven manifest edits are pending. Running without
`--check` applies the plan. `cargo rail unify doctor` is a cheaper read-only resolution diagnostic: it reports the
selected Cargo/channel, resolver, feature mode, source and policy overrides, target domains, ambiguous aliases, and a
recommended next action. Compiler evidence caching and deterministic ordering are internal responsibilities and cannot
be disabled.

`unify --check --explain` keeps feature evidence separated by exact manifest declaration. Each feature rule names the
member, Cargo.toml alias, normal/development/build domain, target predicate, explicit features, default-feature state,
and optional state. Renamed aliases do not share feature-provider evidence unless `include_renamed = true` explicitly
selects package-level union behavior.
When dependency domains disagree on default features, the workspace baseline disables them and declarations that need
them opt back in through the explicit `default` feature. This preserves narrow target, development, and build domains.

Unused dependencies are removed only with complete declaration-kind, target, feature, and compilation evidence. Dormant features and optional dependencies are deleted only when `consumer_scope = "workspace"` proves the private workspace is the complete consumer universe; published packages remain open-world.

| Field | Default | Behavior |
|---|---:|---|
| `include_paths` | `true` | Include path dependency declarations in unification. |
| `include_renamed` | `false` | Include renamed declarations such as `alias = { package = "real", ... }`. |
| `exclude` | `[]` | Dependency names excluded from unification. Workspace-member cohorts remain atomic. |
| `include` | `[]` | Dependency names force-included in unification. |
| `strict_version_compat` | `true` | Treat incompatible existing workspace requirements as blocking. |
| `exact_pin_handling` | `"warn"` | Handle exact pins with `"skip"`, `"preserve"`, or `"warn"`. |
| `major_version_conflict` | `"warn"` | `"warn"` keeps incompatible majors split; `"bump"` explicitly accepts unification to the highest resolved major. |
| `transitive_pinning` | unset | Enable workspace-hack-style pinning and select `host = "root"` or a member path. Virtual workspaces require a member path. |
| `msrv_policy` | compute/max/no inheritance | `{ mode = "disabled" }` or `{ mode = "compute", source = "deps|workspace|max", inherit = true|false }`. |
| `consumer_scope` | `"open"` | Use `"workspace"` only for a closed private consumer graph. |
| `preserve_features` | `[]` | Glob patterns for dormant features that must survive closed-world pruning. |
| `skip_undeclared_patterns` | common implementation names | Glob patterns for borrowed feature names intentionally excluded from diagnostics. |
| `max_backups` | `3` | Number of recovery backups retained after mutation. |

Example policy:

```toml
[unify]
include_renamed = true
msrv_policy = { mode = "compute", source = "workspace", inherit = true }
consumer_scope = "workspace"
preserve_features = ["unstable-*", "bench*"]
```

## `[release]`

Reviewed `.changes/*.md` files are the default authority for bump selection and release prose. Commit-derived modes
exist for migration. Remote release modes bind the prepared commit, wait for readiness on that exact SHA, publish in
dependency order, observe registry state, and create tags last.

| Field | Default | Behavior |
|---|---:|---|
| `source` | `"changes"` | `"changes"` uses reviewed intent only. `"commits"` and `"both"` are explicit compatibility modes. |
| `tag_prefix` | `"v"` | Value rendered by `{prefix}`. |
| `tag_format` | `"{crate}-{prefix}{version}"` | Tag namespace. Multi-crate formats should include `{crate}`. |
| `require_clean` | `true` | Deprecated compatibility input. Preview permits dirt and apply always rejects paths outside the bound plan. Remove it with `cargo rail config migrate`. |
| `publish_delay` | `5` | Deprecated compatibility input with no effect. Cargo-Rail never sleeps for registry convergence. Remove it with `cargo rail config migrate`. |
| `remote_effects` | `"none"` | `"none"` stays local. Other modes push the exact release commit, then exit until GitHub reports complete counts with at least one successful non-skipped context and no pending/failed context, or GitLab reports a successful exact-SHA pipeline. Publication follows readiness; tags are pushed last. `"auto"`, `"github"`, and `"gitlab"` also create forge releases. |
| `sign_tags` | `false` | Sign release tags with the configured Git signing mechanism. |
| `require_changelog_entries` | `false` | Fail when a released crate has no generated changelog entries. |
| `require_release_notes` | `true` | Require reviewed notes before tag, publish, or forge effects. |
| `release_notes_dir` | `"release-notes"` | Manual release-note override directory. |
| `change_dir` | `".changes"` | Reviewed release-intent directory. |
| `pre_1_breaking_bump` | `"minor"` | Map breaking 0.x intent to `"minor"` or `"major"`. |
| `unconventional_commits` | `"warn"` | Compatibility-mode policy for non-conventional commits; ignored in changes mode. |
| `semver_check` | `"warn"` | `"off"` disables external validation. A confirmed bump mismatch blocks instead of escalating reviewed intent. |
| `require_change_files` | `false` | Compatibility-mode coverage policy. Changes mode always gates every changed crate. |
| `version_groups` | `{}` | Named crate lists released in lockstep at their maximum required bump. |

```toml
[release]
source = "changes"
tag_format = "{prefix}{version}"
remote_effects = "auto"
sign_tags = true

[release.version_groups]
core = ["rail-core", "rail-graph", "rail-git"]
```

`cargo rail release run` defaults to `--bump auto`. In changes mode, only release-worthy entries (`patch`, `minor`, or `major`) select a crate; `none` records explicit reviewed no-release intent and satisfies coverage without adding changelog prose. Release-worthy entries in one shared file are consumed atomically, while `none` entries for crates outside the release plan are retained by an exact frontmatter rewrite. Dependency-only releases receive a synthesized patch entry. Use `source = "commits"` or `source = "both"` only while migrating an older commit-driven workflow.

### `[release.changelog]`

| Field | Default | Behavior |
|---|---:|---|
| `path` | `"CHANGELOG.md"` | Changelog path. |
| `relative_to` | `"crate"` | Resolve from each crate or use `"workspace"`. |
| `entry_format` | built-in line | Bounded placeholders: `{scope}`, `{breaking}`, `{description}`, `{prs}`, `{sha}`, `{sha_link}`, `{type}`. |
| `emoji` | `true` | Render emoji in section headings. |
| `group_order` | built-in order | Deterministic commit-type section order. |
| `fallback` | `"other"` | Section for unlisted types, or `"skip"`. |
| `groups` | `[]` | Custom commit-type groups. |
| `commit_url` | inferred | Override the `{sha}` link template. |
| `pr_url` | inferred | Override the `{pr}` link template. |

`[release.changelog.filters]` supports `skip_types`, `skip_scopes`, `include_paths`, and `exclude_paths`, all empty by default. Breaking entries are not suppressed by ordinary type filtering.

```toml
[release.changelog]
relative_to = "workspace"
emoji = false
fallback = "skip"

[[release.changelog.groups]]
types = ["sec", "security"]
title = "Security"
emoji = "🔒"

[release.changelog.filters]
skip_types = ["chore", "ci"]
exclude_paths = ["fixtures/**"]
```

## `[change-detection]`

Cargo ownership and reverse dependency impact come from the resolved graph. The globs below classify infrastructure
and custom repository surfaces; they do not replace crate ownership with hand-maintained path filters.

| Field | Default | Behavior |
|---|---:|---|
| `infrastructure` | built-in tooling globs | Paths that select workspace-wide infrastructure work. |
| `custom` | `{}` | Named planner-output categories mapped to path globs. |
| `unknown_file_policy` | `"strict"` | `"docs"`, `"owned_build_test"`, `"workspace_infra"`, or `"strict"`. |
| `confidence_profile` | `"balanced"` | Repository default: `"strict"`, `"balanced"`, or `"fast"`. |

Provider identity does not change planner policy. CI can override the repository profile explicitly on the CLI, but bot authorship is not a policy input.

```toml
[change-detection]
infrastructure = [".github/**", "scripts/**", "Cargo.lock"]
confidence_profile = "strict"

[change-detection.custom]
verification = ["verification/**"]
assets = ["web/assets/**"]
```

## `[run]`

`run` consumes planner scope instead of reclassifying the workspace. Profiles select ordered action IDs. Built-ins are
`build`, `test`, `bench`, `docs`, `format`, `lint`, `msrv`, `package`, `audit`, and `distribution`. Planner-only
`infra` and `custom:*` values can enable configured actions through `when`, but are not executable action IDs.

| Field | Default | Behavior |
|---|---:|---|
| `default_profile` | unset | Built-in or configured profile used when the CLI does not select one. |
| `profile.NAME.actions` | required | Non-empty ordered list of built-in or configured action IDs. |
| `profile.NAME.run_args` | `[]` | Arguments prepended to CLI run arguments. Supports `{workspace_root}`, `{base_ref}`, and `{cargo_args}`. |
| `profile.NAME.baseline` | unset | One typed baseline: `{ kind = "merge-base" }` or `{ kind = "since", reference = "..." }`. References support `{workspace_root}` and `{base_ref}`. |
| `workflow.NAME` | unset | Map a workflow convention to a built-in or configured profile. |
| `action.NAME.kind` | `"task"` | `"task"` or `"generated"`; only generated actions may own outputs. |
| `action.NAME.argv` | required | Direct regeneration/task program and argv. No shell is involved. |
| `action.NAME.check_argv` | generated only | Required read-only staleness check for a generated action. |
| `action.NAME.dependencies` | `[]` | Action IDs that must complete first. |
| `action.NAME.when` | required | Planner surfaces that enable this action without `--all`. |
| `action.NAME.working_directory` | `"."` | Canonical `/`-separated repository-contained process directory. |
| `action.NAME.packages` | `"none"` | `"none"`, `"selected"`, or `"workspace-or-selected"`; controls `{packages}`. |
| `action.NAME.targets`, `features` | `[]` | Explicit values inserted at `{targets}` and `{features}`. |
| `action.NAME.inputs` | `[]` | Canonical `/`-separated repository-relative input scopes; `"."` means the workspace snapshot. |
| `action.NAME.outputs` | `[]` | Canonical repository-relative paths owned by one generated action without case-insensitive overlap. |
| `action.NAME.environment.inherit` | `false` | Inherit the complete caller environment before applying typed entries. |
| `action.NAME.environment.entries` | `[]` | Fixed, pass-through, Cargo-derived, or secret-capability entries. |

```toml
[run]
default_profile = "commit"

[run.profile.commit]
actions = ["format", "lint", "test"]
baseline = { kind = "merge-base" }

[run.workflow]
pull_request = "commit"
```

Repository actions are bounded direct-process declarations. Each substitution must occupy one complete argv value;
the closed set is `{workspace_root}`, `{base_ref}`, `{packages}`, `{targets}`, and `{features}`. Shell executables,
unknown interpolation, dependency cycles, path escapes, missing generated ownership, and overlapping outputs fail
before any action process starts. Secret entries serialize only the capability name.

```toml
[run.action.codegen]
kind = "generated"
argv = ["cargo", "run", "-p", "xtask", "--", "codegen"]
check_argv = ["cargo", "run", "-p", "xtask", "--", "codegen", "--check"]
dependencies = ["format"]
when = ["build", "infra"]
working_directory = "."
inputs = ["Cargo.toml", "schema"]
outputs = ["src/generated"]

[run.action.codegen.environment]
inherit = true
entries = [
  { kind = "cargo", name = "WORKSPACE_ROOT", value = "workspace-root" },
  { kind = "secret", name = "SCHEMA_TOKEN" },
]

[run.profile.codegen]
actions = ["codegen"]
```

Use `--generated check` to run the declared read-only checks, the default `--generated regenerate` to update outputs,
and `--explain` to show ownership. `run --dry-run -f json` emits the versioned action plan; `-f github` emits the same
topological order as GitHub key/value outputs. Structured formats require `--dry-run`.

## `[crates.NAME]`

### Split and sync

`[crates.NAME.split]` is the single source of split/sync mapping policy.

| Field | Required/default | Behavior |
|---|---:|---|
| `remote` | required | Git URL or local test path. |
| `branch` | required | Destination branch. |
| `mode` | required | `"single"` for one member or `"combined"` for multiple members. |
| `workspace_mode` | `"standalone"` | Combined layout: `"standalone"` or `"workspace"`. |
| `members` | split name | Cargo package names owned by the split. Single mode requires one; combined mode requires at least two. |
| `include` | `[]` | Glob patterns selecting explicit non-Cargo files from the workspace snapshot. |
| `exclude` | `[]` | Glob patterns narrowing `include`; Cargo-owned member files cannot be excluded. |

```toml
[crates.my-crate.split]
remote = "git@github.com:org/my-crate.git"
branch = "main"
mode = "single"
include = ["LICENSE"]
```

Cargo roots, dependency closure, and intersecting release version groups are resolved by package identity from one
`WorkspaceSnapshot`. A single split whose key differs from its package name must set `members = ["package-name"]`.
Included assets retain their workspace-relative paths; ambiguous single-split mappings are rejected before mutation.

### Release and changelog overrides

`[crates.NAME.release]` supports `publish`, defaulting to `true`. `[crates.NAME.changelog]` can override `path`, `relative_to`, `skip`, `entry_format`, `emoji`, `group_order`, `fallback`, `groups`, `filters`, `commit_url`, and `pr_url`. Absent values inherit `[release.changelog]`.

```toml
[crates.internal.release]
publish = false

[crates.my-crate.changelog]
path = "HISTORY.md"
emoji = false
```

## Deprecated inputs and migrations

Deprecated fields remain parseable for a bounded compatibility window, emit actionable warnings, and have one explicit `config migrate` action.

| Deprecated input | Migration |
|---|---|
| `[workspace]`, `[toolchain]`, `[crates.NAME.sync]` | Remove the empty reserved table. |
| `crates.NAME.split.paths` | Resolve each legacy Cargo path to its package name and write `split.members`. |
| `unify.compiler_diag_cache` | Remove; correct caching is automatic. |
| `unify.sort_dependencies` | Remove; edits are always deterministic. |
| `unify.prune_dead_features` | Remove; diagnostics are unconditional and deletion uses `consumer_scope`. |
| `unify.detect_unused`, `unify.remove_unused` | Remove; diagnostics plus `unify --check`/apply define the boundary. |
| `unify.detect_undeclared_features`, `unify.fix_undeclared_features` | Remove; diagnostics plus `unify --check`/apply define the boundary. |
| `unify.pin_transitives`, `unify.transitive_host` | Merge enabled pinning and its host into `unify.transitive_pinning`. |
| `unify.msrv`, `unify.msrv_source`, `unify.enforce_msrv_inheritance` | Merge one valid choice into `unify.msrv_policy`. |
| `change-detection.bot_pr_confidence_profile` | Remove; provider identity no longer changes policy. |
| `change-detection.conservative_unclassified_owner_fallback` | Rename to the equivalent explicit `unknown_file_policy`. |
| Boolean `change-detection.unknown_file_policy` | Replace `true` with `"owned_build_test"` or `false` with `"docs"`. |
| `release.require_clean`, `release.publish_delay` | Remove; cleanliness is fixed command behavior and registry convergence is an explicit stop-and-resume boundary. |
| `release.push`, `release.create_github_release`, `release.forge` | Merge the valid effect combination into one `release.remote_effects` value. |
| `run.profile.NAME.surfaces` | Rename to `run.profile.NAME.actions`. |
| `run.profile.NAME.since`, `run.profile.NAME.merge_base` | Merge one valid baseline into `run.profile.NAME.baseline`. |

```bash
cargo rail config migrate --check -f json
cargo rail config migrate
cargo rail config validate --strict
```

## Exit behavior

| Command | Exit 0 | Exit 1 | Exit 2 |
|---|---|---|---|
| `config migrate --check` | No migration pending | Migration pending | Error |
| Mutation command `--check` | No mutation pending | Mutation pending | Error |
| `--dry-run` | Preview completed | — | Error |
| `config validate` | Valid under selected strictness | — | Invalid or unreadable configuration |

## Environment

`config validate` enables strict mode automatically when `CI`, `GITHUB_ACTIONS`, `GITLAB_CI`, or `CIRCLECI` is present. Use `--no-strict` only when warnings are intentionally non-blocking.

## See also

- [Command reference](commands.md)
- [Planning and execution](planning.md)
- [Split/sync example](../examples/split-sync/README.md)
