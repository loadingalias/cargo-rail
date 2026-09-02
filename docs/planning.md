# Planning

`cargo rail plan` decides which registered work is required and emits the exact scope for each required item. It does
not execute repository commands. Cargo, nextest, Just, scripts, and CI retain execution semantics.

A successful plan exits `0` whether work is required or skipped. Invalid arguments, incompatible contracts, and
operational failures exit `2`.

## Compare source states

By default, Cargo-Rail compares the default branch's merge base with the captured index, worktree, and untracked
files:

```bash
cargo rail plan
cargo rail plan --explain
cargo rail plan --json > plan.json
```

Use `--since <REF>` for an exact base. Use `--from <REF> --to <REF>` for an exact Git-object comparison that ignores
the live worktree. `--all` is the only normal override; it requires every registered item and never narrows work.

One `WorkspaceContext` supplies the captured source, Cargo graph, dependency domains, effective configuration,
toolchain, targets, and compatible evidence. Missing or incomplete evidence requires only the work item that owns the
gap.

## Register repository work

Pure Cargo workspaces need no planning configuration. Cargo-Rail already owns the built-in Cargo, dependency-policy,
release-semver, and Surface decisions.

Register only positive inputs for repository-specific work. Keep commands in Just, scripts, or CI:

```toml
[plan.work.workflow-policy]
scope = "repository"
paths = [".github/**", "scripts/ci/**"]
config = ["targets"]

[plan.work.compatibility]
scope = "variants"
paths = ["tests/compatibility/**"]
variant_catalog = "distribution/compatibility-plan-variants.json"
```

| Scope | Plan output | Use |
|---|---|---|
| `repository` | Required or skipped | Gate a whole-repository command |
| `cargo` | Typed package and target selection | Pass the emitted Cargo arguments to a compatible command |
| `variants` | Selected catalog rows or explicit `all` | Materialize only the emitted workflow variants |

`cargo` subscriptions inherit the selected built-in Cargo scope. A changed declared path adds its package scope. A
changed configuration input widens the declared work to the Cargo workspace because policy can affect every member.

Variant catalog v2 models deliverable impact without subscribing the deliverable to conservative `cargo.build`.
Each row declares `cargo_roots` containing exact packages or targets and `external_paths` for inputs outside Cargo's
graph. A root may add `features` to name one exact no-default-feature set. Cargo-Rail expands member-local Cargo
feature edges and follows ordinary external Rust modules from the captured target root. Only a source path proved
active for that feature set narrows selection; malformed attributes, path overrides, inline modules, platform or
custom cfgs, and unattributed Rust files widen to every row.

An auxiliary root adds `manifest` with the exact repository-relative path of an entry in
`release.auxiliary_cargo_manifests`. Cargo-Rail loads its locked metadata from the same captured source, keeps local
packages inside that source root, and applies the normal structural reverse build closure. Auxiliary manifests,
root lockfiles, shared inputs, and catalog changes remain all-row wideners. Root Cargo manifests, the primary
lockfile, Cargo configuration, and the toolchain likewise select every Cargo-rooted row.

```json
{
  "package": "crypto",
  "features": ["signatures"]
}
```

```json
{
  "manifest": "fuzz-packages/rsa/Cargo.toml",
  "package": "fuzz-rsa"
}
```

Cargo-Rail computes structural impact once across the captured Cargo domains and selects rows whose roots are
affected. Unrelated external paths do not select a deliverable merely because compiler input evidence is incomplete.
If a required path, configuration input, or Cargo input is not attributed by any selected catalog row, Cargo-Rail
selects every row rather than treating the gap as evidence that a deliverable is unaffected.

Runtime artifacts remain separate from test execution. A Cargo-scoped named work item with `cargo_prerequisites`
emits only prerequisite packages and targets; the source `cargo.test` selector still owns which tests execute.
Changing a declared artifact propagates back to its explicitly named test root. Relationships are one hop and contain
no commands.

Work IDs use lowercase ASCII letters, digits, dots, and hyphens. Paths are positive repository-relative globs.
Absolute paths, parent traversal, negative patterns, commands, unknown configuration fields, and malformed variant
catalogs are rejected.

## Consume the machine contract

`cargo rail plan --json` writes one schema-owned value. The checked-in contract is
[`plan-v8.schema.json`](../schemas/plan-v8.schema.json):

```bash
cargo rail plan --schema > plan.schema.json
cargo rail plan --json > plan.json
```

The fields with execution authority are:

- `identity`: the content-derived decision identity;
- `inputs`: comparison, source, Cargo, configuration, toolchain, platform, catalog, and evidence bindings;
- `work`: every tagged required or skipped decision;
- `required`: the sorted projection of required work IDs; and
- `scope`: the selector attached only to required work.

`changes`, causes, explanations, and evidence describe a decision; they are not selectors. Read
`scope.selection.cargo_args` as an argument array, never as shell text. Treat variant `kind = "all"` as an instruction
to materialize the owning workflow's complete checked-in catalog.

Before each executor:

1. Validate the complete plan and required-work projection.
2. Select one known required work item.
3. Lower only that item's typed scope.
4. Run the matching reader's checkout verification in the execution workspace.
5. Start the executor only after verification succeeds.

For local and captured-worktree plans, `verify-checkout` delegates to the matching Cargo-Rail binary and binds the
plan to the exact head plus captured Git provenance. Comparing `HEAD` alone is insufficient. Drift exits `2` before
selectors are emitted or work starts.

Cross-platform CI can transfer an object-bound plan as a portable bundle:

```bash
RAIL_SINCE=BASE_SHA RAIL_OBJECT_HEAD=HEAD_SHA scripts/plan/read.py create target/plan-v8.json
scripts/plan/read.py bundle target/plan-v8.json target --producer-version 0.26.0
python3 target/plan-read.py verify-bundle target/plan-bundle-v1.json
```

[`plan-bundle-v1.schema.json`](../schemas/plan-bundle-v1.schema.json) names the plan contract, producer version,
platform limits, clean-object source authority, file roles, byte sizes, and SHA-256 hashes. The bundle contains the
plan and its exact reader, not a host executable. The bundled reader verifies its own inventory, the complete plan,
the exact plan identity, `HEAD`, and a clean tracked/untracked/submodule checkout. Portable verification requires Git
and Python 3.10 or newer on Linux, macOS, or Windows; it deliberately rejects `WORKTREE` plans.

`scripts/plan/read.py` is the strict reference consumer. It validates identities, projections, selector shapes,
checkout binding, and workflow matrices before emitting NUL-delimited arguments. `cargo-scope` distinguishes
`skipped`, `workspace`, and `packages`; `package-names` emits names only for exact package scope and rejects duplicate
names.

## Observed-input evidence

Pass compatible evidence explicitly:

```bash
cargo rail plan --evidence planning-evidence.json --json
```

[`planning-evidence-v1.schema.json`](../schemas/planning-evidence-v1.schema.json) binds evidence to its source, Cargo
universe, configuration, toolchain, target, platform, provider capabilities, and work kind. Evidence proves a skip
only when every relevant input class is complete and has no bypass. Missing, stale, malformed, or cross-platform
evidence widens only its owning work.

A plan identity compares decisions. It is not a cache key and never authorizes compiler-result reuse.

The current planner executes only variant catalog v2. The v1 schema remains available to validate stored artifacts;
convert active catalogs to v2 before planning with the current release.

## Diagnose a decision

```bash
cargo rail config validate --strict
cargo rail plan --explain
cargo rail plan --json | jq '{inputs, changes, required, work}'
```

See [Troubleshooting](troubleshooting.md) when the compared states, selected work, or executor scope differ from
expectation.
