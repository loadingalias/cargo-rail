# Caching

> Auto-generated from executable CI/release registries, native-cache production gates, reviewed qualification
> manifests, and retained benchmark evidence. Do not edit manually.
>
> Regenerate with: `./scripts/docs/generate.sh`. Support manifest schema: `4`;
> native-cache capability registry schema: `1`; qualification schema:
> `1`.

Planning removes actions that do not need to run. Caching removes work from selected actions only when Cargo-Rail can
prove that the stored result still matches every relevant input.

| Cache | Purpose | Work skipped on a hit |
|---|---|---|
| Compiler evidence | Reuse `unify` observations after complete input revalidation | Workspace diagnostic collection |
| Hermetic whole action | Restore one eligible isolated Cargo check | Cargo and compiler work; the exact fast path also skips bootstrap |
| Native compiler result | Restore one eligible rustc result through Cargo's wrapper boundary | That rustc invocation |

The layers have separate eligibility. A lookup or Cargo `fresh` flag never authorizes reuse. Cargo-Rail revalidates the
input identity, action/result binding, manifest, and exact stored bytes owned by the layer. Incomplete evidence runs
cold with a stable reason. **Fast when proven. Normal Cargo when not.**

## Native compiler-result cache

Native reuse is automatic for an ordinary `build` or `distribution` action when all of these are true:

- the exact Cargo, rustc, rustdoc, sysroot, backend, host, and wrapper protocol has a certificate below;
- `CARGO_INCREMENTAL=0`;
- no Cargo CLI `--config`, action-defined environment, sccache, or custom compiler wrapper changes the boundary; and
- the invocation is an eligible dependency or workspace library with complete Rust inputs, dep-info, metadata,
  optional rlib output, Rust-only dependency artifacts, and no linker responsibility.

```bash
CARGO_INCREMENTAL=0 cargo rail run --all --action build --explain
CARGO_INCREMENTAL=0 cargo rail run --all --action distribution --explain
cargo rail doctor native-cache --format json
```

The doctor reports the exact capability identity and certificate evidence without running a build. A missing
certificate is a cold execution boundary, not a command failure. Existing wrappers remain in their selected order and
Cargo-Rail does not add a second cache.

The run summary reports `hits`, `misses`, `bypasses`, `setup_bytes_hashed`, `bytes_hashed`, and `bytes_restored`:

- `hit` means current inputs and every stored object were reverified before exact output bytes were restored;
- `miss` means no candidate survived revalidation; successful cold output may populate the local CAS; and
- `bypass` means the session or invocation is outside the graduated class and rustc executed normally.

Corrupt or incompatible native entries never authorize reuse and fall back to the exact cold invocation. Cargo still
owns its fingerprints; Cargo-Rail never restores a target directory or synthesizes Cargo freshness. Use `--no-cache`
for an intentional cold baseline.

## Hermetic whole-action cache

```bash
cargo rail run --all --action build --hermetic --explain
cargo rail run --all --action build --hermetic --no-cache
```

The graduated class is a pure-Rust, current-host Cargo check on macOS. A cold run requires an exact `Cargo.lock`,
performs one `cargo fetch --locked` network boundary, then runs locked and offline in fresh roots with read-only source
and dependency inputs.

The process-free lookup accepts `cargo rail run --all --action build --hermetic` in text mode, optionally with
`--explain` or `--print-cmd`, and no configuration override or trailing Cargo arguments. Its verified hit restores the
complete output manifest before workspace context, metadata, fetch, Cargo, or compiler processes start. Other requests
bootstrap normally before any action-cache decision.

Other hosts run the isolated check but report `platform_limited` and receive no action key until Cargo-Rail can enforce
an equivalent filesystem and network boundary. Build scripts, proc macros, documentation, linked or native artifacts,
cross targets, custom tools, configured wrappers, and unmodeled Cargo overrides fail closed for this profile.

`target/cargo-rail/hermetic/reports/` records support, enforcement, action and result identities, fetch reuse, outputs,
cache status, and stable reasons. A corrupt or incompatible whole-action entry fails rather than silently restoring;
use `--no-cache` for a deliberate cold run or validated cleanup to discard it.

## Compiler-evidence cache

`cargo rail unify --check` may reuse compiler observations after revalidating the compiler, source, manifest, target,
features, Cargo configuration, dependency artifacts, emitted outputs, executable identity, and recorded environment
reads. This store contains diagnostic evidence, not restorable Cargo artifacts.

Check mode does not edit manifests, but analysis may update cache and report files under `target/cargo-rail/`.
`cargo rail unify --check -f json` exposes `evidence_cache` hits, misses, and reasons.

## Storage and cleanup

The local CAS defaults to `$CARGO_HOME/cargo-rail/local-cas-v1` or
`$HOME/.cargo/cargo-rail/local-cas-v1`. `CARGO_RAIL_CACHE_DIR` selects another base;
`CARGO_RAIL_CACHE_MAX_BYTES` changes the positive 10 GiB bound.

```bash
cargo rail clean --cache --check
cargo rail clean --cache
```

Cleanup follows a validated workspace reference and owner marker. Do not remove individual CAS objects or Cargo
fingerprints by hand.

## Execution and reuse support

Execution support, cache graduation, and performance qualification are independent. A cache bypass still executes
Cargo normally. Native graduation is certificate-specific, not target-wide.

### Hosts and targets

| Target | Native execution | Cross-target compilation | Release artifact | Native compiler-result cache | Performance qualification |
|---|---|---|---|---|---|
| `aarch64-apple-darwin` | Advertised; full-suite CI required (`macos-15`) | — | Native artifact required | Graduated `library_metadata_rlib` (Cargo `1.97.1`, rustc `1.97.1`; 1 exact certificate) | Qualified: check + release-build; 110 accepted, 0 false hits |
| `aarch64-pc-windows-msvc` | Advertised; full-suite CI required (`windows-11-arm`) | — | Native artifact required | Bypass: `native_cache_platform_not_graduated` | Not qualified |
| `aarch64-unknown-linux-gnu` | Advertised; full-suite CI required (`ubuntu-24.04-arm`) | — | Native artifact required | Bypass: `native_cache_capability_not_certified` | Qualified: check + release-build; 110 accepted, 0 false hits |
| `aarch64-unknown-linux-musl` | Not a native host | Required compatibility build | Cross-built artifact required | Bypass: `cross_target_not_graduated` | Not qualified |
| `thumbv7em-none-eabihf` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` | Not qualified |
| `wasm32-unknown-unknown` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` | Not qualified |
| `wasm32-wasip1` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` | Not qualified |
| `wasm32v1-none` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` | Not qualified |
| `x86_64-apple-darwin` | Advertised; full-suite CI required (`macos-15-intel`) | — | Native artifact required | Bypass: `native_cache_platform_not_graduated` | Not qualified |
| `x86_64-pc-windows-msvc` | Advertised; full-suite CI required (`windows-2022`) | — | Native artifact required | Bypass: `native_cache_capability_not_certified` | Not qualified |
| `x86_64-unknown-linux-gnu` | Advertised; full-suite CI required (`ubuntu-24.04`) | — | Native artifact required | Bypass: `native_cache_capability_not_certified` | Qualified: check + release-build; 110 accepted, 0 false hits |
| `x86_64-unknown-linux-musl` | Not a native host | Required compatibility build | Cross-built artifact required | Bypass: `cross_target_not_graduated` | Not qualified |

Linux musl rows are release cross-builds, not native Linux host evidence.

### Filesystems

| Profile | Runner | Filesystem | Case behavior | Required evidence |
|---|---|---|---|---|
| Default `aarch64-apple-darwin` | `macos-15` | `apfs` | Insensitive | Full endpoint suite and native probe |
| Default `aarch64-pc-windows-msvc` | `windows-11-arm` | `ntfs` | Insensitive | Full endpoint suite and native probe |
| Default `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | `ext4` | Sensitive | Full endpoint suite and native probe |
| Default `x86_64-apple-darwin` | `macos-15-intel` | `apfs` | Insensitive | Full endpoint suite and native probe |
| Default `x86_64-pc-windows-msvc` | `windows-2022` | `ntfs` | Insensitive | Full endpoint suite and native probe |
| Default `x86_64-unknown-linux-gnu` | `ubuntu-24.04` | `ext4` | Sensitive | Full endpoint suite and native probe |
| linux-tmpfs | `ubuntu-24.04` | `tmpfs` | Sensitive | Front-door corpus, CAS/atomicity suite, ENOSPC, and cleanup |
| macos-apfs-case-sensitive | `macos-15` | `apfs` | Sensitive | Front-door corpus, CAS/atomicity suite, ENOSPC, and cleanup |
| windows-ntfs-vhd | `windows-2022` | `ntfs` | Insensitive | Front-door corpus, CAS/atomicity suite, ENOSPC, and cleanup |

Alternate profiles use bounded temporary volumes and detach them after success or failure.

### Deferred native hosts

| Platform | Target | Execution status | Cache status | Performance status |
|---|---|---|---|---|
| IBM Power | `powerpc64le-unknown-linux-gnu` | Pass-through target execution remains unqualified | `native_cache_platform_not_graduated` | Not qualified |
| IBM Z | `s390x-unknown-linux-gnu` | Pass-through target execution remains unqualified | `native_cache_platform_not_graduated` | Not qualified |

IBM Power and IBM Z need native runners before Cargo-Rail can advertise compatibility, cache graduation, or performance
qualification.

### Linkers and codegen backends

| Capability | Advertised non-default implementations | Current contract |
|---|---|---|
| Linker | None | Default, explicit-driver, and bundled host LLD pass-through are tested. Other selected linkers use `configured_linker_not_graduated`. |
| Codegen backend | None | Stable LLVM and pinned-nightly Cranelift pass-through are tested. Other selected backends use `codegen_backend_not_graduated`. |

Pass-through execution is not cache graduation. A non-default implementation needs a named compatibility fixture on
every applicable native host before Cargo-Rail advertises it.

### Native compilation classes

| Class | Reuse status | Boundary |
|---|---|---|
| Dependency and workspace library metadata/rlib | Graduated only for listed host/toolchain tuples | One declared crate root, complete observed Rust inputs, dep-info, `.rmeta`, optional `.rlib`, Rust-only dependencies, no linker responsibility |
| Incremental compilation | Bypassed; compiler executes | Requires `CARGO_INCREMENTAL=0`; forced incremental mode also bypasses |
| Binary, test, example, and benchmark linking | Bypassed; compiler/linker executes | Linker-producing invocations are not graduated |
| `dylib`, `cdylib`, and `staticlib` | Bypassed; compiler/linker executes | Native linker, SDK, runtime, and archive boundaries are incomplete |
| Proc macros and their consumers | Bypassed; compiler executes | Compile-time filesystem and process reads are not completely observed |
| Build scripts and generated output | Bypassed; build script executes | Cargo messages do not prove the ordered instruction stream, runtime reads, output tree, or freshness |
| Native dependencies and `links` contracts | Bypassed; native tools execute | External tools, headers, SDKs, libraries, discovery inputs, and outputs are incomplete |
| rustdoc and doctests | Bypassed; rustdoc/test executes | Stable Cargo output does not enumerate the complete documentation tree |
| Cross compilation and custom targets | Bypassed; compiler executes | Host/target tools, runners, SDKs, and target specifications are not graduated |
| Existing sccache or custom wrappers | Preserved; Cargo-Rail reuse bypassed | The selected wrapper chain remains authoritative and is never double-cached |
| Cargo CLI `--config` and action environments | Bypassed; Cargo executes | Effective configuration or environment is outside the graduated direct-action contract |

## Benchmark evidence

The fixture contains registry and Git dependencies, build scripts, a proc macro, native code, workspace libraries, and
a binary:

```bash
cargo build --release --locked
just bench-native-cache 10
```

The accepted local M1 Pro measurement interleaved 110 samples in 22 complete groups with no rejections or false hits,
identical action censuses and output manifests, and 27 stable warm hits. Every sample used offline Cargo/rustc
`1.97.1`, `CARGO_INCREMENTAL=0`, a clean target, distinct seed/use roots, and a fresh copy of the populated cache.
sccache `0.16.0` used its local disk backend with remote backends disabled.

Times are p50/p95:

| Host and workload | Native Cargo | Cargo-Rail disabled | Cargo-Rail cold | Cargo-Rail warm | sccache |
|---|---:|---:|---:|---:|---:|
| M1 Pro check | 7.028/9.002 s | 7.397/8.183 s | 9.602/10.115 s | 4.979/5.615 s | 5.278/5.999 s |
| M1 Pro release build | 10.369/10.808 s | 10.947/11.262 s | 14.336/15.173 s | 7.630/8.347 s | 8.173/8.902 s |
| Linux x86-64 check | 5.841/5.891 s | — | — | 3.430/4.074 s | 3.155/3.247 s |
| Linux x86-64 release build | 9.224/9.351 s | — | — | 5.547/5.605 s | 6.223/6.244 s |
| Linux ARM64 check | 6.166/6.266 s | — | — | 3.870/4.325 s | 3.843/3.882 s |
| Linux ARM64 release build | 10.591/10.674 s | — | — | 6.618/6.696 s | 7.470/7.562 s |

On the M1 Pro, warm Cargo-Rail reduced p50 by 29.2% for checks and 26.4% for release builds versus native Cargo; two
warm reuses repaid the measured cold publication overhead. Warm Cargo-Rail beat native Cargo for every qualified host
and workload. sccache led the two Linux check workloads; Cargo-Rail led the other four comparisons. These are same-host
fixture results, not universal performance claims.

Windows x86-64 timing remains unqualified. Its latest partial run found reusable clean-root hits but rejected timing
because two bypassed proc-macro consumers produced byte-unstable DLLs and PID reuse could overwrite evidence files.

When evaluating another workspace, record the repository commit, tool and host/target identities, linker, runner,
wrappers, flags, exact action argv, clean-root method, p50/p95 for native/disabled/cold/warm lanes, hit and byte counts,
all bypass reasons, and output portability findings. Use at least five measured runs after warmup, alternate lane order,
preserve raw output, and never combine different commits, toolchains, targets, or policies.
