# Troubleshooting

Cargo-Rail records why it selected, skipped, widened, or refused work. Start with that decision, not the final Cargo
command.

## Planning selected the wrong work

```bash
cargo rail plan --merge-base --explain
cargo rail plan --merge-base -f json
```

Read these fields in order:

1. `inputs.refs` — did the plan compare the intended range?
2. `files` — did it capture and classify the expected paths?
3. `trace` — which ownership, semantic, graph, confidence, or fallback reason fired?
4. `surfaces.NAME` — was the relevant surface enabled, and with which reason IDs?
5. `surfaces.NAME.scope` — which packages belong to that surface?

Use `scope` for combined execution and `surfaces.NAME.scope` for one surface. `impact` explains the decision; it is not
execution scope. Historical `--from/--to` plans widen package work because Cargo cannot resolve an unchecked-out tree.

If the plan is right but execution is wrong, inspect the Cargo or nextest command that consumed
`surfaces.NAME.scope.cargo_args`. Command-specific flags belong to that tool. See [Planning](planning.md).

## Configuration is not what you expected

```bash
cargo rail config locate
cargo rail config explain -f json
cargo rail config validate --strict
```

`locate` shows the discovered file. `explain` shows effective values and their sources. A CLI `--config` override
bypasses the normal search order.

## Surface analysis is unavailable or unexpected

If Surface is unavailable, the CLI came from a core-only source installation, `cargo binstall`, or the musl archive.
`surface --schema` remains available. Use the complete native install in
[Installation](../README.md#installation) to install authenticated Surface components.

Prepare the exact workspace-selected toolchain before analysis:

```bash
cargo rail surface --prepare -f json
```

When compiler development metadata is absent, the preflight installs the selected rustup toolchain's `rustc-dev`
component. It resolves either the exact prebuilt producer or a deterministic driver built from authenticated offline
source, stages and reauthenticates its bytes, and emits readiness contract v1. It never changes the user default
toolchain. An operational failure names the missing toolchain/component or failed authority boundary and exits 2
without starting analysis.

For an unexpected gate or finding, inspect the planner, effective policy, and report authority separately:

```bash
cargo rail plan --merge-base -f json | jq '.surfaces.surface'
cargo rail config explain -f json | jq '.fields[] | select(.path | startswith("surface."))'
cargo rail surface --prepare -f json
cargo rail surface --check -f json
```

In the report, verify `authority.audited_targets`, `authority.open_targets`, `products`, `targets`, `features`, and
`completeness.complete` before changing policy. A declaration observed in any open compiler crate is preserved. Use
`consumer_scope = "workspace"` only when the workspace contains every consumer of its non-publishable internal
compiler crates.

Warm analysis reports `metrics.acquisition`. An unchanged exact hit has zero Cargo views and compiler invocations. A
miss or bypass names its reason and acquires new compiler evidence. Plain `cargo rail surface` reports findings with
exit 0. `--check` exits 1 for deny findings or configuration diagnostics. Operational failures exit 2.

When one of many compiler views fails, use the exact `surface --resume` command in the operational error. Inspect the
JSONL manifest under `target/cargo-rail/surface-acquisitions-v1/` for the failed package, Cargo target, target triple,
feature profile, and remaining views. Correct the source failure before resuming. Cargo-Rail recaptures current state
and trusts the fact CAS, not the journal's prior completion labels, so interrupted or edited manifests cannot authorize
stale compiler evidence.

## Cache reuse did not happen

Use the doctor for the cache boundary you are testing:

```bash
# Exact native compiler-cache identity
cargo rail doctor native-cache --format json

# Compiler evidence used by unify
cargo rail unify --check -f json
```

For native reuse, inspect installation health and the bounded usage ledger:

```bash
cargo rail cache setup --check
cargo rail cache status --scope local --format json
cargo rail doctor native-cache --format json
```

Unsupported shapes execute before session, ledger, or CAS acquisition, so early-bypass totals do not count every rustc
invocation. Missing compiler identity, incremental compilation, an ambiguous wrapper, a nonstandard target layout, an
unsupported compiler class, or incomplete input evidence executes normally. A different physical source root has
different action authority and compiles its own variant.

Set `CARGO_RAIL_CACHE=off` to disable an installed wrapper for one process tree. The launcher executes the selected
compiler chain without starting the cache worker or reading installation context, session state, or CAS data. It
preserves the environment control for the compiler.

For `unify`, `evidence_cache` reports diagnostic-observation reuse. That cache never restores Cargo build artifacts.

The full eligibility matrix, result meanings, storage paths, and current measurements are in [Caching](caching.md).
Inspect the affected scope, then preview validated cleanup before removing cache state:

```bash
cargo rail cache status --scope workspace
cargo rail cache clean --scope workspace --check
cargo rail cache clean --scope workspace

cargo rail cache status --scope local
cargo rail cache clean --scope local --check
cargo rail cache clean --scope local
cargo rail cache setup                    # repair the selected empty CAS

cargo rail cache remove --check
cargo rail cache remove                   # preserve CAS data
```

Setup refuses persistent environment or workspace shadowing and unowned global wrappers. Removal refuses a changed
Cargo field, launcher, worker, or receipt. Resolve the ownership drift instead of deleting configuration by hand.

## Release stopped

`release run` and `release finalize` persist a journal before their first side effect. Inspect it, then use the safe
command Cargo-Rail reports:

```bash
cargo rail release status
cargo rail release status --format json
cargo rail release resume target/cargo-rail/releases/release-<id>.json
```

`status` does not load Cargo metadata. It reports the phase, exact SHA, observed and next effects, ambiguity,
recoverability, and a safe command. `resume` reconciles local Git, remote refs, readiness checks, registry versions,
tags, and forge state before advancing. It does not replan from already-mutated manifests.

Cargo-Rail exits at readiness and registry-propagation boundaries. Resume after the provider settles. GitHub readiness
requires at least one successful context, complete reported counts, and no pending or failed context.

Direct release commit trailers retain transaction and effect authorization. From the exact release commit in a fresh
checkout, `release status` can report reconstructable state and
`cargo rail release resume release-<transaction-id>` can rebuild a missing local journal. PR preparation trailers
retain the transaction plan, but finalization does not rewrite the merged commit. Its journal remains the final-effect
authority until tags, publication, and forge state are reconciled.

Abort only while no external side effect may exist:

```bash
cargo rail release abort target/cargo-rail/releases/release-<id>.json --yes
```

After that boundary, resume and reconcile. Plain `cargo rail clean` removes completed or aborted journals but refuses
active, malformed, or ambiguous release state.

### Shallow history with commit-derived releases

`[release] source = "commits"` or `"both"` needs release tags for each crate's history range. Fetch complete history
for release jobs:

```bash
git fetch --unshallow --tags
```

GitHub Actions release checkouts should use `fetch-depth: 0`. The default reviewed-change source does not use commit
history to choose bumps.

### Finalizing a release PR

`release finalize` expects the version and changelog mutations from `release run --pr` in the current checkout. Merge
the PR, update its target branch, then finalize that merged commit.

## Sync left conflicts

A manual conflict exits `1`, leaves the merge on `cargo-rail-sync-<crate>`, and writes a conflict receipt. Resolve every
listed file and remove all conflict markers, then let Cargo-Rail validate and commit the resolution:

```bash
cargo rail sync --resume target/cargo-rail/receipts/sync-conflict-<crate>-<id>.json
```

Do not commit the conflict manually. Resume verifies the branch, parent, owned paths, and marker-free files.

## CI and local plans differ

Use the same base ref and confidence policy in both environments. `--merge-base` and `--since` are not interchangeable
unless they resolve to the same commit. Compare `inputs.refs.resolved_base`, `inputs.confidence_profile`, and
`inputs.config_fingerprint` in JSON output before inspecting package impact.

## Exit codes

- `0`: success or a clean check
- `1`: a check found required changes, or a resumable sync conflict
- `2`: an argument or operational failure

Executed subprocesses may deliberately propagate another status.
