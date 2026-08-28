# Troubleshooting

Start with Cargo-Rail's decision or status output. It records the failed boundary and the safe next command.

## A plan selected the wrong work

```bash
cargo rail plan --explain
cargo rail plan --json | jq '{inputs, changes, required, work, evidence}'
```

Check in this order:

1. `inputs.base`, `inputs.head`, and `inputs.head_commit` identify the intended comparison.
2. `changes` contains the expected file, Cargo, and configuration deltas.
3. `work.NAME.state`, `.cause`, and `.evidence` explain why the item is required or skipped.
4. `work.NAME.scope` contains the package, target, or variant selector used by the executor.

Explanations and evidence are not execution input. If the plan is correct but execution is wrong, inspect the consumer
that lowered the typed scope. Transfer one exact plan across CI jobs; independently re-planned work can differ when
the base, source, Cargo universe, toolchain, catalog, platform, or evidence differs.

See [Planning](planning.md) for the consumer contract.

## Configuration differs from expectation

```bash
cargo rail config locate
cargo rail config explain --all -f json
cargo rail config validate --strict
```

`locate` identifies the discovered file. `explain` shows defaults, effective values, and sources. A global `--config`
option bypasses discovery.

## Surface is unavailable or reports unexpected findings

Source installs, `cargo binstall`, and musl archives cannot run Surface analysis; `surface --schema` remains
available. Install the complete native component set, then prepare the exact workspace-selected toolchain:

```bash
cargo rail surface --prepare -f json
```

Preparation may install `rustc-dev` for the selected rustup toolchain. It does not change the default toolchain.

For an unexpected gate or finding, inspect planning, policy, readiness, and report authority separately:

```bash
cargo rail plan --json | jq '.work.surface'
cargo rail config explain surface -f json
cargo rail surface --prepare -f json
cargo rail surface --check -f json
```

Before changing policy, verify the report's audited and open targets, products, target views, feature views, and
completeness. Any open compiler-crate observation preserves the declaration. Use
`consumer_scope = "workspace"` only when the workspace contains every consumer of its private compiler crates.

If one compiler view fails, correct the source failure and run the exact `surface --resume` command printed by the
error. The acquisition journal identifies completed and pending views but never authorizes cached facts by itself.

## Compiler reuse did not happen

```bash
cargo rail cache setup --check
cargo rail cache status --scope local --format json
cargo rail doctor native-cache --format json
```

A missing compiler identity, incremental compilation, unsupported compiler class, cross target, custom target
directory, conflicting wrapper, different physical source root, or incomplete input evidence executes the original
compiler. A bypass is safe fallback, not a failed hit.

Disable an installed wrapper for one process tree without changing setup:

```bash
CARGO_RAIL_CACHE=off cargo check --locked
```

`cargo rail unify --check -f json` reports its separate compiler-evidence cache. That cache never restores Cargo build
artifacts.

Preview cleanup before removing state:

```bash
cargo rail cache clean --scope workspace --check
cargo rail cache clean --scope local --check
cargo rail cache remove --check
```

Workspace cleanup removes reconstructible state for the current checkout. Local cleanup removes the receipt-selected
shared CAS and requires `cargo rail cache setup` to repair it. `cache remove` removes owned setup but preserves CAS
data. Resolve receipt, wrapper, or ownership drift instead of deleting cache files or Cargo configuration by hand.

See [Caching](caching.md) for exact eligibility and support.

## A release stopped

`release run` and `release finalize` persist a journal before their first side effect. Inspect it and run the exact
recovery command Cargo-Rail reports:

```bash
cargo rail release status --format json
cargo rail release resume target/cargo-rail/releases/release-<id>.json
```

`resume` reconciles Git, readiness checks, registry versions, tags, and forge state before advancing. It does not
replan from mutated manifests. Waiting at an exact-SHA readiness or registry-convergence boundary is normal.

Abort only while the status says no external side effect may exist:

```bash
cargo rail release abort target/cargo-rail/releases/release-<id>.json --yes
```

After that boundary, resume and reconcile. Do not move a published tag or replace a release asset.

Commit-driven releases need complete tag history. Use `fetch-depth: 0` in GitHub Actions or fetch full history and
tags before checking the release. Reviewed-change mode does not use commit history to choose bumps.

## Sync left conflicts

A manual conflict exits `1` and writes a receipt. Resolve every listed file and remove conflict markers, then let
Cargo-Rail validate and commit the result:

```bash
cargo rail sync --resume target/cargo-rail/receipts/sync-conflict-<crate>-<id>.json
```

Do not commit the conflict manually. Resume verifies the expected branch, parent, owned paths, and files.

## Exit codes

- `0`: success or a clean check.
- `1`: required changes found, or a resumable sync conflict.
- `2`: invalid arguments or an operational failure.

An executed subprocess may deliberately propagate another status.
