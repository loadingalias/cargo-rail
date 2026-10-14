# Troubleshooting

Start with the decision output or status output of Cargo-Rail.
That output records the failed boundary and the safe next command.

## A plan selected the wrong work

```bash
cargo rail plan --explain
cargo rail plan --json | jq '{inputs, changes, required, work, evidence}'
```

Do these checks in this order:

1. Make sure that `inputs.base`, `inputs.head`, and `inputs.head_commit` identify the intended comparison.
1. Make sure that `changes` contains the expected file, Cargo, and configuration deltas.
1. Read `work.NAME.state`.
   It shows whether the item is required or skipped.
   The `.evidence` entries of the item refer to the top-level `evidence` map.
   A required item also has a `.cause`.
1. For a required item, read `work.NAME.scope`.
   It contains the package, target, or variant selector that the executor uses.

Explanations and evidence are not execution input.
If the plan is correct but execution is wrong, examine the consumer that lowered the typed scope.
Transfer one exact plan between CI jobs.
If each job makes its own plan, the work can differ when the base, source, Cargo universe,
toolchain, catalog, platform, or evidence differs.

For the consumer contract, see [Planning](planning.md).

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

Each Cargo metadata failure gives one cause, one recovery, and a `reproduce:` command.
This command repeats the exact Cargo invocation, including `--locked`.
The named causes are a stale `Cargo.lock`, a manifest that Cargo cannot load,
a target or `rustc` that is not available, and a missing Cargo executable.

A registry token and a credential provider are examples of a Cargo credential capability.
When a Cargo credential capability is active, Cargo-Rail does not show the output of Cargo,
because that output can contain provider output.
Manifest failures are the exception: Cargo-Rail shows their output,
because Cargo reports them before it contacts a registry.
To see the complete output of Cargo, run the reproduction command.

A JSON error gives its class in `failure_class`.
The class is `lockfile`, `manifest`, `toolchain`, or `cargo`.

## Unify compiler evidence fails

Unify compiles the workspace to prove which dependencies are unused.
When one compiler-evidence view fails, Unify stops and exits `2`.
It reports one cause, one recovery section, and a `reproduce with Cargo:` command.
That command runs the Cargo command of the view in the workspace root,
outside the private target directory of Cargo-Rail.

| `failure_class` | Cause | Recovery |
| --- | --- | --- |
| `build_script` | The build script of a package failed | Supply the tools, files, or environment that it needs |
| `source` | Rust source failed to compile | Fix the compiler errors that appear under the cause |
| `toolchain` | A target library, linker, `rustc`, or `RUSTC_WRAPPER` is missing or cannot run | Install or correct it, or narrow `unify.compiler_targets` |
| `cargo_rail` | The compiler adapter of Cargo-Rail failed | Report the `--verbose` output as a Cargo-Rail defect |
| `cargo` | Any other Cargo failure | Run the reproduction command |
| `interrupted` | SIGINT or SIGTERM stopped compiler acquisition | Run the same command again |

A build-script failure lists the names of the environment variables that the script declares with `rerun-if-env-changed`.
Unify never prints their values.
In text mode, Unify shows the last lines of the build script's own stderr under the cause.
Unify replaces each exact value of an inherited environment variable in that text with `<env:NAME>`.
The complete output of Cargo appears only with `--verbose`.
It never appears when a Cargo credential capability is active.
When a build script reports a missing native tool through the `cc`, `cmake`, or `pkg-config` crates,
the cause names that tool.
When source reads a file that does not exist, the cause names that file.
Unify applies the same redaction to these names.
JSON errors contain no other Cargo or build-script output.

With `unify.compiler_targets = "none"`, Unify runs without compiler evidence.
Unify then keeps each dependency whose use it cannot prove.

Unify writes progress to stderr, also with `--format json`.
With `--format json`, stdout stays one JSON value.
When a phase prints nothing for 30 seconds, Unify prints a `Still running:` line.
This line names the phase.
It also tells whether Cargo-Rail is analyzing, waiting for a Cargo subprocess,
or waiting for the Cargo file lock.
Unify reports a Cargo file-lock wait as soon as Cargo reports it.
`--quiet` disables progress output.

In a repository wrapper, pass stderr through as it arrives and capture only stdout:

```bash
cargo rail unify --check --format json > target/unify.json
```

In every output format, progress is plain lines on stderr.
Unify writes each line when its event occurs.
A wrapper relays progress unchanged, without parsing it.
If a wrapper captures both streams until the command exits,
all progress stays hidden for the full run.
`$(cargo rail unify --check 2>&1)` is an example of this capture.
`--diagnostics-file` records each completed phase in `progress_phases`.
Each record includes the activity of the phase, whether Cargo waited for a file lock,
and the duration of the phase.

An interrupted acquisition stops its Cargo process tree and names the interrupted phase.
Compiler acquisition writes only to its private target directory,
so the workspace files stay unchanged.
Outside compiler acquisition, SIGINT and SIGTERM stop the process immediately.
A second signal always stops the process immediately.

## Surface is unavailable or reports unexpected findings

An installation from `cargo install` or from single-binary `cargo binstall` does not have the embedded Surface component set.
`cargo rail surface --schema` is still available without that component set.
Use one of these options:

- Install and extract a supported native ZIP.
  Keep all its components together.
- Select an independently authenticated [compiler adapter pack](caching.md#select-an-independent-compiler-adapter).

To prepare the exact toolchain that the workspace selects, run this command:

```bash
cargo rail surface --prepare --json
```

Preparation can install `rustc-dev` for the selected rustup toolchain.
It does not change the default toolchain.

For an unexpected gate or finding, examine planning, policy, readiness,
and report authority separately:

```bash
cargo rail plan --json | jq '.work.surface'
cargo rail config explain surface.enabled surface.targets surface.consumer_scope --json
cargo rail surface --prepare --json
cargo rail surface --check --json
```

Before you change policy, verify these values in the report: audited and open targets, products,
target views, feature views, and completeness.
An open compiler-crate observation keeps the declaration.
Use `consumer_scope = "workspace"` only when the workspace contains every consumer of its private compiler crates.
Prove that boundary with reviewed inventories of published APIs, downstream repositories, plugins,
generated code, and build-script or proc-macro consumers.
Cargo metadata from this checkout cannot prove that the external set is empty.

If one compiler view fails, correct the source failure.
Then run the exact `surface --resume` command that the error prints.
The acquisition journal identifies completed and pending views.
The journal alone never authorizes cached facts.

## Compiler reuse did not happen

```bash
cargo rail cache setup --check
cargo rail cache probe --json
cargo rail cache status --scope local --json
cargo rail doctor native-cache --json
```

With any of these conditions, the original compiler runs:

- a missing compiler identity;
- incremental compilation;
- an unsupported compiler class;
- a conflicting wrapper; or
- incomplete input evidence.

An installation without embedded component authority also needs an independently authenticated
[compiler adapter pack](caching.md#select-an-independent-compiler-adapter).
If pack authentication, compilation, or calibration fails, Cargo-Rail safely bypasses native reuse.
Surface fails when it requires those compiler facts.
Physical-root mode also binds the canonical checkout.
Use `--root-portability remap` only for certified cross-root Rust metadata and library results.
Cargo-Rail supports external `CARGO_TARGET_DIR` locations for eligible native results.
A bypass is a safe fallback, not a failed hit.

`native_cache_hardware_qualification_unavailable` means that Cargo-Rail recognizes the operating system,
but the architecture is not in the [native host table](caching.md#native-host-eligibility).
`native_cache_platform_qualification_unavailable` means that the operating system is not supported.
An explicit target does not cause an automatic bypass.
Reuse requires the compiler distribution, target definition, sysroot,
and target-library evidence of the selected target.
Linked outputs also require complete linker evidence.
Before you change the build target, examine the missing boundary in the report.

Status schema 18 keeps the native failure-reason counters outside the usage ledger.
The usage ledger has a bound of 65,536 events.
Before you interpret `status.installation.usage.failure_reasons`, examine `status.installation.usage.failure_reason_counts_available`.
When the ledger is full, it stops accepting events.
The separate failure counters continue to increase.
A strict `cache probe` verifies authenticated provider readiness and protocol readiness.
It does not show the remote URL, object names, credentials, or local paths.

To disable an installed wrapper for one process tree and keep the setup unchanged, run:

```bash
CARGO_RAIL_CACHE=off cargo check --locked
```

`cargo rail unify --check --json` reports its separate compiler-evidence cache.
That cache never restores Cargo build artifacts.

Before you remove state, preview the cleanup:

```bash
cargo rail cache clean --scope workspace --check
cargo rail cache clean --scope local --check
cargo rail cache detach --check
cargo rail cache profiles --json
cargo rail cache uninstall --check
```

Workspace cleanup removes the reconstructible state for the current checkout.
Local cleanup removes the CAS of the current profile.
After local cleanup, you must run `cargo rail cache setup` to repair the CAS.
`cache detach` keeps the profile and the CAS, and removes the current root binding.
`cache uninstall` removes the global wrapper and Cargo configuration.
It keeps every profile.
For explicit machine-wide cleanup, use `cache profiles` and `cache drop-profile --profile PROFILE_ID --check`.
Resolve receipt, wrapper, profile, or ownership drift.
Do not delete cache files or Cargo configuration by hand instead.

After an upgrade to 0.30.0, every cache starts cold one time.
The identity changed for action keys, registry dependency results, stored compiler evidence,
and units that load a procedural macro.
Remote caches also start cold.
Before the first build, do these steps:

1. Run `cargo rail cache clean --scope local --check`.
   It previews the removal of the current store and the retired `local-cas-v2` store.
1. Run the same command without `--check`.
1. Run `cargo rail cache setup` again.

Cargo-Rail rejects independent compiler adapter packs from earlier releases
(native input protocol 4).
Publish these packs again for 0.30.0.

Cargo-Rail 0.32 uses native input protocol 5.
It rejects independent compiler adapter packs from 0.30 and 0.31.
Publish these packs again for 0.32.

If setup reports an unsupported installation receipt version, do these steps:

1. Keep the installation and the executable that created it.
1. Use that executable to preview the removal.
1. Use that executable to do the removal.
1. Run the current `cargo rail cache setup`.

The receipt error cannot identify the release version that created the receipt.
Do not delete the files manually instead of this procedure.

For exact eligibility and support, see [Caching](caching.md).

## A release stopped

Run the same command again:

```bash
cargo rail release
```

The command reads the most recent release on the branch from Git tags, the registry, and GitHub.
It completes only the missing steps.
It packages from a temporary clone of the release commit, so the local worktree has no effect.
The `state` and `next` fields of the result show what remains:

| `state`           | Meaning and next action |
| ----------------- | ----------------------- |
| `awaiting_checks` | Checks on the source commit are still running. Nothing changed. When the checks finish, run the command again. |
| `awaiting_assets` | A draft GitHub release waits for the files in `missing_assets`. Attach these files, then run the command again. |
| `pull_request`    | A release pull request is open. Merge it, then run the command again. |
| `released`        | Every step is complete. |

If `origin` rejects a push, the release leaves nothing behind.
The checkout goes back to its starting commit.
Pull, then run the command again.

These errors need a decision, not a rerun:

- **A version on the registry came from another commit, or is yanked.**
  That version belongs to another source.
  Release a new version.
- **A tag on `origin` names another commit.**
  A release never moves a published tag.
  Examine the tag.
- **An asset has no matching attestation.**
  Use `loadingalias/cargo-rail-action/release/asset` to build the asset again from the release tag and attach it.
  For a repository that cannot attest, set `release.asset_attestations = "off"` instead.
- **An alias names a commit outside this release line.** Examine the alias.
  A release moves an alias only forward.

Use the same procedure to finish a release that Cargo-Rail 0.31 started,
after its release commit is on the branch.

Commit-driven releases need the complete tag history.
Use `fetch-depth: 0` in GitHub Actions, or fetch the full history and tags before you release.
Reviewed-change mode does not use commit history to choose bumps.

See [releases](releases.md).

## Sync left conflicts

When sync stops at a manual conflict, it exits `1` and writes a receipt.
Resolve every listed file and remove the conflict markers.
Then run this command to let Cargo-Rail validate and commit the result:

```bash
cargo rail sync --resume target/cargo-rail/receipts/sync-conflict-<crate>-<id>.json
```

Do not commit the conflict manually.
Resume verifies the expected branch, parent, owned paths, and files.

## Exit codes

- `0`: success or a clean check.
- `1`: required changes found, or a resumable sync conflict.
- `2`: invalid arguments or an operational failure.

An executed subprocess can deliberately propagate a different status.

## Repair a deleted compiler-cache installation

Run `cargo rail cache setup` from an enrolled workspace.
Setup restores a missing installation
when the wrapper path in the Cargo configuration exactly equals the managed path of Cargo-Rail.
Setup does this even if the receipt was deleted.
Other configured wrappers still require an explicit decision from the owner.
The repair keeps unrelated Cargo configuration.
It replaces the receipt and the missing components.
To prove a cold build and verified reuse, run `cargo rail cache ready`.

The diagnostic for a stale authenticated compiler component shows its path,
the found and expected digests, the adjacent component-manifest version,
and the running Cargo-Rail version.
Install the complete matching native archive from the release URL in the diagnostic.
Then run `cargo rail cache setup` again.
A source installation has no archive driver authority.
It ignores archive-only driver files that remain beside it.
Cargo still runs through the supported fallback of the source installation.
