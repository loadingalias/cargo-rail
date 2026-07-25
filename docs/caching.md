# Caching

cargo-rail has three independent cache boundaries. They share exact-byte identities and local CAS storage where
appropriate, but they do not share eligibility:

| Cache | Purpose | Execution skipped on a hit |
|---|---|---|
| Compiler evidence | Reuse `unify` observations after every recorded input is revalidated | Workspace diagnostic collection |
| Hermetic whole action | Restore the complete declared output of an eligible isolated Cargo check | Cargo, rustc, and rustdoc for the action |
| Native compiler result | Reuse one eligible rustc invocation through Cargo's wrapper boundary | That rustc invocation |

There is no global “caching supported” state. Read the generated [capability matrix](cache-capabilities.md) before
interpreting a hit rate or comparing another repository.

## Native compiler-result reuse

Native reuse is automatic for an ordinary `build` or `distribution` action when every session boundary is supported.
The current class requires:

- `aarch64-apple-darwin` or `aarch64-unknown-linux-gnu`;
- Cargo and rustc 1.97.1;
- `CARGO_INCREMENTAL=0`;
- no Cargo CLI `--config`, action-defined environment, forced incremental mode, existing sccache, or custom compiler
  wrapper;
- an eligible dependency or workspace library invocation with complete observed inputs, dep-info, metadata, optional
  rlib output, Rust-only dependency artifacts, and no native linker responsibility.

Initialize cargo-rail, then run the same action normally:

```bash
cargo rail init

CARGO_INCREMENTAL=0 cargo rail run --all --action build --explain
CARGO_INCREMENTAL=0 cargo rail run --all --action distribution --explain
```

`build` runs the configured check action. `distribution` runs the configured release-build action. Repository
configuration and arguments remain authoritative; an unsupported override causes a named cold bypass.

Use `--no-cache` for an intentional cargo-rail baseline:

```bash
CARGO_INCREMENTAL=0 cargo rail run --all --action build --no-cache
```

A native-cache summary reports `hits`, `misses`, `bypasses`, `setup_bytes_hashed`, `bytes_hashed`, and
`bytes_restored`. Meanings are strict:

- `hit`: a stored observation and every current source, dependency, environment, action, result, tree, and blob binding
  were reverified before exact output bytes were restored;
- `miss`: no candidate survived complete revalidation; successful cold output may populate the local CAS;
- `bypass`: the invocation or session is outside the graduated class and rustc runs normally;
- corrupt or incompatible cache data never authorizes a hit and falls back to the exact cold invocation.

Cargo still creates and validates its own fingerprints around every wrapper invocation. cargo-rail never restores a
target directory, synthesizes Cargo freshness, or treats timestamps, sizes, Cargo's `fresh` flag, or a candidate lookup
as authority.

## Hermetic whole-action reuse

The hermetic profile is separate from native per-invocation reuse:

```bash
cargo rail run --all --action build --hermetic --explain
```

The graduated class is a pure-Rust, current-host Cargo check on macOS. It performs one declared locked fetch, then runs
locked and offline in fresh isolated roots with read-only source and dependency inputs. A verified local-cache hit
restores the complete declared output manifest before workspace context or Cargo starts.

Other hosts remain `platform_limited` until cargo-rail can enforce an equivalent filesystem and network boundary.
Build scripts, proc macros, documentation, native or linked outputs, cross targets, custom tools, and existing wrappers
remain uncacheable in this profile.

## Compiler-evidence reuse

`cargo rail unify --check` can cache compiler observations used to prove dependency and feature decisions. This store is
diagnostic evidence, not restorable build output. It is reused only after the compiler, source, manifest, target,
features, Cargo configuration, dependency artifacts, and every recorded observation are revalidated.

The command does not mutate manifests in check mode, but analysis may update cache and report files under
`target/cargo-rail/`. Use `--skip-report` to suppress the report and `cargo rail clean --cache --reports` to remove
validated cargo-rail state.

## Storage and cleanup

The local CAS defaults to `$CARGO_HOME/cargo-rail/local-cas-v1` or `$HOME/.cargo/cargo-rail/local-cas-v1`.

- `CARGO_RAIL_CACHE_DIR` selects the base directory.
- `CARGO_RAIL_CACHE_MAX_BYTES` changes the positive 10 GiB bound.
- `cargo rail clean --cache --check` previews validated cleanup.
- `cargo rail clean --cache` removes only a cargo-rail-owned root reached through its validated reference and owner
  marker.

Do not delete individual CAS objects or Cargo fingerprint files manually.

## Reproduce the shipped benchmark

The checked-in fixture includes registry and Git dependencies, build scripts, a proc macro, native code, workspace
libraries, and a binary:

```bash
cargo build --release --locked
just bench-native-cache 10
```

The final v0.19.0 measurements used alternating clean roots, offline Cargo, `CARGO_INCREMENTAL=0`, and Cargo/rustc
1.97.1. They are evidence for this fixture, not a promise for unrelated repositories:

| Measurement | Native Cargo | Warm cargo-rail | Result |
|---|---:|---:|---:|
| Apple Silicon check p50 | 6.952 s | 6.281 s | 9.6% faster |
| Apple Silicon release build p50 | 10.300 s | 9.002 s | 12.6% faster |
| ARM64 GNU/Linux check p50 | 7.82 s | 4.70 s | 40.0% faster |
| ARM64 GNU/Linux build p50 | 9.89 s | 5.95 s | 39.8% faster |

The Apple Silicon release-build p95 was 14.0% slower because of one warm outlier. Cold cache population was also slower
than native Cargo, so repeated reuse is required to recover its cost. An unsupported tiny binary measured 0.111 s in
raw Cargo and 0.606/0.602 s through cargo-rail with caching disabled/actively bypassed; the material cost there is the
planner, not cache setup.

An earlier sccache comparison completed the macOS check faster but retained 22 dep-info paths bound to the seed root.
cargo-rail therefore does not claim to replace sccache. Existing sccache installations are preserved and bypass native
reuse. Comparisons must report output portability and invalidation behavior as well as elapsed time.

## Evaluate another workspace

Record enough information to reproduce the result:

```text
repository URL and exact commit:
cargo-rail, cargo, and rustc versions:
host and target triples:
linker, runner, wrappers, and Rust flags:
action and exact argv:
clean-root method:
native Cargo p50/p95:
cargo-rail disabled p50/p95:
cargo-rail cold-population p50/p95:
cargo-rail warm p50/p95:
hits, misses, bypasses, bytes hashed/restored:
all bypass reasons:
output or dep-info portability findings:
```

Use at least five measured runs after one unrecorded warmup, alternate the comparison order, preserve the raw output,
and never combine results from different commits, toolchains, targets, or policies. A false hit, stale physical path,
or unmodeled input is a correctness defect regardless of speed.

See [Architecture](architecture.md#native-compiler-result-cache) for the identity and restore protocol and
[Troubleshooting](troubleshooting.md#why-did-compiler-evidence-miss) for stable bypass reasons.
