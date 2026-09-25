# Troubleshooting

Start with Cargo-Rail's decision or status output.
It records the failed boundary and the safe next command.

## A plan selected the wrong work

```bash
cargo rail plan --explain
cargo rail plan --json | jq '{inputs, changes, required, work, evidence}'
```

Check in this order:

1. `inputs.base`, `inputs.head`, and `inputs.head_commit` identify the intended comparison.
1. `changes` contains the expected file, Cargo, and configuration deltas.
1. `work.NAME.state` says whether the item is required or skipped.
   Its `.evidence` entries refer to the top-level `evidence` map; required items also have a `.cause`.
1. Required items have `work.NAME.scope`, which contains the package, target,
   or variant selector used by the executor.

Explanations and evidence are not execution input.
If the plan is correct but execution is wrong, inspect the consumer that lowered the typed scope.
Transfer one exact plan across CI jobs; independently re-planned work can differ when the base,
source, Cargo universe, toolchain, catalog, platform, or evidence differs.

See [Planning](planning.md) for the consumer contract.

## Configuration differs from expectation

```bash
cargo rail config locate
cargo rail config explain --all --json
cargo rail config validate --strict
```

`locate` identifies the discovered file.
`explain` shows defaults, effective values, and sources.
A global `--config` option bypasses discovery.

## Cargo metadata fails

Every Cargo metadata failure names one cause, one recovery,
and a `reproduce:` command that repeats the exact Cargo invocation, including `--locked`.
Named causes are a stale `Cargo.lock`, an unloadable manifest, an unavailable target or `rustc`,
and a missing Cargo executable.

When a Cargo credential capability is active, such as a registry token or credential provider,
Cargo's output is withheld because it can contain provider output.
Manifest failures are the exception: Cargo reports them before it contacts a registry.
Run the reproduction command to see Cargo's complete output.

A JSON error names its class in `failure_class`: `lockfile`, `manifest`, `toolchain`, or `cargo`.

## Unify compiler evidence fails

Unify compiles the workspace to prove which dependencies are unused.
When one compiler-evidence view fails, Unify stops, exits `2`, and reports one cause,
one recovery section, and a `reproduce with Cargo:` command.
That command runs the view's Cargo command in the workspace root,
outside Cargo-Rail's private target directory.

| `failure_class` | Cause | Recovery |
| --- | --- | --- |
| `build_script` | A package's build script failed | Provide the tools, files, or environment it needs |
| `source` | Rust source failed to compile | Fix the compiler errors shown under the cause |
| `toolchain` | A target library, linker, `rustc`, or `RUSTC_WRAPPER` is missing or cannot run | Install or correct it, or narrow `unify.compiler_targets` |
| `cargo_rail` | Cargo-Rail's compiler adapter failed | Report the `--verbose` output as a Cargo-Rail defect |
| `cargo` | Any other Cargo failure | Run the reproduction command |
| `interrupted` | SIGINT or SIGTERM stopped compiler acquisition | Rerun the same command |

A build-script failure lists the environment variable names that the script declares with `rerun-if-env-changed`.
Unify never prints their values.
Text mode shows the last lines of the build script's own stderr under the cause.
Any exact value of an inherited environment variable in that text appears as `<env:NAME>`.
Cargo's complete output appears only with `--verbose`, and never when a Cargo credential capability is active.
When a build script reports a missing native tool through the `cc`, `cmake`, or `pkg-config` crates,
or source reads a file that does not exist, the cause names that tool or file,
redacted the same way.
JSON errors contain no other Cargo or build-script output.

Setting `unify.compiler_targets = "none"` runs Unify without compiler evidence.
Unify then keeps every dependency whose use it cannot prove.

Unify writes progress to stderr, including with `--format json`; stdout stays one JSON value.
When a phase prints nothing for 30 seconds,
a `Still running:` line names the phase and whether Cargo-Rail is analyzing, waiting for a Cargo subprocess,
or waiting for Cargo's file lock.
A Cargo file-lock wait is reported as soon as Cargo reports it.
`--quiet` suppresses progress.

A repository wrapper should pass stderr through as it arrives and capture only stdout:

```bash
cargo rail unify --check --format json > target/unify.json
```

Progress is plain lines on stderr, written as each event happens, in every output format.
A wrapper relays it unchanged without parsing it.
Capturing both streams until the command exits, as `$(cargo rail unify --check 2>&1)` does, hides all progress for the whole run.
`--diagnostics-file` records each completed phase in `progress_phases`, with its activity, whether Cargo waited for a file lock,
and its duration.

An interrupted acquisition stops its Cargo process tree and names the phase it interrupted.
Compiler acquisition writes only to its private target directory, so no workspace file has changed.
Outside compiler acquisition, SIGINT and SIGTERM end the process immediately;
a second signal always does.

## Surface is unavailable or reports unexpected findings

A `cargo install` or single-binary `cargo binstall` installation lacks the embedded Surface component set.
`cargo rail surface --schema` remains available without it.
Install and extract a supported native ZIP with all its components kept together,
or select an independently authenticated [compiler adapter pack](caching.md#select-an-independent-compiler-adapter).
Prepare the exact workspace-selected toolchain with:

```bash
cargo rail surface --prepare --json
```

Preparation may install `rustc-dev` for the selected rustup toolchain.
It does not change the default toolchain.

For an unexpected gate or finding, inspect planning, policy, readiness,
and report authority separately:

```bash
cargo rail plan --json | jq '.work.surface'
cargo rail config explain surface.enabled surface.targets surface.consumer_scope --json
cargo rail surface --prepare --json
cargo rail surface --check --json
```

Before changing policy, verify the report's audited and open targets, products, target views,
feature views, and completeness.
Any open compiler-crate observation preserves the declaration.
Use `consumer_scope = "workspace"` only when the workspace contains every consumer of its private compiler crates.
Prove that boundary with reviewed inventories of published APIs, downstream repositories, plugins,
generated code, and build-script or proc-macro consumers.
Cargo metadata from this checkout cannot prove that external set is empty.

If one compiler view fails, correct the source failure
and run the exact `surface --resume` command printed by the error.
The acquisition journal identifies completed and pending views
but never authorizes cached facts by itself.

## Compiler reuse did not happen

```bash
cargo rail cache setup --check
cargo rail cache probe --json
cargo rail cache status --scope local --json
cargo rail doctor native-cache --json
```

A missing compiler identity, incremental compilation, unsupported compiler class,
conflicting wrapper, or incomplete input evidence executes the original compiler.
An installation without embedded component authority also needs an independently authenticated
[compiler adapter pack](caching.md#select-an-independent-compiler-adapter).
Pack authentication, compilation, or calibration failure safely bypasses native reuse;
Surface fails when it requires those compiler facts.
Physical-root mode also binds the canonical checkout;
use `--root-portability remap` only for certified cross-root Rust metadata and library results.
External `CARGO_TARGET_DIR` locations are supported for eligible native results.
A bypass is safe fallback, not a failed hit.

`native_cache_hardware_qualification_unavailable` means the operating system is recognized
but its architecture is not in the [native host table](caching.md#native-host-eligibility).
`native_cache_platform_qualification_unavailable` means the operating system is unsupported.
An explicit target is not an automatic bypass.
Reuse requires the selected target's compiler distribution, target definition, sysroot,
and target-library evidence; linked outputs also require complete linker evidence.
Inspect the reported missing boundary before changing the build target.

Status schema 18 keeps native failure-reason counters outside the bounded 65,536-event usage ledger.
Inspect `status.installation.usage.failure_reason_counts_available` before interpreting `status.installation.usage.failure_reasons`.
The ledger stops accepting events when full; the separate failure counters continue to advance.
A strict `cache probe` verifies authenticated provider and protocol readiness without exposing the remote URL,
object names, credentials, or local paths.

Disable an installed wrapper for one process tree without changing setup:

```bash
CARGO_RAIL_CACHE=off cargo check --locked
```

`cargo rail unify --check --json` reports its separate compiler-evidence cache.
That cache never restores Cargo build artifacts.

Preview cleanup before removing state:

```bash
cargo rail cache clean --scope workspace --check
cargo rail cache clean --scope local --check
cargo rail cache detach --check
cargo rail cache profiles --json
cargo rail cache uninstall --check
```

Workspace cleanup removes reconstructible state for the current checkout.
Local cleanup removes the current profile's CAS and requires `cargo rail cache setup` to repair it.
`cache detach` preserves the profile and CAS while removing the current root binding.
`cache uninstall` removes the global wrapper and Cargo configuration while preserving every profile.
Use `cache profiles` and `cache drop-profile --profile PROFILE_ID --check` for explicit machine-wide cleanup.
Resolve receipt, wrapper, profile,
or ownership drift instead of deleting cache files or Cargo configuration by hand.

If setup reports an unsupported installation receipt version,
preserve the installation and the executable that created it.
Use that executable to preview and perform removal, then run the current `cargo rail cache setup`.
The receipt error cannot identify the originating release version;
do not substitute manual file deletion.

See [Caching](caching.md) for exact eligibility and support.

## A release stopped

`release run` persists the original record before its first side effect.
Inspect it and run the exact recovery command Cargo-Rail reports:

```bash
cargo rail release status --format json
cargo rail release resume
```

`resume` reconciles Git, readiness checks, registry versions, tags, and forge state before advancing.
It does not replan from mutated manifests.
Omit the transaction ID when exactly one active transaction exists.
If several exist, supply the ID reported by `status`.
Recovery requires the original record and sealed package bytes;
commit trailers cannot recreate publication authority.
Finish or reconcile an older record with the executable that created it before upgrading.
Hosted execution continues remotely after terminal closure.
`resume` redispatches the same request; `status` fetches its latest retained progress.
If the merge-event runner stopped,
a resumed dispatch rediscovers the recorded PR and requires the same prepared tree
before binding its merge.
A missing record remains `missing_record`; tags and commit trailers cannot prove completion.
See [releases](releases.md).

Abort and restore only while the status says no external side effect may exist:

```bash
cargo rail release abort release-<id> --yes
```

If the preparation commit was pushed but no tag, registry upload, review, forge release,
or alias effect exists, keep the current branch and retire the transaction explicitly:

```bash
cargo rail release abort release-<id> --retain-preparation --yes
```

The pushed preparation must be an ancestor of the clean local and remote branch tip.
After any publication boundary, resume and reconcile.
Do not move a published tag or replace a release asset.

Commit-driven releases need complete tag history.
Use `fetch-depth: 0` in GitHub Actions or fetch full history and tags before checking the release.
Reviewed-change mode does not use commit history to choose bumps.

## Sync left conflicts

A manual conflict exits `1` and writes a receipt.
Resolve every listed file and remove conflict markers,
then let Cargo-Rail validate and commit the result:

```bash
cargo rail sync --resume target/cargo-rail/receipts/sync-conflict-<crate>-<id>.json
```

Do not commit the conflict manually.
Resume verifies the expected branch, parent, owned paths, and files.

## Exit codes

- `0`: success or a clean check.
- `1`: required changes found, or a resumable sync conflict.
- `2`: invalid arguments or an operational failure.

An executed subprocess may deliberately propagate another status.
