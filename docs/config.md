# Configuration

`rail.toml` contains sparse repository policy.
Omitted settings and absent files use coded defaults.
Keep commands, credentials, timeouts, setup, and execution in their owning tools.

## Find and inspect configuration

Cargo-Rail uses the first file found:

1. `rail.toml`
1. `.rail.toml`
1. `.cargo/rail.toml`
1. `.config/rail.toml`

`--config PATH` bypasses discovery.
Relative paths are resolved from the selected workspace root.
Use `--workspace-root PATH` to select a different workspace.

```bash
cargo rail init --dry-run
cargo rail init
cargo rail config
cargo rail config locate
cargo rail config print
cargo rail config explain --all --json
cargo rail config validate --strict
cargo rail config migrate
```

`init` recognizes either an explicit `[workspace]` manifest or a root `[package]` manifest.
Without a Cargo manifest it still writes the requested configuration but reports
that no workspace was detected.

Bare `cargo rail config` shows configured overrides and the active source, or reports that coded defaults apply.
`config explain --all` reports configured and effective values, defaults, sources, and field rationale.
Pass leaf paths for exact fields, or a parent node to enumerate its effective children, such as `config explain surface --json`.
Both commands validate policy before reporting success.
Unknown keys, unreadable files, and failed Cargo workspace discovery are errors;
a missing explicit `--config PATH` never falls back to defaults.

`config print` exports canonical TOML with defaults.
It preserves policy such as `surface.targets = "workspace"`, so saved output continues inheriting future changes to top-level
targets.
Explanation shows the resolved targets.
Use `-f json` or `--json` for machine inspection.
Explanation conforms to [`config-explain-v2.schema.json`](../schemas/config-explain-v2.schema.json).

Canonical output can be validated through stdin:

```bash
cargo rail config print | cargo rail --config - config validate --strict
```

Stdin validation is independent of the checkout.
Policy requiring workspace facts, such as split paths, package selections,
or a transitive host path, reports the missing context;
inspect that policy from a file in its owning workspace.
Ordinary file inspection validates against the selected Cargo workspace.
When `Cargo.lock` exists, validation resolves it with `--locked`, as consuming commands do,
so a stale lockfile fails validation.
Validation never creates a lockfile.
`config validate` states what it checked: `workspace` evidence binds policy to the Cargo workspace,
and `schema` evidence covers stdin or a directory without `Cargo.toml`.
JSON output reports this in `evidence`.

## Compatibility

The current Cargo-Rail release accepts only current configuration fields.
Configuration inspection leaves the input file unchanged;
unsupported keys fail before planning or mutation,
and a retired key names the release that removed it.
Historical comparisons read retired keys with their current meaning,
and identify the revision and configuration path when decoding fails.

`cargo rail config migrate` previews the update; `config migrate apply` writes it.
It replaces each retired `[release]` key from Cargo-Rail 0.31 with its successor and keeps what the key did:
a file without registry publication gets `publish = "none"`, one without GitHub effects gets `github_release = false` and `checks = "none"`,
`{prefix}` in a tag format becomes the prefix itself, and an artifact inventory becomes `release.assets`.
It also removes settings that only restate current defaults; see [Start sparse](#start-sparse).
Run `cargo rail config validate --strict` afterward.

Plans use contract v9 and variant catalogs use v2.
Regenerate older saved plans and validate catalogs against the current schemas before execution.
See [Planning](planning.md#consume-the-machine-contract).

## Start sparse

Keep only choices that differ from defaults.
`config migrate` reduces an existing file, such as saved `config print` output, to that form:

```bash
cargo rail config migrate                 # preview; changes no files
cargo rail config migrate --check         # exit 1 when a migration is pending
cargo rail config migrate apply           # revalidate drift, then write
cargo rail config migrate -f json > target/migrate.json
cargo rail config migrate apply --plan target/migrate.json
```

The migration removes a setting only when removing it leaves the effective policy unchanged,
so an older spelling of a default, such as `unify.compiler_targets = []`, is removed as well.
It keeps every other setting, comments, and ordering, and removes the header comment that `config print` writes.
A file containing only defaults is deleted
when no other discovery location holds a configuration file; otherwise,
and for a file outside the discovery locations, the emptied file is kept.
Keys that an earlier release removed, such as `[run]` or `release.require_changelog_entries`, have no current meaning,
so the migration removes them first and names the release that removed each one;
the JSON preview marks them with `removed_in`.
Every other command rejects such a key, names its release and replacement,
and points to this migration.
Retired `[release]` keys are first replaced by their successors, listed under `added` in the JSON preview,
so the release behaves as before.
A removed key that needs a manual edit, such as a split's `paths`, is reported but never changed.
The JSON preview includes `mutation_plan`.
`apply --plan` accepts that plan, saved to a file, only while the configuration and checkout are unchanged.
Save the plan in an ignored directory such as `target/` or outside the checkout:
an untracked plan file in the worktree is a checkout change.

```toml
targets = ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"]

[unify]
include_renamed = true
major_version_conflict = "bump"

[surface]
enabled = true

[release]
tag_format = "v{version}"

[plan.work.verification]
scope = "repository"
paths = ["verification/**"]
```

## Policy families

| Family             | Owns |
| ------------------ | ---- |
| `targets`          | Additional target triples used by target-aware workspace decisions |
| `[unify]`          | Dependency and manifest policy |
| `[surface]`        | Compiler-derived reachability and visibility policy |
| `[release]`        | Release intent, changelogs, and external-effect authority |
| `[plan.work.NAME]` | Positive inputs and selector shape for repository-owned work |
| `[crates.NAME]`    | Per-crate split, release, and changelog policy |

### Dependency policy

`cargo rail unify` previews one dependency decision.
`--check` makes the same decision and exits `1` when edits are pending.
`cargo rail unify apply` revalidates and writes the planned manifest changes.

Declare standalone Cargo workspaces with `unify.cargo_manifests`:

```toml
[unify]
cargo_manifests = ["fuzz/Cargo.toml", "tools/check/Cargo.toml"]
```

Each entry must name a contained workspace root with an existing `Cargo.lock`.
Unify captures and resolves each workspace independently
and reports its dependency repairs with its own snapshot and manifest.
Apply includes those exact manifests and lockfiles in drift checks, backups, and recovery.
This list is separate from `release.auxiliary_cargo_manifests` because the operations own different policy.
A normal dependency unused in every normal compilation unit,
retained by development compiler evidence, and referenced only in integration tests, examples,
or benchmarks is moved to its matching `dev-dependencies` table.
Production source and README references retain declarations conservatively.
The move preserves its rename, features, requirement, target condition, and comments.
An existing declaration in that destination is retained for manual review.

Cargo's unused-dependency lint is a fast signal for one build.
Unify checks the configured target and feature views before editing declarations.
Both retain dependencies that rustc counts as used through another crate's re-export;
Unify does not infer removal authority from source spelling alone.

Top-level `targets` always remain the dependency-resolution authority.
`unify.compiler_targets` selects where Unify acquires compiler evidence:

| Value             | Evidence targets |
| ----------------- | ---------------- |
| `"all"` (default) | Every top-level target. An empty list written by earlier releases means the same. |
| `["TARGET", ...]` | The exact top-level subset where Unify's workspace compiler command is valid. |
| `"none"`          | No target. Unify resolves dependencies but removes nothing that needs compiler proof. |

Dependencies that require an unobserved resolution target are retained
unless another complete proof makes the compiler view unnecessary;
the narrower evidence set never narrows Cargo resolution.
Before acquisition, Unify checks each target that needs new evidence:
its Rust target library must be installed,
and a target that compiles doctests must link a probe with its configured linker.
A failure names the target and stops before Cargo runs.
`cargo rail unify doctor` reports the same readiness and the unobserved targets.

Use `consumer_scope = "workspace"` only when the workspace contains every consumer of the affected private packages.
Published packages remain open-world.
A major-version unification requires explicit `major_version_conflict = "bump"` authority.
Exact pins, renamed dependencies, MSRV policy, preserved features,
and backup retention remain independent choices; inspect their current defaults with `config explain --all`.
`unify.msrv_policy` defaults to compute mode with `source = "max"` and `inherit = false`.
A partial compute table inherits omitted values; disabled mode rejects compute-only `source` and `inherit` fields.
`unify --explain` shows the exact planned package-inheritance and MSRV manifest edits.

### Surface policy

`surface.enabled = true` adds the Surface gate to planning.
Direct `cargo rail surface` inspection is available even when the gate is disabled.
Surface analyzes the host by default.
Set `surface.targets = "workspace"` to analyze the host and every top-level target,
or list a non-empty subset of `"host"` and the configured top-level targets.
Top-level `targets` are supported dependency-resolution views.
They do not assert that one host has every Rust target, SDK, linker,
or native dependency needed to execute those views.
Surface owns its execution matrix separately.

Keep `consumer_scope = "open"` unless the workspace owns every consumer of its non-publishable compiler crates.
Closed-world pruning requires reviewed evidence covering published APIs, downstream repositories,
plugins, generated code, and build-script or proc-macro consumers.
The current checkout's Cargo graph is not sufficient evidence by itself.
Declare products, external crates, feature profiles, doctest coverage, lint levels, overrides,
and exclusions only when Cargo-Rail's automatic workspace model does not express the intended
boundary.

`cargo rail surface --schema` exposes the report contract.
`surface --check` does not modify source; `surface --fix --dry-run` previews exact source edits before `surface --fix` receives write authority.
When JSON output is written with `--output`,
long-running acquisition keeps one bounded progress update on stderr every 30 seconds.
`--quiet` suppresses those updates.
JSON written to stdout remains silent on stderr.

Surface report contract v5 separates raw compiler observations from merged physical declarations.
Every selected target checks production code without linking.
Tests, examples, benchmarks, and doctests are acquired only on a selected host view.
The coverage object records their target views and reports incomplete target and doctest coverage
when cross targets are selected.
Cross-target inspection and checking remain available without a cross linker or `std`.
Visibility repair requires host-only targets because missing test views cannot authorize an edit.
Host doctests still require the host linker.
Its `retention` section reports both denominators, per-predicate observation and unique-item counts,
and at most three deterministic representatives per predicate.
`--explain` additionally runs an omit-one-reason counterfactual before diagnostic policy;
every other retention reason and graph authority remains active.
Those suppressed-finding counts measure conservative impact, not false positives.
Without `--explain`, the counterfactual is `null` and Surface performs only its three authoritative graph traversals.

### Release policy

`cargo rail release` needs no configuration; set `[release]` keys only to change its defaults.
Reviewed `.changes/*.md` files are the default source for bumps and release prose.
See [releasing a workspace](releases.md) for the command, its effects, and recovery.

| Key                         | Default                                          | Effect |
| --------------------------- | ------------------------------------------------ | ------ |
| `crates`                    | every member                                     | Crates a release manages; `change check` requires change files only for these. |
| `publish`                   | `"crates-io"`                                    | Registry that receives releases: `"crates-io"`, `"none"`, or a registry in Cargo's `[registries]`. Each crate's Cargo `package.publish` still decides whether it may publish there. |
| `github_release`            | `true` on a GitHub `origin`                      | Create a GitHub release for each tag. |
| `checks`                    | `"all"` on a GitHub `origin`, otherwise `"none"` | Require every check GitHub reports on the source commit to pass before the first effect. |
| `tag_format`                | `"{crate}-v{version}"`                           | Tag template; `{crate}` and `{version}` are its only placeholders. |
| `sign_tags`                 | `false`                                          | Sign release tags with the configured Git signing key. |
| `assets`                    | none                                             | Per-crate GitHub release files the draft must hold before publication. |
| `asset_attestations`        | `"required"`                                     | `"off"` accepts assets without GitHub artifact attestations. |
| `aliases`                   | none                                             | Per-crate mutable tags, such as `"v{major}"`, moved to each new release. |
| `auxiliary_cargo_manifests` | none                                             | Standalone workspaces whose lockfiles a release rewrites. |

`config explain --all` lists every key, including changelog shape, version groups, and semver policy.

The effective tag format controls both previous-tag lookup and new tags.
Repositories that use unqualified tags such as `v1.2.3` set `tag_format = "v{version}"`.
Override the format for one crate with `[crates.NAME.release].tag_format`:

```toml
[release]
tag_format = "v{version}"

[crates.provider.release]
tag_format = "{crate}-v{version}"
```

The other crates keep `v<version>` tags; `provider` looks up and creates only `provider-v<version>` tags.
Release output includes configuration warnings, including colliding effective tag formats.

Aliases move only forward and only after the crate's GitHub release is published.
`{major}` and `{minor}` follow the released version, so `"v{major}"` needs no edit for a new major release.

Asset names may use `{crate}` and `{version}`:

```toml
[release.assets]
my-cli = ["{crate}-{version}-x86_64-unknown-linux-gnu.zip", "SHA256SUMS"]
```

Standalone tools or fuzz workspaces
that depend on released crates by path must declare their exact manifests with `auxiliary_cargo_manifests`
when their independent lockfiles are tracked in Git.
A release inspects captured tracked lockfiles and uses Cargo metadata to identify workspaces
that resolve a released crate by its manifest path.
Discovery follows Cargo-declared path dependencies, including inactive optional dependencies.
An omitted workspace blocks the release before any effect.
A matching local lockfile identity that Cargo cannot attribute also blocks it
until the workspace is declared.
Packages with the same name and version
that Cargo attributes to another manifest remain independent.
Cargo-Rail resolves each manifest's committed `Cargo.lock`,
computes its post-release bytes in an isolated copy of the captured workspace,
and writes those bytes into the release commit.
The primary workspace uses the same projection.
Compatible path requirements of unreleased crates and path-only dependencies stay unchanged.
Released members retain the minimum-version refresh for their released local dependencies;
shared workspace declarations used by those members refresh with them.
An incompatible caret, tilde, or exact requirement on a local released crate is retargeted to its
new version; compound and inequality requirements that need editing are rejected before any effect.
Each manifest and lockfile must be a regular file whose index identity,
filter-cleaned worktree content, and executable mode exactly match `HEAD`;
every local Cargo package must resolve inside the captured Git source.
This is a Cargo-only projection, not a general command hook.

```toml
[release]
auxiliary_cargo_manifests = ["fuzz/Cargo.toml", "tools/check/Cargo.toml"]
```

### Repository work

`[plan.work.NAME]` declares inputs, not commands:

```toml
[plan.work.deliverables]
scope = "variants"
paths = ["deliverables/**"]
config = ["targets"]
variant_catalog = "variants.json"
```

`scope` is `repository`, `cargo`, or `variants`.
Paths are positive repository-relative globs.
`config` names exact effective fields.
`cargo` subscribes repository- or Cargo-scoped work to built-in Cargo decisions.
Variant work requires a checked-in catalog conforming to [`plan-variants-v2.schema.json`](../schemas/plan-variants-v2.schema.json).
Each row declares typed Cargo package or exact target roots plus external paths;
variant Cargo impact cannot be supplied by work-level string subscriptions.
`features` binds one exact no-default-feature set
and follows Cargo feature edges plus captured Rust module cfgs.
`manifest` selects an exact entry in `release.auxiliary_cargo_manifests` and resolves its committed lockfile.
Any source, cfg, manifest, or lock relationship Cargo-Rail cannot attribute exactly widens the
variant family.

A Cargo-scoped named work item may declare bounded one-hop artifact prerequisites:

```toml
[plan.work.runtime-artifacts]
scope = "cargo"
cargo_prerequisites = [
  { source_work = "cargo.test", when = [{ package = "integration" }], require = [
    { package = "server", target = { name = "server", kind = "bin" } },
    { package = "plugin", target = { name = "plugin", kind = "cdylib" } },
  ] },
]
```

`when` selects execution packages or targets.
`require` emits a separate build selector and adds the declared execution root when an artifact changes.
`require_target_kinds` requires every workspace target of the named Cargo kinds,
such as `["cdylib"]` for plugins that tests load at run time,
so a new target joins the edge without a configuration edit.
Package and target identities must resolve uniquely in captured Cargo metadata,
and each named kind must select at least one workspace target.
This is not a command hook or task graph.

Absolute paths, parent traversal, negative globs, commands, unknown fields,
and malformed catalogs are rejected.
See [Planning](planning.md) for selector and consumer rules.

### Per-crate policy

`[crates.NAME.split]` maps package-owned history and explicit non-Cargo assets to one split destination.
`include` adds non-Cargo files; `exclude` can only narrow that explicit set.
Cargo-owned package files cannot be excluded.

`[crates.NAME.release]` may narrow publication.
`[crates.NAME.changelog]` overrides workspace changelog policy for one crate.

## Rejected ownership

Repository `[cache]` configuration is rejected.
Remote URLs, credentials, provider environment, local CAS placement,
and distributed-worker selection are machine-owned cache setup.

Execution tables and path-category planning policy are rejected.
Keep commands in Cargo, nextest, Just, scripts, or CI,
and register only their positive inputs under `[plan.work.NAME]`.

## Exit behavior

| Command                    | Exit `0`                     | Exit `1`         | Exit `2` |
| -------------------------- | ---------------------------- | ---------------- | -------- |
| Mutation command `--check` | No mutation                  | Mutation pending | Invalid input or operation |
| `config validate`          | Valid at selected strictness | —                | Invalid or unreadable configuration, or a Cargo workspace that fails to load |

`config validate` enables strict mode in common CI environments.
Use `--no-strict` only when warnings are deliberately non-blocking.
