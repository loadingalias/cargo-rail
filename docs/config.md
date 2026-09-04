# Configuration

`rail.toml` contains sparse repository policy. An empty file is valid; omitted fields use coded defaults. Keep
commands, credentials, timeouts, setup, and execution in their owning tools.

## Find and inspect configuration

Cargo-Rail uses the first file found:

1. `rail.toml`
2. `.rail.toml`
3. `.cargo/rail.toml`
4. `.config/rail.toml`

`--config PATH` bypasses discovery. Relative paths are resolved from `--workspace-root`.

```bash
cargo rail init --dry-run
cargo rail init
cargo rail config locate
cargo rail config print
cargo rail config explain --all --json
cargo rail config validate --strict
cargo rail config migrate --check
```

`config print` emits canonical effective TOML, including defaults. `config explain` reports each configured and
effective value, its default, source, classification, and reason. Use those commands instead of
copying defaults from documentation.

`config migrate --check` previews the frozen v0.25 normalization and exits `1` when changes are pending. Review its
field actions before running `cargo rail config migrate`. Apply mode edits only recognized v0.25 fields, preserves
unrelated TOML, revalidates the file, and replaces it atomically without adding coded defaults. On Unix, success
reports the preserved previous file as `Previous configuration` (`previous_config` in JSON). A failure reports every
recovery path that remains.

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

| Family | Owns |
|---|---|
| `targets` | Additional target triples used by target-aware workspace decisions |
| `[unify]` | Dependency and manifest policy |
| `[surface]` | Compiler-derived reachability and visibility policy |
| `[release]` | Release intent, changelogs, and external-effect authority |
| `[plan.work.NAME]` | Positive inputs and selector shape for repository-owned work |
| `[crates.NAME]` | Per-crate split, release, and changelog policy |

### Dependency policy

`cargo rail unify` previews one dependency decision. `--check` makes the same decision and exits `1` when edits are
pending. `cargo rail unify apply` revalidates and writes the planned manifest changes.

Top-level `targets` always remain the dependency-resolution authority. By default, Unify acquires compiler evidence
on every one of those domains. A workspace whose supported target policy is package- or feature-specific can set
`unify.compiler_targets` to the exact top-level subset where Unify's workspace compiler command is valid. Dependencies
that require an omitted resolution domain are retained unless another complete proof makes the compiler view
unnecessary; the narrower evidence set never narrows Cargo resolution.

Use `consumer_scope = "workspace"` only when the workspace contains every consumer of the affected private packages.
Published packages remain open-world. A major-version unification requires explicit `major_version_conflict = "bump"`
authority. Exact pins, renamed dependencies, MSRV policy, preserved features, and backup retention remain independent
choices; inspect their current defaults with `config explain --all`.

### Surface policy

`surface.enabled = true` adds the Surface gate to planning. Direct `cargo rail surface` inspection is available even
when the gate is disabled. Surface analyzes the host by default. Set `surface.targets = "workspace"` to analyze the
host and every top-level target, or list an explicit non-empty subset.

Keep `consumer_scope = "open"` unless the workspace owns every consumer of its non-publishable compiler crates.
Declare products, external crates, feature profiles, doctest coverage, lint levels, overrides, and exclusions only
when Cargo-Rail's automatic workspace model does not express the intended boundary.

`cargo rail surface --schema` exposes the report contract. `surface --check` is read-only; `surface --fix --dry-run`
previews exact source edits before `surface --fix` receives write authority.

Surface report contract v3 separates raw compiler observations from merged physical declarations. Its `retention`
section reports both denominators, per-predicate observation and unique-item counts, and at most three deterministic
representatives per predicate. `--explain` additionally runs an omit-one-reason counterfactual before diagnostic
policy; every other retention reason and graph authority remains active. Those suppressed-finding counts measure
conservative impact, not false positives. Without `--explain`, the counterfactual is `null` and Surface performs only
its three authoritative graph traversals.

### Release policy

Reviewed `.changes/*.md` files are the default source for bumps and release prose. `cargo rail release check` previews
the local plan and exits `1` when a release is pending. It performs no mutation or external effect.

`remote_effects` controls Git and forge effects. Registry publication is separate and requires both
`registry_publication = "crates-io"` and `--publish` on the exact release invocation. Remote modes persist
transaction state before external effects. `release run --wait` stays attached while exact-SHA checks are pending;
without it, Cargo-Rail stops at the readiness boundary and reports the recorded state to resume instead of
replanning.

`release run --pr` is a separate reviewed path. Its `release finalize` step accepts only the merge commit that
introduces the exact prepared transaction; a later commit or unrelated merge cannot inherit that release.

Standalone tools or fuzz workspaces that depend on released packages by path can declare their exact manifests with
`auxiliary_cargo_manifests`. Cargo-Rail resolves each manifest's committed `Cargo.lock`, computes its post-release
bytes in an isolated copy of the captured workspace, binds the before/after digests into the release plan, and writes
only those lockfiles during release apply. Each manifest and lockfile must be a regular file whose index identity,
filter-cleaned worktree content, and executable mode exactly match `HEAD`; every local Cargo package must resolve
inside the captured Git source.
This is a Cargo-only projection, not a general command hook.

```toml
[release]
auxiliary_cargo_manifests = ["fuzz/Cargo.toml", "tools/check/Cargo.toml"]
```

Cargo-Rail uses fixed changelog structure rather than a template engine; put release prose in reviewed change files.

### Repository work

`[plan.work.NAME]` declares inputs, not commands:

```toml
[plan.work.deliverables]
scope = "variants"
paths = ["deliverables/**"]
config = ["targets"]
variant_catalog = "variants.json"
```

`scope` is `repository`, `cargo`, or `variants`. Paths are positive repository-relative globs. `config` names exact
effective fields. `cargo` subscribes repository- or Cargo-scoped work to built-in Cargo decisions. Variant work
requires a checked-in catalog conforming to
[`plan-variants-v2.schema.json`](../schemas/plan-variants-v2.schema.json). Each row declares typed Cargo package or
exact target roots plus external paths; variant Cargo impact cannot be supplied by work-level string subscriptions.
`features` binds one exact no-default-feature set and follows Cargo feature edges plus captured Rust module cfgs.
`manifest` selects an exact entry in `release.auxiliary_cargo_manifests` and resolves its committed lockfile. Any
source, cfg, manifest, or lock relationship Cargo-Rail cannot attribute exactly widens the variant family.

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

`when` selects execution packages or targets. `require` emits a separate build selector and adds the declared
execution root when an artifact changes. Package and target identities must resolve uniquely in captured Cargo
metadata. This is not a command hook or task graph.

Absolute paths, parent traversal, negative globs, commands, unknown fields, and malformed catalogs are rejected. See
[Planning](planning.md) for selector and consumer rules.

### Per-crate policy

`[crates.NAME.split]` maps package-owned history and explicit non-Cargo assets to one split destination. `include` adds
non-Cargo files; `exclude` can only narrow that explicit set. Cargo-owned package files cannot be excluded.

`[crates.NAME.release]` may narrow publication. `[crates.NAME.changelog]` overrides workspace changelog policy for one
crate.

## Rejected ownership

Repository `[cache]` configuration is rejected. Remote URLs, credentials, provider environment, local CAS placement,
and distributed-worker selection are machine-owned cache setup.

Execution tables and path-category planning policy are rejected. Keep commands in Cargo, nextest, Just, scripts, or
CI, and register only their positive inputs under `[plan.work.NAME]`.

## Exit behavior

| Command | Exit `0` | Exit `1` | Exit `2` |
|---|---|---|---|
| `config migrate --check` | No migration | Migration pending | Invalid input or operation |
| Mutation command `--check` | No mutation | Mutation pending | Invalid input or operation |
| `config validate` | Valid at selected strictness | — | Invalid or unreadable configuration |

`config validate` enables strict mode in common CI environments. Use `--no-strict` only when warnings are deliberately
non-blocking.
