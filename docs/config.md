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
```

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

## Compatibility

The current Cargo-Rail release accepts only current configuration fields.
Configuration inspection leaves the input file unchanged;
unsupported keys fail before planning or mutation.
Historical comparisons apply the same decoder and identify the revision and configuration path
when decoding fails.

Review existing policy against `config explain --all` from a workspace with supported configuration.
Update the configuration file explicitly, then run `cargo rail config validate --strict`;
Cargo-Rail does not rewrite unsupported fields automatically.

Plans use contract v9 and variant catalogs use v2.
Regenerate older saved plans and validate catalogs against the current schemas before execution.
See [Planning](planning.md#consume-the-machine-contract).

## Start sparse

Keep only choices that differ from defaults:

```toml
targets = ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"]

[unify]
include_renamed = true
major_version_conflict = "bump"

[surface]
enabled = true

[release]
remote_effects = "auto"

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

Top-level `targets` always remain the dependency-resolution authority.
By default, Unify acquires compiler evidence on every one of those domains.
A workspace whose supported target policy is package-
or feature-specific can set `unify.compiler_targets` to the exact top-level subset
where Unify's workspace compiler command is valid.
Dependencies that require an omitted resolution domain are retained
unless another complete proof makes the compiler view unnecessary;
the narrower evidence set never narrows Cargo resolution.

Use `consumer_scope = "workspace"` only when the workspace contains every consumer of the affected private packages.
Published packages remain open-world.
A major-version unification requires explicit `major_version_conflict = "bump"` authority.
Exact pins, renamed dependencies, MSRV policy, preserved features,
and backup retention remain independent choices; inspect their current defaults with `config explain --all`.

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

Surface report contract v4 separates raw compiler observations from merged physical declarations
and reports target scope independently from complete product, feature-profile, and doctest coverage.
Its `retention` section reports both denominators, per-predicate observation and unique-item counts,
and at most three deterministic representatives per predicate.
`--explain` additionally runs an omit-one-reason counterfactual before diagnostic policy;
every other retention reason and graph authority remains active.
Those suppressed-finding counts measure conservative impact, not false positives.
Without `--explain`, the counterfactual is `null` and Surface performs only its three authoritative graph traversals.

### Release policy

Reviewed `.changes/*.md` files are the default source for bumps and release prose.
`cargo rail release check` previews the local plan and exits `1` when a release is pending.
It performs no mutation or external effect.

`remote_effects` controls Git and forge effects.
Registry publication is separate and requires both `registry_publication = "crates-io"` and `--publish` on the exact release invocation.

GitHub releases also require explicit validation workflows and job names:

```toml
[release.validation]
".github/workflows/ci.yml" = ["tests", "lint"]
```

Use the jobs' displayed names, including matrix values where applicable.
Cargo-Rail requires successful jobs from one exact workflow run and attempt for the release commit.
Missing, skipped, failed, ambiguous, or unavailable required jobs block publication.
The release executor cannot authorize itself as validation.
Retained validation attempts cannot be replaced on retry.
Each workflow query and job inventory is bounded to 100 results;
a larger or incomplete response blocks release.
Set `release.hosted_workflow` to submit durable requests to a separate GitHub workflow.
The hosted executor dispatches validation explicitly and waits for its exact runs.
Without that setting, execution is local; `--local` overrides it for a new request.
`release run --pr` adds a review boundary to the same transaction.
The merged tree must match preparation,
and the merge event continues validation and publication on its exact commit.
See [releasing a workspace](releases.md) for the workflow contract and recovery behavior.

Optional `release.aliases = { my-action = "v9" }` authorizes a mutable tag for a package.
Cargo-Rail captures its prior object in the intent and promotes it with a Git lease only
after verifying the immutable forge release.
Aliases require tags and forge publication.

Standalone tools or fuzz workspaces
that depend on released packages by path can declare their exact manifests with `auxiliary_cargo_manifests`.
Cargo-Rail resolves each manifest's committed `Cargo.lock`,
computes its post-release bytes in an isolated copy of the captured workspace,
binds the before/after digests into the release plan,
and writes only those lockfiles during release apply.
Each manifest and lockfile must be a regular file whose index identity,
filter-cleaned worktree content, and executable mode exactly match `HEAD`;
every local Cargo package must resolve inside the captured Git source.
This is a Cargo-only projection, not a general command hook.

```toml
[release]
auxiliary_cargo_manifests = ["fuzz/Cargo.toml", "tools/check/Cargo.toml"]
```

Release changelogs use a small fixed-placeholder formatter for commit sourced releases.
Reviewed change files remain the default release source and may carry optional presentation metadata
for authored Markdown.
Configure section order, labels, filters, links, and per-crate changelog paths under `[release.changelog]` or `[crates.NAME.changelog]`;
configure an independently authored forge body with files in `release_notes_dir`.

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
Package and target identities must resolve uniquely in captured Cargo metadata.
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
| `config validate`          | Valid at selected strictness | —                | Invalid or unreadable configuration |

`config validate` enables strict mode in common CI environments.
Use `--no-strict` only when warnings are deliberately non-blocking.

### Native release assets

Declare optional assets per package and name their producer in `release.validation`.
A target is an assertion made by the authorized packaging job;
that job must validate the product it builds.
The executor verifies byte identity and producer authority.
Use `source` to require an asset to match a regular committed file, including a license.

```toml
[release.validation]
".github/workflows/build.yml" = ["package", "assemble"]

[release.artifacts.my-cli]
workflow = ".github/workflows/build.yml"

[release.artifacts.my-cli.files."{crate}-{version}-x86_64-unknown-linux-gnu.zip"]
target = "x86_64-unknown-linux-gnu"

[release.artifacts.my-cli.files.LICENSE]
source = "LICENSE"
```

Upload exactly these files in the producer's `release-my-cli-<run-id>-<attempt>` artifact.
See [native asset verification and recovery](release-records.md#native-assets) for the transport contract and bounds.
This integration requires GitHub release effects and tags.
