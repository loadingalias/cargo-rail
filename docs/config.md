# Configuration

`rail.toml` contains sparse repository policy.
When a setting or the file is absent, Cargo-Rail uses the coded default.
Keep commands, credentials, timeouts, setup, and execution in the tools that own them.

## Find and inspect configuration

Cargo-Rail uses the first file that it finds in this order:

1. `rail.toml`
1. `.rail.toml`
1. `.cargo/rail.toml`
1. `.config/rail.toml`

`--config PATH` skips discovery.
Cargo-Rail resolves relative paths from the selected workspace root.
To select a different workspace, use `--workspace-root PATH`.

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

`init` recognizes an explicit `[workspace]` manifest or a root `[package]` manifest.
If no Cargo manifest exists, `init` still writes the requested configuration.
It also reports that it detected no workspace.
`init` refuses to write below the Cargo workspace root, because discovery never reads a file there.
When another discovery location already holds the active file, `init` refuses to write, even with `--force`.
To replace the active file, pass its exact path with `--output` and `--force`.

`cargo rail config` without a subcommand shows the configured overrides and the active source.
When no configuration file exists, it reports that the coded defaults apply.
`config explain --all` reports the configured value, effective value, default, source, and rationale of each field.
To explain exact fields, give their leaf paths.
To list the effective children of a parent node, give the parent path, for example `config explain surface --json`.
Both commands validate the policy before they report success.
Unknown keys, unreadable files, and failed Cargo workspace discovery are errors.
A missing explicit `--config PATH` file is also an error.
Cargo-Rail never uses the defaults in its place.

`config print` writes canonical TOML that includes the defaults.
It writes policy values such as `surface.targets = "workspace"` without resolving them.
As a result, saved output continues to follow later changes to the top-level targets.
`config explain` shows the resolved targets.
For machine inspection, use `-f json` or `--json`.
The JSON output of `config explain` conforms to [`config-explain-v2.schema.json`](../schemas/config-explain-v2.schema.json).

To validate canonical output through stdin, use this command:

```bash
cargo rail config print | cargo rail --config - config validate --strict
```

Stdin validation does not depend on the checkout.
Some policy needs workspace facts, such as split paths, package selections,
or a transitive host path.
For that policy, stdin validation reports the missing context.
Inspect that policy from a file in the workspace that owns it.
Inspection of a file validates the policy against the selected Cargo workspace.
When `Cargo.lock` exists, validation resolves it with `--locked`, as the consuming commands do.
Thus a stale lockfile fails validation.
Validation never creates a lockfile.
`config validate` states what it checked.
`workspace` evidence binds the policy to the Cargo workspace.
`schema` evidence applies to stdin or to a directory without `Cargo.toml`.
The JSON output reports this value in `evidence`.

## Compatibility

The current Cargo-Rail release accepts only current configuration fields.
Configuration inspection does not change the input file.
An unsupported key causes a failure before planning or mutation starts.
The error for a retired key names the release that removed it.
Historical comparisons read retired keys with their current meaning.
When decoding fails, the error identifies the revision and the configuration path.

`cargo rail config migrate` previews the update.
`config migrate apply` writes it.
The migration replaces each retired `[release]` key from Cargo-Rail 0.31 with its successor.
It writes successors only to a file that contains a retired `[release]` key or the `{prefix}` placeholder.
In that file, the successor keeps the behavior of the retired key:

- Without `registry_publication = "crates-io"`, the file gets `publish = "none"`.
- Without `remote_effects = "github"` or `"auto"`, the file gets `github_release = false` and `checks = "none"`.
- `{prefix}` in a tag format becomes the prefix itself.
- An artifact inventory becomes `release.assets`.

Without such a file, the release uses the 0.32 defaults.
By default, Cargo-Rail 0.31 published to no registry and created no GitHub releases.
By default, Cargo-Rail 0.32 publishes to crates.io.
On a GitHub `origin`, it also creates GitHub releases and uses `checks = "all"`.
To keep publication and GitHub releases off, as in 0.31, set `publish = "none"`, `github_release = false`, and `checks = "none"` in `[release]`.
Publication still needs a registry credential, and Cargo-Rail checks for it before any effect.

The migration also removes settings that only repeat the current defaults.
See [Start sparse](#start-sparse).
Then run `cargo rail config validate --strict`.

Plans use contract v9.
Variant catalogs use v2.
Before execution, regenerate older saved plans and validate catalogs against the current schemas.
See [Planning](planning.md#consume-the-machine-contract).

## Start sparse

Keep only the settings that differ from the defaults.
`config migrate` reduces an existing file to that form, for example saved `config print` output:

```bash
cargo rail config migrate                 # preview; changes no files
cargo rail config migrate --check         # exit 1 when a migration is pending
cargo rail config migrate apply           # revalidate drift, then write
cargo rail config migrate -f json > target/migrate.json
cargo rail config migrate apply --plan target/migrate.json
```

The migration removes a setting only when the removal does not change the effective policy.
Thus it also removes an older spelling of a default, such as `unify.compiler_targets = []`.
It keeps every other setting, the comments, and the order.
It removes the header comment that `config print` writes.
When a file contains only defaults,
the migration deletes it if no other discovery location holds a configuration file.
Otherwise, the migration keeps the emptied file.
The migration also keeps an emptied file that is outside the discovery locations,
and an emptied file that `--config` selects explicitly.

Keys that an earlier release removed, such as `[run]` or `release.require_changelog_entries`, have no current meaning.
The migration removes them first and names the release that removed each one.
The JSON preview marks them with `removed_in`.
Every other command rejects such a key.
Its error names the release that removed the key and the replacement, and points to this migration.
The migration first replaces retired `[release]` keys with their successors, so the release behaves as before.
The JSON preview lists those successors under `added`.
The migration reports a removed key that needs a manual edit, such as a split's `paths`,
but never changes it.

The JSON preview includes `mutation_plan`.
`apply --plan` accepts that plan from a file.
It accepts the plan only while the configuration and the checkout are unchanged.
The plan file itself is not a checkout change.
The preview ignores the file that it writes through its standard output or `-o`.
Apply ignores the file that `--plan` names.
Any other untracked file in the worktree is a checkout change.

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
| `targets`          | Additional target triples that target-aware workspace decisions use |
| `[unify]`          | Dependency and manifest policy |
| `[surface]`        | Compiler-derived reachability and visibility policy |
| `[release]`        | Release intent, changelogs, and external-effect authority |
| `[plan.work.NAME]` | Positive inputs and selector shape for repository-owned work |
| `[crates.NAME]`    | Per-crate split, release, and changelog policy |

### Dependency policy

`cargo rail unify` previews one dependency decision.
`--check` makes the same decision and exits `1` when edits are pending.
`cargo rail unify apply` revalidates and writes the planned manifest changes.

To declare standalone Cargo workspaces, use `unify.cargo_manifests`:

```toml
[unify]
cargo_manifests = ["fuzz/Cargo.toml", "tools/check/Cargo.toml"]
```

Each entry must name a contained workspace root that has an existing `Cargo.lock`.
Unify captures and resolves each workspace independently.
It reports the dependency repairs of each workspace with the snapshot and manifest of
that workspace.
Apply includes those exact manifests and lockfiles in drift checks, backups, and recovery.
This list is separate from `release.auxiliary_cargo_manifests`, because the two operations own different policy.

Unify moves a normal dependency to its matching `dev-dependencies` table when all of these conditions are true:

- No normal compilation unit uses the dependency.
- Development compiler evidence retains the dependency.
- Only integration tests, examples, or benchmarks refer to the dependency.

A reference in production source or in a README keeps the declaration, as a conservative choice.
The move keeps the rename, features, requirement, target condition, and comments of the declaration.
Unify keeps an existing declaration in that destination for manual review.

Cargo's unused-dependency lint is a fast signal for one build.
Before Unify edits declarations, it checks the configured target and feature views.
The lint and Unify both keep a dependency that rustc counts
as used through the re-export of another crate.
Unify does not use source spelling alone as authority to remove a dependency.

Top-level `targets` always stay the authority for dependency resolution.
`unify.compiler_targets` selects the targets where Unify acquires compiler evidence:

| Value             | Evidence targets |
| ----------------- | ---------------- |
| `"all"` (default) | Every top-level target. An empty list from earlier releases has the same meaning. |
| `["TARGET", ...]` | The exact subset of top-level targets where the Unify workspace compiler command is valid. |
| `"none"`          | No target. Unify resolves dependencies but removes nothing that needs compiler proof. |

Unify keeps a dependency that needs an unobserved resolution target,
unless another complete proof makes the compiler view unnecessary.
The narrower evidence set never narrows Cargo resolution.
Before acquisition, Unify checks each target that needs new evidence:

- The Rust target library of the target must be installed.
- A target that compiles doctests must link a probe with its configured linker.

If a check fails, Unify names the target and stops before Cargo runs.
`cargo rail unify doctor` reports the same readiness and the unobserved targets.

Use `consumer_scope = "workspace"` only when the workspace contains every consumer of the affected private packages.
Published packages stay open-world.
A major-version unification needs the explicit authority of `major_version_conflict = "bump"`.
Exact pins, renamed dependencies, MSRV policy, preserved features,
and backup retention are independent choices.
To see their current defaults, run `config explain --all`.
The default `unify.msrv_policy` is compute mode with `source = "max"` and `inherit = false`.
In a partial compute table, each omitted value uses its default.
Disabled mode rejects the compute-only fields `source` and `inherit`.
`unify --explain` shows the exact manifest edits that it plans for package inheritance and MSRV.

### Surface policy

`surface.enabled = true` adds the Surface gate to planning.
You can run `cargo rail surface` directly for inspection even when the gate is disabled.
By default, Surface analyzes the host.
To analyze the host and every top-level target, set `surface.targets = "workspace"`.
Alternatively, list a non-empty subset of `"host"` and the configured top-level targets.
Top-level `targets` are supported views for dependency resolution.
They do not assert that one host has every Rust target, SDK, linker,
or native dependency that is necessary to execute those views.
Surface owns its execution matrix separately.

Keep `consumer_scope = "open"` unless the workspace owns every consumer of its non-publishable compiler crates.
Closed-world pruning needs reviewed evidence.
That evidence must cover published APIs, downstream repositories, plugins, generated code,
and build-script or proc-macro consumers.
The Cargo graph of the current checkout is not sufficient evidence by itself.
Declare products, external crates, feature profiles, doctest coverage, lint levels, overrides,
and exclusions only when the automatic workspace model of Cargo-Rail does not express the intended
boundary.

`cargo rail surface --schema` shows the report contract.
`surface --check` does not change source.
Before you give write authority with `surface --fix`, preview the exact source edits with `surface --fix --dry-run`.
When `--output` receives the JSON output,
long-running acquisition writes one bounded progress update to stderr every 30 seconds.
`--quiet` disables those updates.
When the JSON output goes to stdout, stderr stays silent.

Surface report contract v5 separates raw compiler observations from merged physical declarations.
For every selected target, Surface checks production code without linking.
Surface acquires tests, examples, benchmarks, and doctests only on a selected host view.
The coverage object records their target views.
When cross targets are selected, the coverage object reports incomplete target and doctest coverage.
Cross-target inspection and checking stay available without a cross linker or `std`.
Visibility repair needs host-only targets, because missing test views cannot authorize an edit.
Host doctests still need the host linker.

The `retention` section of the report contains:

- both denominators;
- the observation count and the unique-item count of each predicate;
- at most three deterministic representatives of each predicate.

With `--explain`, Surface also runs an omit-one-reason counterfactual before diagnostic policy.
In that counterfactual, every other retention reason and graph authority stays active.
The counts of suppressed findings measure conservative impact, not false positives.
Without `--explain`, the counterfactual is `null`.
Surface then does only its three authoritative graph traversals.

### Release policy

`cargo rail release` needs no configuration.
Set `[release]` keys only to change its defaults.
By default, the bumps and the release text come from reviewed `.changes/*.md` files.
For the command, its effects, and recovery, see [releasing a workspace](releases.md).

| Key                         | Default                                          | Effect |
| --------------------------- | ------------------------------------------------ | ------ |
| `crates`                    | every member                                     | The crates that a release manages. `change check` requires change files only for these crates. |
| `publish`                   | `"crates-io"`                                    | The registry that receives releases: `"crates-io"`, `"none"`, or a registry in Cargo's `[registries]`. The Cargo `package.publish` of each crate still decides whether the crate can publish there. |
| `github_release`            | `true` on a GitHub `origin`                      | Create a GitHub release for each tag. |
| `checks`                    | `"all"` on a GitHub `origin`, otherwise `"none"` | Before the first effect, require every check that GitHub reports on the source commit to pass. |
| `tag_format`                | `"{crate}-v{version}"`                           | The tag template. `{crate}` and `{version}` are its only placeholders. |
| `sign_tags`                 | `false`                                          | Sign release tags with the configured Git signing key. |
| `assets`                    | none                                             | Per-crate GitHub release files that the draft must hold before publication. |
| `asset_attestations`        | `"required"`                                     | `"off"` accepts assets without GitHub artifact attestations. |
| `aliases`                   | none                                             | Per-crate mutable tags, such as `"v{major}"`, that move to each new release. |
| `auxiliary_cargo_manifests` | none                                             | Standalone workspaces whose lockfiles a release rewrites. |

`config explain --all` lists every key, including changelog shape, version groups, and semver policy.

The effective tag format controls the lookup of the previous tag and the names of new tags.
A repository that uses unqualified tags, such as `v1.2.3`, sets `tag_format = "v{version}"`.
To override the format for one crate, use `[crates.NAME.release].tag_format`:

```toml
[release]
tag_format = "v{version}"

[crates.provider.release]
tag_format = "{crate}-v{version}"
```

The other crates keep `v<version>` tags.
`provider` looks up and creates only `provider-v<version>` tags.
Release output includes configuration warnings, for example about colliding effective tag formats.

An alias moves only forward.
It moves only after the GitHub release of the crate is published.
`{major}` and `{minor}` follow the released version.
Thus `"v{major}"` needs no edit for a new major release.

Asset names can use `{crate}` and `{version}`:

```toml
[release.assets]
my-cli = ["{crate}-{version}-x86_64-unknown-linux-gnu.zip", "SHA256SUMS"]
```

A standalone tool or fuzz workspace can depend on released crates by path.
If Git tracks the independent lockfile of such a workspace, declare its exact manifest with `auxiliary_cargo_manifests`.
A release inspects the captured tracked lockfiles.
It uses Cargo metadata to find the workspaces that resolve a released crate by its manifest path.
Discovery follows Cargo-declared path dependencies, including inactive optional dependencies.
An undeclared workspace blocks the release before any effect.
A matching local lockfile identity that Cargo cannot attribute also blocks the release
until you declare the workspace.
Packages with the same name and version that Cargo attributes to another manifest stay independent.

For each manifest, Cargo-Rail resolves the committed `Cargo.lock`.
It computes the post-release bytes in an isolated copy of the captured workspace.
Then it writes those bytes into the release commit.
The primary workspace uses the same projection.
Compatible path requirements of unreleased crates and path-only dependencies stay unchanged.
Released members keep the minimum-version refresh for their released local dependencies.
Shared workspace declarations that those members use refresh with them.
Cargo-Rail retargets an incompatible caret, tilde,
or exact requirement on a local released crate to its new version.
Before any effect, it rejects compound and inequality requirements that need an edit.
Each manifest and lockfile must be a regular file.
Its index identity, filter-cleaned worktree content, and executable mode must exactly match `HEAD`.
Every local Cargo package must resolve inside the captured Git source.
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
Variant work needs a checked-in catalog that conforms to [`plan-variants-v2.schema.json`](../schemas/plan-variants-v2.schema.json).
Each row declares typed roots and external paths.
A typed root is a Cargo package or an exact target.
Work-level string subscriptions cannot supply the Cargo impact of a variant.
`features` binds one exact no-default-feature set.
It follows Cargo feature edges and captured Rust module cfgs.
`manifest` selects an exact entry in `release.auxiliary_cargo_manifests` and resolves its committed lockfile.
If Cargo-Rail cannot attribute a source, cfg, manifest, or lock relationship exactly,
that relationship widens the variant family.

A Cargo-scoped named work item can declare bounded one-hop artifact prerequisites:

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
`require` emits a separate build selector.
When an artifact changes, `require` adds the declared execution root.
`require_target_kinds` requires every workspace target of the named Cargo kinds,
such as `["cdylib"]` for plugins that tests load at run time.
Thus a new target joins the edge without a configuration edit.
Package and target identities must resolve uniquely in captured Cargo metadata.
Each named kind must select at least one workspace target.
This is not a command hook or task graph.

Cargo-Rail rejects absolute paths, parent traversal, negative globs, commands, unknown fields,
and malformed catalogs.
For selector and consumer rules, see [Planning](planning.md).

### Per-crate policy

`[crates.NAME.split]` maps package-owned history and explicit non-Cargo assets to one split destination.
`include` adds non-Cargo files.
`exclude` can only narrow that explicit set.
`exclude` cannot remove Cargo-owned package files.

`[crates.NAME.release]` can narrow publication.
`[crates.NAME.changelog]` overrides workspace changelog policy for one crate.

## Rejected ownership

Cargo-Rail rejects repository `[cache]` configuration.
Remote URLs, credentials, provider environment, local CAS placement,
and distributed-worker selection are machine-owned cache setup.

Cargo-Rail also rejects execution tables and path-category planning policy.
Keep commands in Cargo, nextest, Just, scripts, or CI.
Register only their positive inputs under `[plan.work.NAME]`.

## Exit behavior

| Command                    | Exit `0`                     | Exit `1`         | Exit `2` |
| -------------------------- | ---------------------------- | ---------------- | -------- |
| Mutation command `--check` | No mutation                  | Mutation pending | Invalid input or operation |
| `config validate`          | Valid at selected strictness | —                | Invalid or unreadable configuration, or a Cargo workspace that fails to load |

In common CI environments, `config validate` uses strict mode.
Use `--no-strict` only when you decide that warnings must not block.
