# Troubleshooting

Cargo-Rail records why it selected, skipped, widened, or refused work. Start at the boundary that made the decision;
do not infer it from the final Cargo command.

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

Use `scope` for combined execution and `surfaces.NAME.scope` for one surface. `impact` is explanation, not an execution
handoff. Historical `--from/--to` plans widen package work because Cargo cannot resolve an unchecked-out tree.

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

If analysis reports that Surface is unavailable, the CLI came from a source installation, `cargo binstall`, or an
unsupported native archive. `surface --schema` remains available, but analysis requires a supported release archive
with `cargo-rail-fact-driver` beside the CLI. Use the checksum-verifying installer documented in
[Installation](../README.md#installation).

For an unexpected gate or finding, inspect the planner, effective policy, and report authority separately:

```bash
cargo rail plan --merge-base -f json | jq '.surfaces.surface'
cargo rail config explain -f json | jq '.fields[] | select(.path | startswith("surface."))'
cargo rail surface --check -f json
```

In the report, verify `authority.audited_targets`, `authority.open_targets`, `products`, `targets`, `features`, and
`completeness.complete` before changing policy. A declaration observed in any open compiler crate is preserved. Use
`consumer_scope = "workspace"` only when the workspace contains every consumer of its non-publishable internal
compiler crates.

Warm analysis exposes `metrics.acquisition`: an unchanged exact hit has zero Cargo views and compiler invocations.
A miss or bypass names its reason and runs the required compiler acquisition instead of trusting incomplete facts.
Plain `cargo rail surface` reports findings with exit 0; `--check` uses exit 1 for deny findings or configuration
diagnostics, and operational failures use exit 2.

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

Clearly unsupported shapes execute before session, ledger, or CAS acquisition, so early-bypass totals are not a census
of rustc. Failure to capture the exact compiler identity, incremental compilation, an ambiguous wrapper, a nonstandard
target layout, an unsupported compiler class, or incomplete observed input executes normally; it is not a false cache
hit. A different physical source root has different action authority and compiles its own exact variant.

If an installed wrapper must be disabled for one process tree, set `CARGO_RAIL_CACHE=off`. The minimal launcher
executes the selected compiler chain without starting the cache worker or reading installation context, session state,
or CAS data and preserves the environment control for the compiler.

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

Setup refuses persistent environment/workspace shadowing and unowned global wrappers. Removal refuses a changed Cargo
field, launcher, worker, or receipt; resolve the ownership drift instead of deleting configuration by hand.

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

Readiness and registry propagation are explicit wait boundaries: Cargo-Rail exits instead of polling. Resume after the
provider settles. GitHub readiness requires at least one successful context, complete reported counts, and no pending
or failed context.

Direct release commit trailers retain the transaction and effect authorization. From the exact release commit in a
fresh checkout, `release status` can report reconstructable state and
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
