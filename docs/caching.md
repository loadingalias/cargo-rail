# Caching

> Auto-generated from executable CI/release registries, native-cache production gates, reviewed qualification
> manifests, and reviewed benchmark evidence. Do not edit manually.
>
> Regenerate with: `./scripts/docs/generate.sh`. Support manifest schema: `4`;
> native-cache toolchain-identity schema: `2`; qualification schema:
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

- Cargo-Rail can content-identify the exact Cargo, rustc, rustdoc, complete sysroot, host, and wrapper protocol;
- the selected Cargo profile has no active fingerprints and neither the environment nor rustc forces incremental
  compilation;
- no Cargo CLI `--config`, action-defined environment, unknown Cargo setting, `build.dep-info-basedir`, sccache, or
  custom compiler wrapper changes the boundary; and
- the invocation is an eligible dependency or workspace library with complete rustc-reported filesystem and
  environment inputs, root-independent dep-info, metadata, optional rlib output, Rust-only dependency artifacts, and
  no linker responsibility.

```bash
cargo rail run --all --action build --explain
cargo rail run --all --action distribution --explain
cargo rail doctor native-cache --format json
```

The reusable session binds toolchain, compiler class, wrapper protocol, capabilities, and compiler-process environment.
It does not bind the complete Cargo configuration or `Cargo.lock`. Each compiler unit instead binds its exact rustc
arguments and cfg, source inputs, dependency artifact contents, and rustc-reported filesystem and environment reads.
Changes to `build.warnings`, jobs, build or target directories, network policy, registry settings, and unrelated
lockfile entries can therefore reuse a result when those exact unit inputs remain unchanged. Rust flags, features,
dependency contents, target, linker, sysroot, and observed inputs still change or reject reuse at their owning
boundary.

Filesystem reads include files used through `include!`, `include_str!`, and `include_bytes!`. A dep-info file that
retains the physical workspace root as an input bypasses reuse because that input cannot be restored unchanged in
another root. The CAS stores canonical internal tokens for verified dep-info and compiler-stream output paths, then
binds them to Cargo's current output directory after verification. Output names and materialized bytes remain exact;
any other cached reference to a previous output directory fails closed.
Cargo-Rail sets `CARGO_INCREMENTAL=0` only for an eligible clean-profile child. An active profile, an explicit nonzero
incremental request, or forced incremental compilation keeps Cargo's ordinary path. The doctor reports the exact
toolchain identity without running a build. If that exact identity cannot be captured, Cargo executes normally with
`native_cache_capability_unavailable`. Existing wrappers remain in their selected order and Cargo-Rail does not add a
second cache.

For the exact normal all-workspace `build` and `distribution` shapes, an unambiguous active profile delegates the
unchanged built-in Cargo action before metadata, Git, tool hashing, and action-key construction. Cargo configuration
that makes the target location ambiguous, a non-workspace manifest, or an explicit incremental setting retains the
captured planner/runner path. An eligible clean profile uses one locked, no-dependencies Cargo metadata query to prove
the workspace-library boundary, then enters the same verified compiler-result cache without capturing Git state or
expanding the full action plan. Any ambiguity or acquisition failure falls back to the captured path. Both shortcuts
preserve ambient wrappers and record their deliberately absent snapshot in the ordinary decision receipt.

Default text mode emits one concise decision with `hits`, `misses`, `bypasses`, and `bytes_restored`, or the stable
action-level bypass reason. `--explain` adds `setup_bytes_hashed`, `bytes_hashed`, accounted verified-result
`cache_bytes_read` and `cache_bytes_written` totals, the complete reason census, and per-unit evidence. Low-level I/O
that fails without returning byte statistics is not inferred:

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
cargo rail cache status
cargo rail cache clean --scope workspace --check
cargo rail cache clean --scope local
```

Workspace cleanup removes reconstructible state for the current checkout. Local cleanup resolves the currently
configured cache domain, validates its owner marker, waits for in-flight readers, and removes the cross-workspace CAS.
Use `--scope all` only when both effects are intended. `cargo rail clean --cache` remains a combined compatibility
alias. Do not remove individual CAS objects or Cargo fingerprints by hand.

## Execution and reuse support

Execution support, cache graduation, and performance qualification are independent. A cache bypass still executes
Cargo normally. Native reuse is structural and exact-toolchain-keyed; benchmark qualification reports measured
performance and is not an allowlist.

### Hosts and targets

| Target | Native execution | Cross-target compilation | Release artifact | Native compiler-result cache | Performance qualification |
|---|---|---|---|---|---|
| `aarch64-apple-darwin` | Advertised; full-suite CI required (`macos-15`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units; exact toolchain identity is part of every key | Qualified: check + release-build; 88 accepted, 0 false hits |
| `aarch64-pc-windows-msvc` | Advertised; full-suite CI required (`windows-11-arm`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units; exact toolchain identity is part of every key | Not qualified |
| `aarch64-unknown-linux-gnu` | Advertised; full-suite CI required (`ubuntu-24.04-arm`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units; exact toolchain identity is part of every key | Qualified: check + release-build; 110 accepted, 0 false hits |
| `aarch64-unknown-linux-musl` | Not a native host | Required compatibility build | Cross-built artifact required | Bypass: `cross_target_not_graduated` | Not qualified |
| `thumbv7em-none-eabihf` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` | Not qualified |
| `wasm32-unknown-unknown` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` | Not qualified |
| `wasm32-wasip1` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` | Not qualified |
| `wasm32v1-none` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` | Not qualified |
| `x86_64-apple-darwin` | Advertised; full-suite CI required (`macos-15-intel`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units; exact toolchain identity is part of every key | Not qualified |
| `x86_64-pc-windows-msvc` | Advertised; full-suite CI required (`windows-2022`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units; exact toolchain identity is part of every key | Not qualified |
| `x86_64-unknown-linux-gnu` | Advertised; full-suite CI required (`ubuntu-24.04`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units; exact toolchain identity is part of every key | Qualified: check + release-build; 110 accepted, 0 false hits |
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
| IBM Power | `powerpc64le-unknown-linux-gnu` | Native execution evidence is not retained yet | Structurally active when the exact toolchain identity is captured | Not qualified |
| IBM Z | `s390x-unknown-linux-gnu` | Native execution evidence is not retained yet | Structurally active when the exact toolchain identity is captured | Not qualified |

IBM Power and IBM Z need native runners before Cargo-Rail can claim tested execution or performance. They are not
excluded by a runtime platform allowlist.

### Linkers and codegen backends

| Capability | Advertised non-default implementations | Current contract |
|---|---|---|
| Linker | None | Default and bundled host linkers are tested. An explicitly configured linker keeps the complete action on Cargo's path until Cargo-Rail can preserve its exact argv and effects. |
| Codegen backend | None | Bundled named backends are bound by the complete sysroot identity and compiler arguments. An external backend path bypasses until its executable bytes are content-identified. |

Pass-through execution is not cache graduation. A non-default implementation needs a named compatibility fixture on
every applicable native host before Cargo-Rail advertises it.

### Native compilation classes

| Class | Reuse status | Boundary |
|---|---|---|
| Dependency and workspace library metadata/rlib | Active for any exact, content-identified native toolchain | One declared crate root, complete dep-info-observed inputs, root-independent dep-info, `.rmeta`, optional `.rlib`, Rust-only dependencies, no linker responsibility |
| Incremental compilation | Automatic clean-profile policy | Active fingerprints, explicit nonzero incremental requests, and forced incremental mode preserve Cargo's path; eligible clean profiles run non-incrementally without global setup |
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
just bench-native-cache
```

The latest optimized Linux x86-64 screen used one complete interleaved group on an AWS `c8i.xlarge` with local
sccache `0.16.0`, offline Cargo/rustc `1.97.1`, distinct seed/use roots, and clean target directories. All 20 available
lane samples were accepted with no rejections or false hits. Warm Cargo-Rail restored 28 verified results per workload.

| Final-candidate sample | Cargo-Rail warm | sccache 0.16.0 | Cargo-Rail difference |
|---|---:|---:|---:|
| Check wall time | 4.003 s | 4.777 s | 16.2% lower |
| Check complete CPU | 12.855 s | 15.511 s | 17.1% lower |
| Check peak RSS | 374,759,424 B | 1,076,908,032 B | 65.2% lower |
| Release-build wall time | 6.302 s | 8.081 s | 22.0% lower |
| Release-build complete CPU | 20.131 s | 23.464 s | 14.2% lower |
| Release-build peak RSS | 406,233,088 B | 1,041,481,728 B | 61.0% lower |

This is a one-sample same-machine comparison, not a p50/p95 distribution. The retained qualification corpus below
records the broader historical host coverage and remains the authority for qualified targets.

One local M1 Pro before/after group measured the narrower unit identity and removed redundant warm rehashing. Warm
release-build input hashing fell from 105,219,083 to 68,903,107 bytes (34.5%); warm check input hashing fell from
75,037,802 to 49,793,592 bytes (33.6%). Both groups completed with zero false hits. The single wall-time samples moved
by +4.6% and +8.0%, so this evidence supports the hashing reduction and wider reuse boundary, not a latency claim.

The retained local M1 Pro automatic-policy measurement interleaved 110 lane attempts in 22 complete groups. sccache
`0.16.0` was unavailable, so all 88 available samples were accepted with no rejections or false hits, identical action
censuses and output manifests, and 27 stable eligible units per workload. Every sample used offline Cargo/rustc
`1.97.1`, a clean target, distinct seed/use roots, and a fresh copy of the populated cache. Native Cargo used
`CARGO_INCREMENTAL=0`; Cargo-Rail received no incremental environment setup and selected its clean-profile policy.

Times are p50/p95:

| Host and workload | Native Cargo | Cargo-Rail disabled | Cargo-Rail cold | Cargo-Rail warm | sccache |
|---|---:|---:|---:|---:|---:|
| M1 Pro check | 7.574/8.265 s | 8.012/10.072 s | 10.834/12.427 s | 5.506/6.932 s | — |
| M1 Pro release build | 11.591/12.536 s | 11.822/13.284 s | 14.866/17.018 s | 8.293/13.532 s | — |
| Linux x86-64 check | 5.841/5.891 s | — | — | 3.430/4.074 s | 3.155/3.247 s |
| Linux x86-64 release build | 9.224/9.351 s | — | — | 5.547/5.605 s | 6.223/6.244 s |
| Linux ARM64 check | 6.166/6.266 s | — | — | 3.870/4.325 s | 3.843/3.882 s |
| Linux ARM64 release build | 10.591/10.674 s | — | — | 6.618/6.696 s | 7.470/7.562 s |

On the M1 Pro, warm Cargo-Rail reduced p50 by 27.3% for checks and 28.5% for release builds versus native Cargo. The
fresh CAS grew by 24,424,339 bytes for check and 65,631,693 bytes for release build; warm results restored 24,107,066
and 65,362,456 bytes per sample. Two check reuses or one release-build reuse repaid the p50 cold-publication premium.
Warm Cargo-Rail beat native Cargo at p50 for every qualified host and workload; the M1 Pro release-build p95 remained
slower in this corpus. sccache led the two Linux check workloads and Cargo-Rail led the two Linux release-build
comparisons. These are same-host fixture results, not universal performance claims.

The final 30-sample M1 Pro active-profile follow-up measured unchanged Cargo at 60.2/60.8 ms p50/p95 and the narrow
build delegation at 66.3/70.7 ms. The lean path removed workspace capture, metadata, Git, native-cache setup,
action-key construction, and a false synchronous flush on the observational receipt, reducing the earlier 479.6 ms
cargo-rail median by 86.2%. The corresponding active distribution profile measured Cargo at 59.3/60.3 ms and
cargo-rail at 65.3/66.0 ms. The remaining 6.0–6.1 ms p50 premiums had non-overlapping observed ranges, so
the active-profile policy accepts a 7 ms p50 supervisory budget for exact child exit behavior and a post-success
receipt. The evidence does not justify process replacement, weaker receipt semantics, or broader static bypasses.

Windows x86-64 timing remains unqualified. Its latest partial run found reusable clean-root hits but rejected timing
because two bypassed proc-macro consumers produced byte-unstable DLLs and PID reuse could overwrite evidence files.

When evaluating another workspace, record the repository commit, tool and host/target identities, linker, runner,
wrappers, flags, exact action argv, clean-root method, native/disabled/cold/warm timings, hit and byte counts, all bypass
reasons, and output portability findings. Use one accepted interleaved group for a bounded engineering decision.
Increase the sample count only for distribution or tail claims. Preserve raw output and never combine different
commits, toolchains, targets, or policies.
