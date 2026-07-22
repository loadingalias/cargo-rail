# Troubleshooting

Start with the planner trace. It records the base ref, file classification, ownership, graph expansion, surface decisions, and execution scope.

## Why did this run?

```bash
cargo rail plan --merge-base --explain
cargo rail plan --merge-base -f json
cargo rail run --merge-base --dry-run --print-cmd --explain
```

Check:

- `surfaces.*.enabled`
- `surfaces.*.reasons`
- `scope.mode`
- `scope.crates`
- `scope.cargo_args`

Use `scope` for execution decisions. Use `impact` for diagnostic context.

## Why is this action uncacheable?

```bash
cargo rail doctor hermeticity --action build
cargo rail doctor hermeticity --profile ci --format json
```

The report binds declared inputs to exact SHA-256 content and lists every reason cargo-rail withheld an `ActionKey`.
Ambient environment or output access, unobserved build scripts or proc macros, an unverified external program or
source, and missing dependency-result digests fail closed. A reported input-root digest is diagnostic evidence; it is
not itself a cache key.

## How do I run the hermetic proof profile?

```bash
cargo rail run --all --action build --hermetic
cargo rail run --all --action build --hermetic --explain
cargo rail run --all --action build --hermetic --no-cache
cargo rail run --all --action build --hermetic --dry-run --format json
```

On a cold run, the first command requires a reviewed `Cargo.lock` and executes the explicit built-in `cargo check`
action in fresh source, target, build, Cargo-home, home, and temporary roots. Before the fetch boundary, only
local-package metadata runs locked/offline. The explicit fetch action may use the network; full metadata and the check
then run locked/offline. An eligible local-cache hit restores before workspace context, metadata, fetch, Cargo, or
rustc starts. The JSON preview names the intended boundary without executing it; preview planning is not itself a
network-denial proof.
Actual execution currently requires `--action build`; default, profile, workflow, and other action selection is
rejected before workspace context or hermetic state is created.
Feature, target, target-kind, and profile arguments remain available after `--`. Workspace/package selectors,
manifest/lockfile changes, output redirects, Cargo configuration, unstable Cargo flags, and raw rustc arguments are
rejected before fetch because they would change an unmodeled input or output boundary.

The profile discovers a preinstalled sysroot toolchain with rustup auto-install/update disabled, then invokes that
exact Cargo/rustc/rustdoc trio. Ambient compiler wrappers are not executed. Configured compiler/rustdoc wrappers and
ambient `RUSTC`/`RUSTDOC` overrides are rejected before fetch state is created. During the check, an observation-only
global wrapper records workspace and external dependency compiler units. Cargo artifact coverage, observed invocations,
emitted outputs, and dep-info must agree exactly or the action remains uncacheable.

Successful executions write a redaction-safe report under `target/cargo-rail/hermetic/reports/`. Read `support`,
`enforcement`, `action_key`, `result_digest`, `fetch`, `output_manifest`, `cache`, and `reasons` together:

- `eligible` currently means a pure current-host Cargo check protected by the macOS filesystem/network sandbox.
- `platform_limited` means isolated roots and offline Cargo ran, but the host could not enforce the complete boundary;
  no authorizing action key is issued.
- `uncacheable`, or an error before execution, means the class or one of its inputs has not earned reuse. Build scripts,
  proc macros, docs, linked artifacts, cross/custom targets, configured linkers/runners, repository wrappers, sccache,
  local directory/source replacement, and alternate registry declarations are deliberately blocked today. A remote
  registry source replacement is supported because its exact fetched package tree is bound and materialized offline.

`cargo check --all-targets` proves compilation of library, binary, test, example, and bench units; it does not execute
tests or benchmarks. Ordinary `cargo rail run` behavior is unchanged and remains the way to run unsupported classes.

A warm `fetch.reused = true` means only that the exact immutable dependency inventory was reused. It is independent of
the local action/output cache. The process-free lookup currently accepts only exact text-mode
`run --all --action build --hermetic`, with optional `--explain` or `--print-cmd`, no `--config` override, and no
trailing Cargo arguments. For an eligible P6 key, the cache surfaces have these meanings:

- `hit`: every CAS object and exact blob byte verified, the complete output manifest was restored into a new root, and
  the hermetic `cargo check` action and compiler units did not execute;
- `miss`: no action-key pin exists, so cold execution may populate it;
- `corrupt` or `incompatible`: a selected pin or object was missing, malformed, tampered, oversized, or from another
  schema. The command fails with an explanation; use `--no-cache` for an intentional cold run or `clean --cache` for
  validated cleanup;
- `disabled`: `--no-cache` was requested or the cache root/reference was unavailable;
- `uncacheable`: P6 did not issue an eligible action key.

These booleans are named `cargo_check_executed` and `compiler_units_executed` deliberately. A hit launches no Cargo,
rustc, or rustdoc process, but it is an action/output restore rather than native per-invocation compiler caching. The
lookup digest is non-authorizing; retained P6 inputs, all CAS objects, exact blob bytes, and final platform/input state
must verify. Cache objects default to `$CARGO_HOME/cargo-rail/local-cas-v1` or
`$HOME/.cargo/cargo-rail/local-cas-v1`. Set `CARGO_RAIL_CACHE_DIR` to choose the base and
`CARGO_RAIL_CACHE_MAX_BYTES` to change the 10 GiB bound.

Preview cleanup without deleting anything, then remove only the validated cargo-rail-owned root and workspace state:

```bash
cargo rail clean --cache --check
cargo rail clean --cache
```

## Why did compiler evidence miss?

`cargo rail unify -f json` reports `evidence_cache` hits, misses, and stable miss reasons per member. A hit requires the
same exact semantic key and successful revalidation of every recorded compiler input, dependency artifact, emitted
output, executable identity, and non-Cargo environment read. Missing files are misses, not fatal errors. Same-size
edits still miss because timestamps and sizes are not authority.

Build scripts, proc macros, unverified external sources, unavailable dep-info, response files, secrets, unmodeled Cargo
configuration, dynamic executable inputs, default linker/SDK state, incomplete rustdoc output trees, and incomplete
platform identity bypass reuse. Any bypass recorded by an observation prevents a hit, and a collector-semantics change
invalidates prior evidence. Cargo freshness does not override those checks. Delete
`target/cargo-rail/cache/compiler-diags-v1.json` only to discard diagnostic evidence; it contains no restorable build
artifacts or Cargo fingerprint state.

Each observation's `execution.wrappers` is the effective outer-to-inner wrapper chain with stable roles and exact
executable identities. `execution.cache_wrapper` explains the separate native compiler-cache boundary:

- `hit` with `verified_local_result` means the stored observation and every current input were re-digested, the final
  action/result binding and CAS objects verified, and the invocation's exact `.d`, `.rmeta`, stdout, and stderr bytes
  were restored;
- `miss` means no candidate survived complete revalidation. A successful cold compile appends
  `stored_verified_result`; a failed publication appends `local_cache_store_failed` and remains non-authorizing;
- `bypassed` with `sccache_wrapper_preserved` means Cargo selected sccache and cargo-rail kept it in place without
  adding a second compiler cache;
- `bypassed` with `existing_compiler_wrapper_preserved` means Cargo selected another global or workspace wrapper. It
  is preserved in order and conservatively treated as potentially cache-owning;
- any other `bypassed` reason names the rejected class or missing proof. Common class reasons include
  `incremental_compilation_not_graduated`, `test_compilation_not_graduated`, `binary_not_graduated`,
  `linker_producing_crate_type_not_graduated`, `proc_macro_not_graduated`, `build_script_not_graduated`,
  `native_linking_not_graduated`, `compiler_stdin_not_graduated`, `compiler_emit_mode_not_graduated`,
  `compiler_flag_not_graduated`, `filesystem_reading_macro_not_graduated`, `multi_source_library_not_graduated`,
  `dependency_crate_not_graduated`, `dependency_artifact_class_not_graduated`, and `cross_target_not_graduated`;
- proof-boundary reasons include `native_cache_platform_not_graduated`, `native_cache_toolchain_not_graduated`,
  `native_cache_session_unavailable`, `compiler_inputs_incomplete`, `secret_compiler_environment`,
  `complete_compiler_observation_unavailable`, `compiler_output_root_not_graduated`,
  `local_cache_candidate_corrupt`, and `verified_result_materialization_failed`.

The cargo-rail diagnostic wrapper remains in Cargo's workspace-wrapper position; an existing workspace wrapper runs
inside it. A custom or renamed cache receives the generic existing-wrapper reason, but still cannot be double-wrapped.
Remove cargo-rail itself from `RUSTC_WRAPPER`, `RUSTC_WORKSPACE_WRAPPER`, or the corresponding Cargo configuration if
the recursive-wrapper error appears; cargo-rail inserts its own roles.

There is no global “compiler caching supported” state. Native reuse currently requires aarch64 macOS, Cargo/rustc
1.97.1, `CARGO_INCREMENTAL=0`, no existing wrapper, and the exact single-source workspace-library metadata class.
`candidate_key` is only an index. `action_key` appears only after the stored observation's source, dependency, and
environment inputs are re-digested. `bytes_hashed` and `bytes_restored` make the per-invocation cost visible. Delete
neither CAS files nor Cargo fingerprints manually: `cargo rail clean --cache` follows the validated workspace reference
and owner marker. The P7.1 hermetic profile remains separate and still rejects configured wrappers.

For a local custom-build unit, the cached observation also contains `build_script_action_key`. A missing `key` is
intentional when Cargo inherited ambient environment or process state, a dependency result is not yet verified, or an
isolated logical launch layout is unavailable. The listed reasons are the missing proof; the captured-input counts are
diagnostic and cannot authorize reuse.

The same custom-build observation contains `build_script_result`. A missing `digest` is also intentional during normal
Cargo execution. `cargo_output` contains redacted counts from Cargo's replayable `build-script-executed` message, not
the raw values. Reasons such as `build_script_instruction_stream_unavailable`,
`build_script_environment_reads_unavailable`, `build_script_generated_output_tree_unavailable`,
`build_script_execution_observations_unavailable`, and `cargo_build_script_execution_freshness_unavailable` mean the
collector saw useful output metadata but not enough proof to authorize reuse. Do not treat the counts or `OUT_DIR`
presence as a result digest.

## Why did this not run?

Check these in order:

1. Confirm the base ref contains the intended comparison range.
2. Confirm the requested action or profile.
3. Inspect confidence-profile expansion.
4. Inspect path classification and custom rules.
5. Check binary-only filtering when `--ignore-bin-crates` is set.

## Common mistakes

### Shallow CI checkout

Commit compatibility modes (`[release] source = "commits"` or `"both"`) need
release tags to measure each crate's history range. In a shallow clone, tags
may be missing, so commit-derived auto-bump planning fails instead of guessing
from incomplete history. The default reviewed-changes mode does not parse
commit history for bump selection.

```bash
git fetch --unshallow --tags
```

In GitHub Actions, use `fetch-depth: 0` for release jobs.

### Interrupted release

`release run` and `release finalize` print a durable state path before their
first side effect. Resume that exact plan after any local, network, registry, or
forge failure:

```bash
cargo rail release status
cargo rail release status --format json
cargo rail release resume target/cargo-rail/releases/release-<id>.json
```

Status does not load Cargo metadata, so it remains available if a crash left a
manifest between local mutations. It reports the phase, exact SHA, last and next
effect, persisted observations, ambiguity, recoverability, and one safe command.

Resume reconciles release commits, exact remote refs, check/pipeline rollups,
crate versions, tags, and forge releases before advancing. It never replans from
already-bumped manifests or republishes an unobserved in-progress crate version.
`awaiting_checks` and registry propagation are explicit external wait boundaries:
cargo-rail exits and never sleeps or polls. Resume after the provider settles.

Every release commit also records the plan-bound transaction plus release mode,
crate versions, and publish/tag authorization in `Rail-Release-*` trailers. In a
fresh checkout at the exact release commit, `release status` reconstructs the
transaction from Git and `cargo rail release resume release-<transaction-id>`
rebuilds the local journal before reconciling remote, forge, and registry truth.
Reconstruction refuses a different HEAD or legacy trailers that do not encode
irreversible-effect authorization.

If no remote, forge, or registry side effect has started, abandon the release
and restore its original local commit with:

```bash
cargo rail release abort target/cargo-rail/releases/release-<id>.json --yes
```

Abort refuses once an external side effect may exist; resume is the safe path
from that point.

Plain `cargo rail clean` removes completed and aborted release journals. It
refuses active or malformed journals before deleting anything; use `release
status` to resolve them first.

### Manual sync conflict

Manual sync conflicts exit with status `1`, leave the merged files on the
`cargo-rail-sync-<crate>` recovery branch, and print a receipt path. Resolve all
listed files, remove every conflict marker, then commit through the receipt:

```bash
cargo rail sync --resume target/cargo-rail/receipts/sync-conflict-<crate>-<id>.json
```

Resume verifies the branch, parent commit, owned paths, and marker-free content
before committing. Do not commit the conflict manually.

### Release PR finalization

`release finalize` expects the version bumps and changelog sections created by
`release run --pr` to already be present in the current checkout. Merge the
release PR first, update the target branch, then finalize from that merged
commit:

```bash
git switch main
git pull --ff-only
cargo rail release finalize --all --yes
```

### Planner surfaces versus run actions

Custom surfaces are planner outputs, not action IDs. Use them in a configured action's `when` policy, then select that
action from the profile.

```toml
# wrong
[run.profile.bench]
actions = ["custom:benchmarks"]

# right
[run.action.benchmark-report]
argv = ["cargo", "run", "-p", "xtask", "--", "benchmark-report"]
when = ["custom:benchmarks"]
inputs = ["benches"]

[run.action.benchmark-report.environment]
inherit = true

[run.profile.bench]
actions = ["bench", "benchmark-report"]
```

### CI/local mismatch

Use the same base-ref strategy and profile in both places.

```bash
# local
cargo rail run --merge-base --profile ci

# CI
cargo rail run --since "$RAIL_SINCE" --profile ci
```
