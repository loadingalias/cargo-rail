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
cargo rail config explain --all --json
cargo rail config validate --strict
```

`locate` identifies the discovered file. `explain` shows defaults, effective values, and sources. A global `--config`
option bypasses discovery.

## Surface is unavailable or reports unexpected findings

Source installs and `cargo binstall` cannot run Surface analysis; `surface --schema` remains available. Native
archives—including both Linux musl archives—install the complete component set. Prepare the exact workspace-selected
toolchain with:

```bash
cargo rail surface --prepare --json
```

Linux musl Surface also requires the host's standard musl `libgcc_s` runtime, matching rustc's native host-tools
dependency.

Preparation may install `rustc-dev` for the selected rustup toolchain. It does not change the default toolchain.

For an unexpected gate or finding, inspect planning, policy, readiness, and report authority separately:

```bash
cargo rail plan --json | jq '.work.surface'
cargo rail config explain surface --json
cargo rail surface --prepare --json
cargo rail surface --check --json
```

Before changing policy, verify the report's audited and open targets, products, target views, feature views, and
completeness. Any open compiler-crate observation preserves the declaration. Use
`consumer_scope = "workspace"` only when the workspace contains every consumer of its private compiler crates.

If one compiler view fails, correct the source failure and run the exact `surface --resume` command printed by the
error. The acquisition journal identifies completed and pending views but never authorizes cached facts by itself.

## Compiler reuse did not happen

```bash
cargo rail cache setup --check
cargo rail cache probe --json
cargo rail cache status --scope local --json
cargo rail doctor native-cache --json
```

A missing compiler identity, incremental compilation, unsupported compiler class, cross target, conflicting wrapper,
or incomplete input evidence executes the original compiler. Physical-root mode also binds the canonical checkout;
use `--root-portability remap` only for certified cross-root Rust metadata and library results. External
`CARGO_TARGET_DIR` locations are supported for eligible native results. A bypass is safe fallback, not a failed hit.

`native_cache_hardware_qualification_unavailable` means the operating system is recognized but its architecture is
not in the [native host table](caching.md#native-host-eligibility).
`native_cache_platform_qualification_unavailable` means the operating system is unsupported.
`cross_target_toolchain_evidence_unavailable` means Cargo selected a target other than rustc's host target; run that
build natively to make it eligible for compiler-result reuse.

Status schema 15 keeps native failure-reason counters outside the bounded 65,536-event usage ledger. Inspect
`usage.failure_reason_counts_available` before interpreting `usage.failure_reasons`; event eviction does not erase
those counters. A strict `cache probe` verifies authenticated provider and protocol readiness without exposing the
remote URL, object names, credentials, or local paths.

Disable an installed wrapper for one process tree without changing setup:

```bash
CARGO_RAIL_CACHE=off cargo check --locked
```

`cargo rail unify --check --json` reports its separate compiler-evidence cache. That cache never restores Cargo build
artifacts.

Preview cleanup before removing state:

```bash
cargo rail cache clean --scope workspace --check
cargo rail cache clean --scope local --check
cargo rail cache detach --check
cargo rail cache profiles --json
cargo rail cache uninstall --check
```

Workspace cleanup removes reconstructible state for the current checkout. Local cleanup removes the current profile's
CAS and requires `cargo rail cache setup` to repair it. `cache detach` preserves the profile and CAS while removing the
current root binding. `cache uninstall` removes the global wrapper and Cargo configuration while preserving every
profile. Use `cache profiles`, `cache drop-profile --profile PROFILE_ID --check`, and `cache drop-unbound --check` for
explicit machine-wide cleanup. Resolve receipt, wrapper, profile, or ownership drift instead of deleting cache files
or Cargo configuration by hand.

See [Caching](caching.md) for exact eligibility and support.

## A release stopped

`release run` and `release finalize` persist a journal before their first side effect. Inspect it and run the exact
recovery command Cargo-Rail reports:

```bash
cargo rail release status --json
cargo rail release resume target/cargo-rail/releases/release-<id>.json
```

`resume` reconciles Git, readiness checks, registry versions, tags, and forge state before advancing. It does not
replan from mutated manifests. Use `release run --wait` when the initiating process should stay attached until
exact-SHA checks settle; an interrupted wait remains resumable from the same journal.

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
