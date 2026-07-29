# Caching

Affected planning removes actions that do not need to run. Cargo-Rail's caches remove compiler work from the actions that
remain, but only after the relevant inputs and outputs prove that reuse is valid.

Cargo-Rail has three independent cache boundaries. They share exact-byte identities and local CAS storage where appropriate,
but they do not share eligibility:

| Cache | Purpose | Execution skipped on a hit |
|---|---|---|
| Compiler evidence | Reuse `unify` observations after every recorded input is revalidated | Workspace diagnostic collection |
| Hermetic whole action | Restore the complete declared output of an eligible isolated Cargo check | Cargo, rustc, and rustdoc for the action |
| Native compiler result | Reuse one eligible rustc invocation through Cargo's wrapper boundary | That rustc invocation |

All three follow one rule: a result is reused only after every input that produced it is revalidated. Anything Cargo-Rail
cannot verify runs cold through the normal tool, with a stable reason you can inspect. **Fast when proven. Normal Cargo
when not.** A wrong reuse is a correctness bug, never an acceptable speed tradeoff.

There is no global “caching supported” state. The generated [support matrix](cache-capabilities.md) is authoritative
for execution support, reuse certification, and performance qualification. This document owns the benchmark method,
scorecards, limitations, and result interpretation.

## Native compiler-result reuse

Native reuse is automatic for an ordinary `build` or `distribution` action when every session boundary is supported.
The current class requires:

- an exact Cargo, rustc, rustdoc, sysroot, backend, host, and wrapper-protocol identity with a certificate in the
  generated [support matrix](cache-capabilities.md);
- `CARGO_INCREMENTAL=0`;
- no Cargo CLI `--config`, action-defined environment, forced incremental mode, existing sccache, or custom compiler
  wrapper;
- an eligible dependency or workspace library invocation with complete observed inputs, dep-info, metadata, optional
  rlib output, Rust-only dependency artifacts, and no native linker responsibility.

In practice: use a toolchain release listed in the support matrix, set `CARGO_INCREMENTAL=0`, and do not configure
another compiler wrapper — if sccache or a custom wrapper is present, Cargo-Rail preserves it and steps aside rather than
double-caching.

Initialize Cargo-Rail, then run the same action normally:

```bash
cargo rail init

CARGO_INCREMENTAL=0 cargo rail run --all --action build --explain
CARGO_INCREMENTAL=0 cargo rail run --all --action distribution --explain
```

`build` runs the configured check action. `distribution` runs the configured release-build action. Repository
configuration and arguments remain authoritative; an unsupported override causes a named cold bypass.

Inspect the captured toolchain without running a build:

```bash
cargo rail doctor native-cache --format json
```

The report includes the root-independent capability identity and its evidence when certified. A matching release
without matching executable and sysroot bytes remains cold with `native_cache_capability_not_certified`; a mixed
Cargo/rustc/rustdoc selection reports `native_cache_toolchain_incoherent`.

Use `--no-cache` for an intentional Cargo-Rail baseline:

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

Cargo still creates and validates its own fingerprints around every wrapper invocation. Cargo-Rail never restores a
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

Other hosts remain `platform_limited` until Cargo-Rail can enforce an equivalent filesystem and network boundary.
Build scripts, proc macros, documentation, native or linked outputs, cross targets, custom tools, and existing wrappers
remain uncacheable in this profile.

## Compiler-evidence reuse

`cargo rail unify --check` can cache compiler observations used to prove dependency and feature decisions. This store is
diagnostic evidence, not restorable build output. It is reused only after the compiler, source, manifest, target,
features, Cargo configuration, dependency artifacts, and every recorded observation are revalidated.

The command does not mutate manifests in check mode, but analysis may update cache and report files under
`target/cargo-rail/`. Use `--skip-report` to suppress the report and `cargo rail clean --cache --reports` to remove
validated Cargo-Rail state.

## Storage and cleanup

The local CAS defaults to `$CARGO_HOME/cargo-rail/local-cas-v1` or `$HOME/.cargo/cargo-rail/local-cas-v1`.

- `CARGO_RAIL_CACHE_DIR` selects the base directory.
- `CARGO_RAIL_CACHE_MAX_BYTES` changes the positive 10 GiB bound.
- `cargo rail clean --cache --check` previews validated cleanup.
- `cargo rail clean --cache` removes only a root owned by Cargo-Rail and reached through its validated reference and owner
  marker.

Do not delete individual CAS objects or Cargo fingerprint files manually.

## Reproduce and interpret the benchmark

The checked-in fixture includes registry and Git dependencies, build scripts, a proc macro, native code, workspace
libraries, and a binary:

```bash
cargo build --release --locked
just bench-native-cache 10
```

The accepted development measurement is an interleaved 110-sample run on a local M1 Pro: 22 complete groups, no
rejections, no false hits, identical action censuses and output manifests, and 27 stable warm hits. It used offline
Cargo/rustc 1.97.1, `CARGO_INCREMENTAL=0`, a clean target for every sample, distinct seed/use roots, and a fresh copy of
the populated cache for every warm sample. sccache 0.16.0 used its local disk backend with ambient remote backends
disabled and both physical roots in `SCCACHE_BASEDIRS`.

Times are p50/p95:

| Workload | Lane | p50 | p95 |
|---|---|---:|---:|
| check | native Cargo | 7.028 s | 9.002 s |
| check | Cargo-Rail disabled | 7.397 s | 8.183 s |
| check | Cargo-Rail cold | 9.602 s | 10.115 s |
| check | Cargo-Rail warm cross-root | 4.979 s | 5.615 s |
| check | sccache 0.16.0 | 5.278 s | 5.999 s |
| release build | native Cargo | 10.369 s | 10.808 s |
| release build | Cargo-Rail disabled | 10.947 s | 11.262 s |
| release build | Cargo-Rail cold | 14.336 s | 15.173 s |
| release build | Cargo-Rail warm cross-root | 7.630 s | 8.347 s |
| release build | sccache 0.16.0 | 8.173 s | 8.902 s |

Warm Cargo-Rail saved 29.2% at check p50 and 26.4% at release-build p50 versus native Cargo. It saved 48.1% and 46.8%
respectively versus cold publication; two warm reuses repaid the measured cold overhead for both workloads. Against
sccache, warm Cargo-Rail was 5.7% faster for checks and 6.6% faster for release builds at p50.

Dedicated AWS runs using the same 110-sample contract also qualified on Linux x86-64 and Linux ARM64:

| Host | Workload | Native p50/p95 | Cargo-Rail warm p50/p95 | sccache p50/p95 |
|---|---|---:|---:|---:|
| Linux x86-64 | check | 5.841/5.891 s | 3.430/4.074 s | 3.155/3.247 s |
| Linux x86-64 | release build | 9.224/9.351 s | 5.547/5.605 s | 6.223/6.244 s |
| Linux ARM64 | check | 6.166/6.266 s | 3.870/4.325 s | 3.843/3.882 s |
| Linux ARM64 | release build | 10.591/10.674 s | 6.618/6.696 s | 7.470/7.562 s |

Warm Cargo-Rail beat native Cargo on every qualified host and workload. Against sccache, it won both macOS workloads
and both Linux release builds; sccache led Linux checks by 8.7% on x86-64 and 0.7% on ARM64 at p50. These remain
same-host fixture results, not universal performance claims.

Windows x86-64 timing remains unqualified. Its latest partial run preserved exact rejected evidence: reusable clean-root
hits were complete, but two deliberately bypassed consumers observed byte-unstable proc-macro DLLs, and PID reuse could
overwrite explain-event files. Neither rejected timing nor the older pre-evidence run belongs in this scorecard.
Existing sccache installations remain preserved and cause Cargo-Rail native reuse to bypass.

The benchmark maps each modeled compiler output back to its versioned unit event. A modeled output may retain a
physical root only when that exact path belongs to an explicit bypass; eligible outputs must remain portable and
byte-identical. Cargo-owned projections outside the compiler-output model are reported but never treated as cache
authority.

## Performance and coverage priorities

This section records current limitations and the order in which broader coverage graduates; it is engineering
context, not something you need to act on to use the cache.

Treat elapsed time as the sum of distinct work:

- fixed front-end work before Cargo starts;
- cold observation, validation, hashing, and CAS publication;
- warm candidate discovery, revalidation, CAS reads, and output materialization; and
- cold execution for every bypassed compiler invocation.

The tiny-action p50 gap of 0.203–0.721 seconds is an upper bound on removable front-end work, not a guaranteed saving.
The first performance target is an eligibility boundary that avoids workspace planning, metadata, wrapper, report, and
cache setup when no graduated unit can reuse a result. Cold-path work must hash immutable bytes once per captured
snapshot and must not republish verified CAS objects. Warm-path work must reduce scans, decoding, revalidation, and
materialization without allowing a candidate lookup to authorize restoration.

Broader coverage is an accuracy problem before it is a hit-rate problem:

| Compiler class | Proof required before reuse |
|---|---|
| Linked binaries and linked crate types | Exact linker/driver, response files, sysroot, SDK, native libraries, environment, dependency results, and complete executable/debug output |
| Test compilation | `cfg(test)`, harness generation, dev dependencies, profile, target, linker, runner configuration, and produced test executable; test execution remains a separate action |
| Proc macros | Macro binary and compiler bridge, input tokens, output tokens and diagnostics, plus isolated or observed filesystem, environment, subprocess, time, randomness, and network inputs |
| Build scripts | Existing `BuildScriptActionKey` and `BuildScriptResult` authority plus the ordered Cargo instruction stream, runtime reads, subprocesses, environment, and exact `OUT_DIR` tree |
| Native compilation and linking | External compilers, assemblers, archivers, linkers, headers, SDKs, libraries, discovery inputs, response files, dependency files, and output formats |
| Additional Rust toolchains | Exact Cargo/rustc/rustdoc/sysroot/backend identity and a versioned probe of wrapper, argv, dep-info, emit, response-file, path-remap, and output behavior |

“Arbitrary toolchain support” cannot mean accepting an unknown compiler. Cargo-Rail qualifies an exact toolchain
protocol and issues `native_cache_toolchain_not_graduated` for an unqualified release,
`native_cache_toolchain_incoherent` for a mixed toolchain, or `native_cache_capability_not_certified` when an exact
candidate lacks proof. Every new class must pass positive reuse, negative mutation, corruption, partial-result,
cross-root, concurrent-publication, and native-platform tests. One false hit blocks graduation regardless of
performance.

## Evaluate another workspace

Record enough information to reproduce the result:

```text
repository URL and exact commit:
Cargo-Rail (`cargo-rail`), Cargo, and rustc versions:
host and target triples:
linker, runner, wrappers, and Rust flags:
action and exact argv:
clean-root method:
native Cargo p50/p95:
Cargo-Rail disabled p50/p95:
Cargo-Rail cold-population p50/p95:
Cargo-Rail warm p50/p95:
hits, misses, bypasses, bytes hashed/restored:
all bypass reasons:
output or dep-info portability findings:
```

Use at least five measured runs after one unrecorded warmup, alternate the comparison order, preserve the raw output,
and never combine results from different commits, toolchains, targets, or policies. A false hit, stale physical path,
or unmodeled input is a correctness defect regardless of speed.

See [Architecture](architecture.md#native-compiler-result-cache) for the identity and restore protocol and
[Troubleshooting](troubleshooting.md#why-did-compiler-evidence-miss) for stable bypass reasons.
