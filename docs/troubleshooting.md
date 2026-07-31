# Troubleshooting

Cargo-Rail records why it selected, skipped, widened, or refused work. Start at the boundary that made the decision;
do not infer it from the final Cargo command.

## Planning selected the wrong work

```bash
cargo rail plan --merge-base --explain
cargo rail plan --merge-base -f json
cargo rail run --merge-base --dry-run --print-cmd --explain
```

Read these fields in order:

1. `inputs.refs` — did the plan compare the intended range?
2. `files` — did it capture and classify the expected paths?
3. `trace` — which ownership, semantic, graph, confidence, or fallback reason fired?
4. `surfaces.NAME` — was the relevant surface enabled, and with which reason IDs?
5. `surfaces.NAME.scope` — which packages belong to that surface?
6. the run preview — which profile actions, feature/target view, and argv were expanded?

Use `scope` for combined execution and `surfaces.NAME.scope` for one surface. `impact` is explanation, not an execution
handoff. Historical `--from/--to` plans widen package work because Cargo cannot resolve an unchecked-out tree.

If `plan` is right but `run` is wrong, check the selected action/profile, configured `when` policies, trailing Cargo
arguments, and `--ignore-bin-crates`. Planner surfaces such as `infra` and `custom:*` are conditions, not executable
action IDs. See [Planning and execution](planning.md).

## Configuration is not what you expected

```bash
cargo rail config locate
cargo rail config explain -f json
cargo rail config validate --strict
```

`locate` shows the discovered file. `explain` shows effective values and their sources. A CLI `--config` override
bypasses the normal search order.

## Cache reuse did not happen

Use the doctor for the cache boundary you are testing:

```bash
# Expanded action-key eligibility
cargo rail doctor hermeticity --action build --format json

# Exact native compiler-cache capability certificate
cargo rail doctor native-cache --format json

# Compiler evidence used by unify
cargo rail unify --check -f json
```

For native reuse, inspect the run summary's hits, misses, bypasses, hashed/restored bytes, and stable reasons. A missing
certificate, incremental compilation, an existing wrapper, an unsupported compiler class, or incomplete observed input
executes cold; it is not a failed build.

For the hermetic whole-action profile, read the report under `target/cargo-rail/hermetic/reports/`. `fetch.reused`
describes only the dependency inventory; it does not mean the action result was restored. `platform_limited` means the
isolated check ran without an authorizing cache key.

For `unify`, `evidence_cache` reports diagnostic-observation reuse. That cache never restores Cargo build artifacts.

The full eligibility matrix, result meanings, storage paths, and current measurements are in [Caching](caching.md).
Preview validated cleanup before removing cache state:

```bash
cargo rail clean --cache --check
cargo rail clean --cache
```

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

Release commit trailers retain the transaction and effect authorization. From the exact release commit in a fresh
checkout, `release status` can report reconstructable state and
`cargo rail release resume release-<transaction-id>` can rebuild a missing local journal.

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
