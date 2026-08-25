# Configuration Reference

`rail.toml` supplies repository policy. It does not copy Cargo-Rail's internal defaults. An empty file is valid, and
omitted fields use coded defaults.

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

Text `config print` output is canonical reusable `rail.toml`: it contains effective repository policy and coded
defaults, omits compatibility-only inputs, passes `config validate --strict`, and has no pending migration. JSON
contains the same effective public policy under `config`. Reprinting the generated TOML preserves that policy.

`config migrate` preserves unrelated TOML and does not write coded defaults. Deprecated inputs warn while accepted.

Text and JSON explanations use the same field records. Each record contains the configured value, effective value,
default, source, classification, reason, and deprecation guidance.

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

[surface]
enabled = true
# Only when every compiler crate closed by policy has no external consumers.
consumer_scope = "workspace"

[release]
remote_effects = "auto"

[change-detection.custom]
verification = ["verification/**"]
```

## Top-level fields

| Field              |  Default | Behavior                                                                                                                                 |
| ------------------ | -------: | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `targets`          |     `[]` | Additional target triples used for target-aware resolution and compiler evidence. `init` can detect these from repository configuration. |
| `unify`            | defaults | Dependency and manifest policy.                                                                                                          |
| `surface`          | defaults | Rust declaration reachability, diagnostic, and visibility-repair policy.                                                                 |
| `release`          | defaults | Release, changelog, and remote-effect policy.                                                                                            |
| `change-detection` | defaults | Planner classification policy.                                                                                                           |
| `crates`           |     `{}` | Per-crate split, release, and changelog policy.                                                                                          |

The old empty `[workspace]`, `[toolchain]`, and `[crates.NAME.sync]` tables had no behavior and are deprecated.
Repository `[cache]` configuration is rejected: transparent L1 setup and optional L2 selection are machine state, not
project policy. `cargo rail config migrate` removes the old table without materializing a destination. See
[Caching](caching.md#shared-native-cache-l2) for the non-secret URL grammar and current activation boundary.

## `[unify]`

`cargo rail unify --check` diagnoses without mutating manifests. It may update compiler-evidence cache and reports
under `target/cargo-rail/`. It exits 1 when proven edits are pending. Running without `--check` applies the plan.
`cargo rail unify doctor` is a cheaper resolution diagnostic. It reports the selected Cargo channel, resolver, feature
mode, source and policy overrides, target domains, ambiguous aliases, and a recommended action. Compiler evidence
caching and deterministic ordering cannot be disabled.

`unify --check --explain` keeps feature evidence separated by exact manifest declaration. Each feature rule names the
member, Cargo.toml alias, normal/development/build domain, target predicate, explicit features, default-feature state,
and optional state. Renamed aliases do not share feature-provider evidence unless `include_renamed = true` explicitly
selects package-level union behavior.
When dependency domains disagree on default features, the workspace baseline disables them. Declarations that need
them opt back in through the explicit `default` feature. This preserves narrow target, development, and build domains.

Unused dependencies require complete declaration-kind, target, feature, and compilation evidence. Dormant features and
optional dependencies are deleted only when `consumer_scope = "workspace"` proves the private workspace is the
complete consumer universe. Published packages remain open-world.

| Field                                |                     Default | Behavior                                                                                                                                         |
| ------------------------------------ | --------------------------: | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| `include_paths`                      |                      `true` | Include path dependency declarations in unification.                                                                                             |
| `include_renamed`                    |                     `false` | Include renamed declarations such as `alias = { package = "real", ... }`.                                                                        |
| `exclude`                            |                        `[]` | Dependency names excluded from unification. Workspace-member cohorts remain atomic.                                                              |
| `include`                            |                        `[]` | Dependency names force-included in unification.                                                                                                  |
| `strict_version_compat`              |                      `true` | Treat incompatible existing workspace requirements as blocking.                                                                                  |
| `exact_pin_handling`                 |                    `"warn"` | Handle exact pins with `"skip"`, `"preserve"`, or `"warn"`.                                                                                      |
| `major_version_conflict`             |                    `"warn"` | `"warn"` keeps incompatible majors split; `"bump"` explicitly accepts unification to the highest resolved major.                                 |
| `transitive_pinning`                 |                       unset | Enable host-owned pins for fragmented transitive features and select `host = "root"` or a member path. Virtual workspaces require a member path. |
| `msrv_policy`                        |  compute/max/no inheritance | Disabled, or compute with source `"deps"`, `"workspace"`, or `"max"` and boolean inheritance.                                                    |
| `consumer_scope`                     |                    `"open"` | Use `"workspace"` only for a closed private consumer graph.                                                                                      |
| `preserve_features`                  |                        `[]` | Glob patterns for dormant features that must survive closed-world pruning.                                                                       |
| `skip_undeclared_patterns`           | common implementation names | Glob patterns for borrowed feature names intentionally excluded from diagnostics.                                                                |
| `max_backups`                        |                         `3` | Number of recovery backups retained after mutation.                                                                                              |
| `compiler_artifact_soft_limit_bytes` |               `34359738368` | Report storage pressure when the command-owned compiler working set reaches 32 GiB.                                                              |
| `compiler_artifact_hard_limit_bytes` |               `68719476736` | Stop compiler acquisition when that working set reaches 64 GiB.                                                                                  |

Example policy:

```toml
[unify]
include_renamed = true
msrv_policy = { mode = "compute", source = "workspace", inherit = true }
consumer_scope = "workspace"
preserve_features = ["unstable-*", "bench*"]
```

## `[surface]`

`cargo rail surface` merges authenticated compiler facts from production, non-production, build-script, proc-macro,
doctest, feature, and configured target views into one physical declaration graph. With no operation flag it prints a
read-only report and exits 0 even when findings exist. `--check` uses the same analysis but exits 1 for `deny` findings
or configuration diagnostics; `warn` findings remain visible with exit 0. `--fix --dry-run` emits the exact mutation
plan. `--fix` revalidates the captured snapshot, edits only planned visibility spans, recompiles every configured view,
and writes a receipt after successful verification. Dead-public findings are report-only.

The complete installer in the [README](../README.md#installation) includes authenticated prebuilt and offline-source
driver authority. `cargo rail surface --prepare` resolves the workspace's exact Cargo-selected compiler, installs its
`rustc-dev` component through `rustup` when compiler development metadata is absent, builds a toolchain-matched driver
when the prebuilt one differs, and authenticates both before analysis. It does not change the user default toolchain.
Source installs and `cargo binstall` keep `surface --schema`, but cannot prepare or analyze code.

| Field                     |       Default | Behavior                                                                                                                                        |
| ------------------------- | ------------: | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `enabled`                 |       `false` | Include the whole-workspace `surface` gate in planner and CI decisions. Direct `cargo rail surface` inspection remains available when disabled. |
| `consumer_scope`          |      `"open"` | `"workspace"` permits closed-world conclusions at the compiler-crate boundary described below.                                                  |
| `targets`                 |    `["host"]` | Compiler target views to merge. Use `"host"` or target triples already captured by the top-level `targets` policy.                              |
| `crate_visibility`        |  `"preserve"` | `"allow"` enables the otherwise allow-by-default `unnecessary-crate-visibility` class.                                                          |
| `preserve_uniform_fields` |       `false` | Preserve one intentional field-visibility level across a struct or union instead of reducing fields independently.                              |
| `lint`                    |          `[]` | Ordered global `{ selector, level }` directives. Later matching directives win; `selector` is `warnings` or one exact lint.                     |
| `product`                 |          `[]` | Complete shipped binary/library roots. When empty, every workspace binary is implicit; an explicit list replaces that inference.                |
| `feature-profile`         |          `[]` | Exact Cargo feature profiles. Empty uses automatic coverage; an explicit list replaces it.                                                      |
| `doctest`                 |          `[]` | Exact doctest package set. Empty follows `doctest_coverage`.                                                                                    |
| `doctest_coverage`        | `"automatic"` | `"automatic"` covers every doctest-enabled workspace package; `"disabled"` covers none and cannot accompany explicit doctests.                  |
| `external`                |          `[]` | Exact compiler crates deliberately kept outside closed-world authority, each with a reason.                                                     |
| `override`                |          `[]` | Item-specific policy. `allow` suppresses, `expect` suppresses but fails when stale, and `warn`/`deny` retain a finding.                         |
| `exclude`                 |          `[]` | Module- or repository-file-scoped policy with the same `allow`, `expect`, `warn`, and `deny` levels.                                            |

The four lint names are `dead-public`, `unnecessary-public`, `unnecessary-restricted-visibility`, and
`unnecessary-crate-visibility`. Core findings deny by default; crate-visibility reductions allow by default. The
ordered `warnings` selector applies to the three core classes. `--only` filters
finding classes after policy evaluation and never hides unknown, ambiguous, overlapping, or stale policy diagnostics.

Closed-world authority is exact per compiler crate, not per package. Selected binary products are closed even in a
publishable package. With `consumer_scope = "workspace"`, non-publishable library, proc-macro, and build-script targets
are also closed; publishable libraries stay open. A physical declaration observed in any open compiler crate is
preserved even when another observation is closed. `[[surface.external]]` opens a named compiler crate explicitly.

```toml
[surface]
enabled = true
consumer_scope = "workspace"
targets = ["host", "x86_64-unknown-linux-gnu"]
crate_visibility = "allow"
preserve_uniform_fields = true

[[surface.product]]
package = "app"
bin = "app"
target = "cfg(unix)"
reason = "shipped application"

[[surface.feature-profile]]
name = "server"
no-default-features = true
features = ["tls", "metrics"]

[[surface.doctest]]
package = "app-core"

[[surface.external]]
crate = "app_ffi"
reason = "loaded by consumers outside this workspace"

[[surface.lint]]
selector = "warnings"
level = "warn"

[[surface.lint]]
selector = "dead-public"
level = "deny"

[[surface.override]]
lint = "dead-public"
crate = "app_core"
item = "migration::legacy_entry"
kind = "function"
target = "cfg(unix)"
level = "expect"
reason = "removed after downstream migration"

[[surface.exclude]]
package = "app-core"
module = "generated"
level = "expect"
reason = "generated protocol bindings"
```

Each product or policy entry can use an optional Cargo target triple or `cfg(...)` selector. An override and exclusion
requires exactly one owner selector, `package` or Rust compiler `crate`; exclusions require exactly one of `module` or
`file`. Missing and ambiguous item selectors are configuration failures instead of broad matches.

`surface.targets` selects only target views declared by the repository's top-level target policy. With no explicit
feature profiles, Cargo-Rail derives default, no-default, all-features, and applicable selected-feature views from
manifests and cfg expressions. Explicit profiles replace that matrix exactly.

See [Migrate from Hawk](migrate-hawk.md) for configuration and command mappings.

## `[release]`

Reviewed `.changes/*.md` files are the default authority for bumps and release prose. Commit-derived modes support
migration. Remote release modes bind the prepared commit and wait for readiness on its exact SHA. Registry publication
is a separate default-deny authority and requires both `registry_publication = "crates-io"` and `--publish`; when
authorized, it runs in dependency order, observes crates.io state, and creates tags last.

| Field                       |                       Default | Behavior                                                                                                                                                                                                                                                                                                                                                                 |
| --------------------------- | ----------------------------: | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `source`                    |                   `"changes"` | `"changes"` uses reviewed intent only. `"commits"` and `"both"` are explicit compatibility modes.                                                                                                                                                                                                                                                                        |
| `tag_prefix`                |                         `"v"` | Value rendered by `{prefix}`.                                                                                                                                                                                                                                                                                                                                            |
| `tag_format`                | `"{crate}-{prefix}{version}"` | Tag namespace. Multi-crate formats should include `{crate}`.                                                                                                                                                                                                                                                                                                             |
| `remote_effects`            |                      `"none"` | `"none"` stays local. Other modes push the exact release commit, then exit until GitHub reports complete counts with at least one successful non-skipped context and no pending/failed context, or GitLab reports a successful exact-SHA pipeline. Publication follows readiness; tags are pushed last. `"auto"`, `"github"`, and `"gitlab"` also create forge releases. |
| `registry_publication`      |                      `"none"` | Registry authority independent from Git/forge effects. `"crates-io"` permits publication only when the invocation also passes `--publish`; Cargo manifest registry restrictions remain the upper bound.                                                                                                                                                      |
| `sign_tags`                 |                       `false` | Sign release tags with the configured Git signing mechanism.                                                                                                                                                                                                                                                                                                             |
| `require_changelog_entries` |                       `false` | Fail when a released crate has no generated changelog entries.                                                                                                                                                                                                                                                                                                           |
| `require_release_notes`     |                        `true` | Require reviewed notes before tag, publish, or forge effects.                                                                                                                                                                                                                                                                                                            |
| `release_notes_dir`         |             `"release-notes"` | Manual release-note override directory.                                                                                                                                                                                                                                                                                                                                  |
| `change_dir`                |                  `".changes"` | Reviewed release-intent directory.                                                                                                                                                                                                                                                                                                                                       |
| `pre_1_breaking_bump`       |                     `"minor"` | Map breaking 0.x intent to `"minor"` or `"major"`.                                                                                                                                                                                                                                                                                                                       |
| `unconventional_commits`    |                      `"warn"` | Compatibility-mode policy for non-conventional commits; ignored in changes mode.                                                                                                                                                                                                                                                                                         |
| `semver_check`              |                      `"warn"` | `"off"` disables external validation. A confirmed bump mismatch blocks instead of escalating reviewed intent.                                                                                                                                                                                                                                                            |
| `require_change_files`      |                       `false` | Compatibility-mode coverage policy. Changes mode always gates every changed crate.                                                                                                                                                                                                                                                                                       |
| `version_groups`            |                          `{}` | Named crate lists released in lockstep at their maximum required bump.                                                                                                                                                                                                                                                                                                   |

```toml
[release]
source = "changes"
tag_format = "{prefix}{version}"
remote_effects = "auto"
registry_publication = "crates-io"
sign_tags = true

[release.version_groups]
core = ["rail-core", "rail-graph", "rail-git"]
```

`cargo rail release run` defaults to `--bump auto`. In changes mode, only `patch`, `minor`, or `major` entries select a
crate. `none` records reviewed no-release intent and satisfies coverage without adding changelog prose. Release-worthy
entries in one file are consumed atomically. `none` entries outside the release plan are retained by an exact
frontmatter rewrite. Dependency-only releases receive a synthesized patch entry. Use `source = "commits"` or
`source = "both"` only during migration from a commit-driven workflow.

### `[release.changelog]`

| Field          |          Default | Behavior                                                                                                  |
| -------------- | ---------------: | --------------------------------------------------------------------------------------------------------- |
| `path`         | `"CHANGELOG.md"` | Changelog path.                                                                                           |
| `relative_to`  |        `"crate"` | Resolve from each crate or use `"workspace"`.                                                             |
| `entry_format` |    built-in line | Bounded placeholders: `{scope}`, `{breaking}`, `{description}`, `{prs}`, `{sha}`, `{sha_link}`, `{type}`. |
| `emoji`        |           `true` | Render emoji in section headings.                                                                         |
| `group_order`  |   built-in order | Deterministic commit-type section order.                                                                  |
| `fallback`     |        `"other"` | Section for unlisted types, or `"skip"`.                                                                  |
| `groups`       |             `[]` | Custom commit-type groups.                                                                                |
| `commit_url`   |         inferred | Override the `{sha}` link template.                                                                       |
| `pr_url`       |         inferred | Override the `{pr}` link template.                                                                        |

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

| Field                 |                Default | Behavior                                                            |
| --------------------- | ---------------------: | ------------------------------------------------------------------- |
| `infrastructure`      | built-in tooling globs | Paths that select workspace-wide infrastructure work.               |
| `custom`              |                   `{}` | Named planner-output categories mapped to path globs.               |
| `unknown_file_policy` |             `"strict"` | `"docs"`, `"owned_build_test"`, `"workspace_infra"`, or `"strict"`. |
| `confidence_profile`  |           `"balanced"` | Repository default: `"strict"`, `"balanced"`, or `"fast"`.          |

Provider identity does not change planner policy. CI can override the repository profile on the CLI; bot authorship
is not a policy input.

```toml
[change-detection]
infrastructure = [".github/**", "scripts/**", "Cargo.lock"]
confidence_profile = "strict"

[change-detection.custom]
verification = ["verification/**"]
assets = ["web/assets/**"]
```

## Removed execution configuration

The former execution table is rejected with an actionable diagnostic. Keep repository commands in Cargo,
cargo-nextest, Just, or CI, and pass typed package scope from `cargo rail plan`.

## `[crates.NAME]`

### Split and sync

`[crates.NAME.split]` is the single source of split/sync mapping policy.

| Field            | Required/default | Behavior                                                                                               |
| ---------------- | ---------------: | ------------------------------------------------------------------------------------------------------ |
| `remote`         |         required | Git URL or local test path.                                                                            |
| `branch`         |         required | Destination branch.                                                                                    |
| `mode`           |         required | `"single"` for one member or `"combined"` for multiple members.                                        |
| `workspace_mode` |   `"standalone"` | Combined layout: `"standalone"` or `"workspace"`.                                                      |
| `members`        |       split name | Cargo package names owned by the split. Single mode requires one; combined mode requires at least two. |
| `include`        |             `[]` | Glob patterns selecting explicit non-Cargo files from the workspace snapshot.                          |
| `exclude`        |             `[]` | Glob patterns narrowing `include`; Cargo-owned member files cannot be excluded.                        |

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

`[crates.NAME.release]` supports `publish`, which defaults to `true` but can only narrow the registries authorized by
Cargo.toml. `[crates.NAME.changelog]` can override `path`,
`relative_to`, `skip`, `entry_format`, `emoji`, `group_order`, `fallback`, `groups`, `filters`, `commit_url`, and
`pr_url`. Absent values inherit `[release.changelog]`.

```toml
[crates.internal.release]
publish = false

[crates.my-crate.changelog]
path = "HISTORY.md"
emoji = false
```

## Deprecated inputs and migrations

Deprecated fields remain parseable for a bounded compatibility window. They emit warnings and have one
`config migrate` action.

| Deprecated input                                                    | Migration                                                                                                       |
| ------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| `[workspace]`, `[toolchain]`, `[crates.NAME.sync]`                  | Remove the empty reserved table.                                                                                |
| `crates.NAME.split.paths`                                           | Resolve each legacy Cargo path to its package name and write `split.members`.                                   |
| `unify.compiler_diag_cache`                                         | Remove; correct caching is automatic.                                                                           |
| `unify.sort_dependencies`                                           | Remove; edits are always deterministic.                                                                         |
| `unify.prune_dead_features`                                         | Remove; diagnostics are unconditional and deletion uses `consumer_scope`.                                       |
| `unify.detect_unused`, `unify.remove_unused`                        | Remove; diagnostics plus `unify --check`/apply define the boundary.                                             |
| `unify.detect_undeclared_features`, `unify.fix_undeclared_features` | Remove; diagnostics plus `unify --check`/apply define the boundary.                                             |
| `unify.pin_transitives`, `unify.transitive_host`                    | Merge enabled pinning and its host into `unify.transitive_pinning`.                                             |
| `unify.msrv`, `unify.msrv_source`, `unify.enforce_msrv_inheritance` | Merge one valid choice into `unify.msrv_policy`.                                                                |
| `change-detection.bot_pr_confidence_profile`                        | Remove; provider identity no longer changes policy.                                                             |
| `change-detection.conservative_unclassified_owner_fallback`         | Rename to the equivalent explicit `unknown_file_policy`.                                                        |
| Boolean `change-detection.unknown_file_policy`                      | Replace `true` with `"owned_build_test"` or `false` with `"docs"`.                                              |
| `release.require_clean`, `release.publish_delay`                    | Remove; cleanliness is fixed command behavior and registry convergence is an explicit stop-and-resume boundary. |
| `release.push`, `release.create_github_release`, `release.forge`    | Merge the valid effect combination into one `release.remote_effects` value.                                     |

```bash
cargo rail config migrate --check -f json
cargo rail config migrate
cargo rail config validate --strict
```

## Exit behavior

| Command                    | Exit 0                          | Exit 1            | Exit 2                              |
| -------------------------- | ------------------------------- | ----------------- | ----------------------------------- |
| `config migrate --check`   | No migration pending            | Migration pending | Error                               |
| Mutation command `--check` | No mutation pending             | Mutation pending  | Error                               |
| `config validate`          | Valid under selected strictness | —                 | Invalid or unreadable configuration |

## Environment

`config validate` enables strict mode when `CI`, `GITHUB_ACTIONS`, `GITLAB_CI`, or `CIRCLECI` is present. Use
`--no-strict` only when warnings are intentionally non-blocking.

## See also

- [Command reference](commands.md)
- [Planning](planning.md)
- [Split/sync example](../examples/split-sync/README.md)
