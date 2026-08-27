# Planning

`cargo rail plan` produces one evidence-backed decision for every registered unit of work. Cargo, cargo-nextest,
Just, scripts, and CI retain command semantics; they consume the plan's typed selectors instead of reclassifying paths
or recomputing dependency impact.

Planning never executes repository code. A successful plan exits `0` whether work is required or skipped. Invalid
arguments, incompatible contracts, and operational failures exit `2`.

## Compare source states

The default command compares the default branch's merge base with the captured worktree:

```bash
cargo rail plan
cargo rail plan --explain
cargo rail plan --json > plan.json
```

Use `--since <REF>` when the exact base is already known. Use the inseparable `--from <REF> --to <REF>` pair for an
exact Git-object comparison that must ignore index, worktree, and untracked state. `--merge-base` is a compatibility
alias for the default comparison.

`--all` is the only normal policy override. It monotonically changes every registered item to required and gives it a
complete valid scope. No normal option narrows work.

## How decisions are built

One `WorkspaceContext` owns the source capture, Cargo metadata, dependency universe, effective configuration, and
toolchain view used by a plan. Evaluation proceeds from that authority:

1. A sparse captured source view retains add, modify, delete, rename, provenance, and old/new content identities.
2. A semantic index compares Cargo structural data and exact effective `rail.toml` fields. Formatting-only and
   metadata-only manifest edits remain no-ops where Cargo semantics permit.
3. Cargo target membership and normal, build, and development dependency domains determine portable package and
   target selectors.
4. Code-owned Cargo semantics and repository-owned positive input declarations evaluate independently.
5. Compatible observed-input evidence can prove that a Cargo work item is disjoint. Missing or incomplete evidence
   requires only that item with an explicit fallback.

A cold plan does not pretend that unobserved compiler, build-script, or proc-macro reads are absent. It requires the
bounded Cargo work whose evidence is incomplete while still skipping declared-disjoint compatibility, archive,
Surface, and repository work.

## Register repository work

Pure Cargo workspaces require no planning configuration. Cargo-Rail registers `cargo.build`, `cargo.clippy`,
`cargo.doc`, `cargo.doctest`, `cargo.fmt`, `cargo.package`, `cargo.test`, `dependency-policy`, `release.semver`, and
`surface`.

Repository commands remain in Just, scripts, or CI. Register only their positive inputs:

```toml
[plan.work.workflow-policy]
scope = "repository"
paths = [".github/**", "scripts/ci/**"]
config = ["targets"]

[plan.work.compatibility]
scope = "variants"
paths = ["tests/compatibility/**", ".github/workflows/compatibility.yaml"]
cargo = ["cargo.build", "cargo.test"]
variant_catalog = "distribution/compatibility-plan-variants.json"
```

Work IDs contain lowercase ASCII letters, digits, dots, and hyphens. Path inputs are positive repository-relative
globs: absolute paths, parent traversal, and negative patterns are rejected. `config` names exact schema-owned fields;
`cargo` subscribes repository work to code-owned Cargo decisions. Declarations cannot contain commands.

Repository work uses one of three scopes:

- `repository` carries no executable selector;
- `cargo` carries a typed Cargo selection; and
- `variants` carries selected catalog rows or an explicit `all` fallback when variant evidence is incomplete.

When Cargo-scoped work subscribes to a changed built-in Cargo decision, it inherits that decision's exact package and
target selection. A matching path adds its package selection to that inherited scope. A changed configuration input
widens the declared work to the Cargo workspace because configuration can affect every package.

Catalogs conform to [`plan-variants-v1.schema.json`](../schemas/plan-variants-v1.schema.json). Catalog dimensions are
data consumed by the owning workflow, not command syntax.

### Choose scope from command capability

The planner decides whether work is required. The command still decides how it executes.

| Command shape | Declaration | Consumption |
|---|---|---|
| Cargo, nextest, or Miri accepts package selectors | `scope = "cargo"`, subscribe to `cargo.test` or another built-in | Pass the typed Cargo argv to the command |
| Kani or another whole-repository verifier | `scope = "repository"`, subscribe to the closest Cargo work | Use the decision as a gate and run the verifier unchanged |
| Just, Make, `xtask`, or a script | Choose scope from the underlying command, not the task runner | Pass selectors only when the recipe accepts them |
| Docker build | Usually `scope = "repository"` with Docker paths and a `cargo.build` subscription | Gate the build; host cache setup does not enter the container |
| Benchmark or profiling run | `cargo` when the harness accepts package scope; otherwise `repository` | Scope the run, not its result data |
| Toolchain, target, feature, or runner matrix | `scope = "variants"` with a checked-in catalog | Execute only selected catalog rows |

For example:

```toml
[plan.work.miri]
scope = "cargo"
cargo = ["cargo.test"]
paths = ["scripts/miri/**"]

[plan.work.kani]
scope = "repository"
cargo = ["cargo.test"]
paths = ["kani/**"]

[plan.work.benchmarks]
scope = "cargo"
cargo = ["cargo.test"]
paths = ["benches/**", "scripts/bench/**"]
```

Markdown and arbitrary TOML files trigger only named work whose `paths` match them. Cargo manifests, Cargo
configuration, toolchain files, and `rail.toml` fields also use Cargo-Rail's semantic rules. Formatting-only manifest
edits can remain disjoint. `cargo.package` conservatively owns every file in a package source tree.

The retired `[change-detection]` path categories and global confidence policy are rejected by current configuration
loading. Run `cargo rail config migrate --check` against an older configuration, translate still-owned positive globs
to named work, then apply the migration. Historical comparisons normalize the retired table in memory because it has
no v8 authority; they do not make it valid in the current worktree.

## Consume the machine contract

`cargo rail plan --json` writes exactly one schema-owned JSON value to stdout. The current contract is v8:

```bash
cargo rail plan --schema > plan.schema.json
cargo rail plan --json > plan.json
```

[`plan-v8.schema.json`](../schemas/plan-v8.schema.json) is the checked-in schema. `plan --schema` runs before workspace
capture, so consumers can inspect compatibility without opening a repository.

Important fields are:

- `identity`: deterministic, root-independent `plan-v8:sha256:...` decision identity;
- `inputs`: resolved base, requested head, captured head commit, source capture, Cargo/toolchain/platform bindings,
  catalog identity, evidence identities, and override;
- `changes`: typed file, Cargo structural, and configuration deltas;
- `work`: every registered work decision;
- `required`: the sorted projection of required work IDs; and
- `evidence`: canonical evidence records referenced by decisions.

Decisions are tagged. A skipped decision has `state` and evidence but cannot carry scope. A required decision also has
`cause` and `scope`. The cause is `changed_input`, `incomplete_evidence`, or `forced_all`.

For Cargo work, read `scope.selection.cargo_args` as an argument array. Never shell-join it or derive selectors from
explanation fields. `scope.selection.targets` identifies exact current Cargo targets where that boundary is known.
For variant work, consume `scope.selection.variants` exactly; `kind = "all"` means the owning workflow must materialize
its complete checked-in catalog.

This repository's `scripts/plan/read.py` is the strict reference consumer. It recomputes content-derived evidence and
plan identities, validates the required-work projection and typed-selector lowering, and rejects unknown or malformed
contracts before it emits NUL-delimited Cargo/target arguments or workflow matrices.

## Portable observed-input evidence

Pass compatible evidence explicitly:

```bash
cargo rail plan --evidence planning-evidence.json --json
```

[`planning-evidence-v1.schema.json`](../schemas/planning-evidence-v1.schema.json) binds evidence to its content-derived
identity, base source, Cargo dependency universe and configuration, toolchain, target, platform, provider capabilities,
and work kind. Its source-bound portable base model contains package roots, target roots, and conservative dependency
domains; this can bound removed-member impact without claiming historical Cargo resolution. Observed inputs carry
Git-object or content identities. Environment evidence contains names, never values, and secret-like names invalidate
the manifest rather than entering planner output.

Evidence can prove a skip only when the relevant compiler, rustdoc, build-script, proc-macro, and process-domain
capabilities are complete and carry no bypasses. Missing, malformed, stale, cross-platform, or incomplete evidence
widens only its owning Cargo work. A plan identity compares decisions; it is not a cache key and never authorizes
result reuse.

The current compiler observer explicitly reports build-script and proc-macro runtime-read bypasses, so it cannot
produce production evidence that proves those input classes complete. Cold production planning therefore retains the
bounded Cargo fallback. Complete synthetic fixtures qualify planner behavior without overstating provider capability.

## Process and machine boundaries

Transfer the exact plan file, not reconstructed booleans. Before starting each consuming process, a consumer must:

1. validate contract v8 and its required-work projection;
2. lower only the typed scope belonging to the selected work ID;
3. reject unknown work, variants, or selector shapes; and
4. run `scripts/plan/read.py verify-checkout PLAN` from the execution workspace immediately before the executor, then
   start the executor only when verification succeeds.

`verify-checkout` first validates the complete v8 contract and canonical plan identity. It then delegates to the
matching Cargo-Rail binary through `cargo rail plan --verify` to bind the plan to the current source: the exact head
commit plus either the worktree capture or a clean object-bound checkout. Comparing `HEAD` alone does not authorize
execution. Drift fails with exit code `2`; verification emits no selectors and performs no mutation.

Cargo, configuration, toolchain, target, and platform identities record the environment that produced the plan. A
consumer does not recompute those planner-owned facts. This preserves one decision across Linux, macOS, and Windows
instead of allowing each execution host to create a different authority.

The Commit workflow uploads one plan artifact. Quality, compatibility, archive, and Surface jobs download that same
artifact, validate its contract, and verify its checkout binding immediately before each executor. Reusable workflows
receive planner-selected matrices; missing variant evidence is represented as explicit `all`, never an inferred empty
matrix.

Compiler reuse remains independent of planning. Run `cargo rail cache setup` on an execution machine when verified L1
reuse is desired; planning never treats a cache hit as evidence that work is unnecessary.

## Diagnose

```bash
cargo rail config validate --strict
cargo rail config migrate --check
cargo rail plan --explain
cargo rail plan --json | jq '{required, work}'
```

See [Troubleshooting](troubleshooting.md) when a named work decision, package selector, evidence boundary, or variant
selection differs from expectation.
