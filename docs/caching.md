# Caching

> Auto-generated from executable CI/release registries and native-cache production gates. Do not edit manually.
>
> Regenerate with: `./scripts/docs/generate.sh`. Support manifest schema: `7`;
> native-cache compiler-identity schema: `11`.

Planning removes jobs that do not need to run. Caching removes compiler or analysis work only when Cargo-Rail can
prove that the stored result still matches every relevant input.

| Cache | Purpose | Work skipped on a hit |
|---|---|---|
| Compiler evidence | Reuse `unify` observations after complete input revalidation | Workspace diagnostic collection |
| Native compiler result | Restore one eligible rustc result through Cargo's wrapper boundary | That rustc invocation |

The layers have separate eligibility. A lookup or Cargo `fresh` flag never authorizes reuse. Cargo-Rail revalidates the
input identity, action/result binding, manifest, and exact stored bytes owned by the layer. Incomplete evidence runs
cold with a stable reason. **Fast when proven. Normal Cargo when not.**

## Transparent native compiler-result cache

One machine-local setup enables verified L1 reuse for ordinary Cargo, nextest, Just, IDE, and CI invocations that use
the same effective Cargo home:

```bash
cargo rail cache setup --check
cargo rail cache setup
cargo rail cache status
cargo rail doctor native-cache
```

Setup losslessly sets Cargo's global `build.rustc-wrapper`, installs a private minimal launcher plus compiler-cache
worker, initializes the bounded local CAS, and writes a versioned receipt under the effective Cargo home. The receipt
binds the content and stable file generation of both executables; the worker authenticates itself and its launcher
before acquiring installation context. `--check` previews the exact Cargo field and private paths and exits 1 when
changes are pending. Setup refuses a conflicting wrapper, an environment or workspace configuration that shadows the
global field, an ambiguous Cargo home, linked authority paths, or receipt drift. Repeating setup verifies or repairs
only the same owned authority.

Cargo freshness and incremental compilation remain authoritative L0. Cargo invokes the wrapper only for compiler work
that L0 did not eliminate; Cargo-Rail never restores a target directory or synthesizes fingerprints. Incremental,
Clippy, rustdoc, response-file, information, cross-target, custom-target-directory, native proc-macro consumer,
COFF-linked output, explicitly configured linker, and otherwise unmodeled shapes execute the selected compiler chain
before session or CAS acquisition. Each cold boundary reports its stable missing capability. Exact compiler-owned
metadata, rlib, and static-archive results use L1 only when Cargo-Rail can identify the toolchain, complete bounded
source and native-search namespaces, compiler environment, dependency artifacts, arguments, outputs, and physical
workspace root. Certified default Apple and Linux ELF linker paths additionally admit build-script executables,
proc-macro producers, ordinary binaries, tests, examples, benchmarks, `dylib`, and `cdylib` outputs. Build-script
execution itself remains Cargo-owned cold work.

Every compiler process first enters one pre-Clap boundary that captures Cargo's selected program and byte-exact argv.
Transparent execution preserves the working directory, inherited non-private environment, wrapper order, streams, and
process status; the analysis role adds only its owned lint or observation-output arguments. The boundary distinguishes
the cache wrapper, workspace fact driver, and rustdoc proxy before loading a compiler session. Ambiguous roles fail
before Clap or compiler execution. Information requests, response files, clippy, incremental compilation, unsupported
crate types, and invocations without modeled outputs execute the original chain before session or CAS acquisition.

`CARGO_RAIL_CACHE=off` is the process-local kill switch for an already-selected Cargo-Rail compiler wrapper. The
minimal launcher directly executes the original compiler chain without starting the cache worker, reading the
installation receipt or session state, or opening the CAS. Use this control for deliberate cold baselines.

Each authenticated installation session binds one physical workspace root, and each compiler action additionally
binds the physical source namespace for that unit. Cargo-Rail therefore reuses exact path-bearing compiler artifacts
only within that root; a moved or independent checkout compiles cold and records its own exact variant. Each compiler
unit also binds its exact rustc arguments and cfg, dependency artifact contents, every entry and regular-file byte
below the crate-root directory, and every compiler-visible environment name and value after removing only Cargo-Rail's
exact private controls. Rustc observation then proves that the successful invocation selected no input outside those
capabilities. The action identity is available before lookup; lookup work does not grow with retained source history.

One protocol owns every accepted result. The pre-execution action binds the compiler session, argv, source topology,
environment, dependencies, generated inputs, and native-search state. Linked Linux ELF actions use that value only as
a non-authoritative candidate selector until the cold linker witness closes found and missing lookup paths, driver
and linker bytes, platform tool inputs, exact dependency archives, and rustc-endogenous objects. The witnessed action
then binds one result descriptor and exact output/stream objects. L1 verifies the same action, witness, result
association, and objects at restore time. L2 transports the same pack and cannot redefine its authority.

Linux ELF certification is similarly bounded. Cargo-Rail resolves the installed default `cc` driver and its selected
linker, requires GNU-compatible dependency-file evidence, and records direct driver inputs, selected auxiliary tools,
and driver/linker search directories. The cold witness binds every reported link input, closes relevant search
namespaces with found and missing same-name candidates, and treats only rustc-generated objects under the exact linked
output directory as endogenous. A changed driver, linker, tool, input, or symlink; an appeared negative lookup;
missing dependency evidence; no certified rustc object; or a non-byte-stable result runs cold and receives no linked
cache authority.

The cache deliberately over-invalidates when an unused file in the bounded source directory changes. This is the
smallest sound contract for Rust's path discovery: positive dep-info alone does not record failed probes such as the
choice between `foo.rs` and `foo/mod.rs`. A source symlink, unsupported file kind, incomplete capture, mutation during
capture, or exceeded entry, byte, depth, or time bound runs the original compiler instead of weakening that proof.

The session does not bind the complete Cargo configuration or `Cargo.lock`.
Changes to `build.warnings`, jobs, build or target directories, network policy, registry settings, and unrelated
lockfile entries can therefore reuse a result when those exact unit inputs remain unchanged. Rust flags, features,
dependency contents, target, linker, sysroot, source topology, and compiler environment still change or reject reuse
at their owning boundary.

Filesystem reads include files used through `include!`, `include_str!`, and `include_bytes!`. Rust metadata can contain
source roots, so Cargo-Rail never injects compiler remapping into a cache-requested invocation. The CAS uses reversible
tokens only for verified dep-info and JSON output-path bindings, including their Windows separator and escaping form,
then restores the current output paths after verification. Source-root authority remains physical and root-bound;
output names and materialized bytes remain exact, and ambiguous or unmodeled cached paths fail closed.
Cargo-Rail never changes incremental policy. An active or requested incremental invocation stays on Cargo's ordinary
path. The doctor reports the exact compiler identity and installation health without running a build. An existing
`rustc-workspace-wrapper` remains in Cargo's selected chain; Cargo-Rail bypasses reuse for that composition. Setup
refuses an existing `rustc-wrapper`, and the compiler boundary rejects ambiguous or recursive wrapper chains.

`cache status` reports installation health, Cargo L0 ownership, a bounded best-effort usage ledger, and exact CAS
ownership. Acquisition-free early bypasses are reported separately and do not touch session or CAS state. The ledger
is operational evidence, not reuse authority:

- `hit` means current inputs and every stored object were reverified before exact output bytes were restored;
- `miss` means the exact action has no authoritative result; successful cold output may populate the local CAS; and
- `bypass` means the session or invocation is outside the graduated class and rustc executed normally.

The corrected local store resolves an exact action to zero or one authoritative result. A second semantic result for
the same action becomes a durable conflict; malformed or transaction-ambiguous state becomes quarantined. Neither
state restores cached output. Restore uses one durable marker for the exact destination set. Failure before the first
visible replacement falls back to the original compiler; failure after that boundary cleans every owned destination
and fails without running rustc over a partial commit.

Corrupt or incompatible native entries never authorize reuse. Missing installation state, an unavailable CAS, or
incomplete pre-commit evidence executes cold. A failure after the first visible restore replacement fails closed so
rustc never runs over a partial restored output set.

## Shared native cache (L2)

Repository `[cache]` configuration is rejected because a checkout must not select a network write destination. Persist
one machine-owned authority during the same setup used for L1, then run ordinary Cargo without wrapper commands or
Cargo-Rail environment variables:

```bash
cargo rail cache setup --check --remote \
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012' \
  --remote-mode read-write
cargo rail cache setup --remote \
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012' \
  --remote-mode read-write
cargo build --workspace --all-features --locked
```

`loadingalias/cargo-rail-action/cache@v6` accepts the same URL as `url`, runs setup once, and leaves later ordinary
Cargo commands in that GitHub Actions job on the installed cache path. Configure provider credentials before Cargo
runs and invoke the cache Action separately in each execution job because hosted jobs do not share a machine or Cargo
home.

Validate or canonicalize a URL without resolving credentials or contacting storage:

```bash
cargo rail cache normalize \
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012'
```

The accepted authority families are:

```text
s3://BUCKET/PREFIX?region=REGION&owner=AWS_ACCOUNT_ID
r2://ACCOUNT_ID/BUCKET/PREFIX
azure://ACCOUNT/CONTAINER/PREFIX
```

Official S3 requires the expected 12-digit owner. R2 derives its documented account endpoint and region `auto`; Azure
derives the public Blob endpoint from the storage-account name. User information, fragments, encoded separators, dot
segments, duplicate or unknown query keys, ambiguous ports, and credentials are rejected before identity.

```bash
cargo rail cache status --scope local --format json
cargo rail doctor native-cache --format json
```

`--remote-mode` accepts `read` or `read-write` and defaults to `read-write` for an explicit selection. Read mode
requires an existing compatible protocol marker and never writes. Read-write mode conditionally creates that marker
and one compressed, bounded entry per base action. The entry binds the selected environment names, exact action,
result identity, and verified result pack. A second distinct identity conditionally replaces it with a terminal
metadata-only conflict. Build clients use only object reads and conditional writes; they do not list or delete objects.
`--local-only` removes persisted L2 activation without removing L1. The environment variables remain transient
overrides for qualification and advanced automation.

L1 remains authoritative. A verified L1 hit makes no remote request. Only an L1 miss contacts a short-lived private
loopback coordinator. It shares one AWS SDK runtime, credential resolution, client, and connection pool across rustc
processes, holds no build-result memory cache, and exits after bounded inactivity. Coordinator startup or IPC failure
falls back to the direct transport; every remote failure still compiles cold. An empty-L1 hit needs one entry GET after
the coordinator's protocol check. Downloaded packs must match the base action, selected environment names, exact
action, declared length, canonical descriptor, result identity, every payload digest, current compiler invocation,
source capture, and reviewed environment-name policy before L1 admits them.

Official S3 URLs require a region and expected 12-digit bucket-owner account. Cargo-Rail pins both on every object
operation and rejects configured endpoint overrides for official AWS authority. Credentials remain outside URLs,
configuration, result packs, compiler arguments, diagnostics, and cache keys. A domain-separated digest isolates live
coordinators by credential authority without storing raw credential values. Cargo-Rail removes remote selection and
credential environment from every compiler child it launches. Prefer a machine/container role, OIDC, or a
preconfigured profile; keep exported credentials job-scoped and least-privilege when a provider requires them.

`CARGO_RAIL_CACHE_REMOTE_ENVIRONMENT` may add a sorted comma-separated set of reviewed, non-secret compiler
environment names to the built-in `CARGO_PKG_NAME` and `OUT_DIR` policy. Only value digests enter action identity; raw
values are never uploaded. An unapproved name bypasses L2 for that compiler unit without disabling an existing valid
L1 result.

Status schema 10 reports provider, protocol, mode, approved-name count, and a redacted authority identity as
`direct_transport_selected`; it never prints the URL. Status and doctor remain network-free.

AWS S3 and R2 use the same bounded conditional-object protocol; Azure Blob Storage implements the same cache protocol
through its native conditional operations. Cleartext transport exists only for deterministic loopback protocol
fixtures and is not a supported remote provider. No remote superiority claim is made until retained real-backend
measurements satisfy the benchmark contract.

## Compiler-evidence cache

`cargo rail unify --check` may reuse compiler observations after revalidating the compiler, source, manifest, target,
features, Cargo configuration, dependency artifacts, emitted outputs, executable identity, and recorded environment
reads. This store contains diagnostic evidence, not restorable Cargo artifacts.

An explicitly launched analysis run creates one private fact capability bound to its exact source root and observation
directory. Rustc and rustdoc publish only while that capability validates. An absent capability bypasses fact
collection and preserves the original compiler; an incomplete, moved, or tampered capability fails the analysis run
instead of silently accepting partial evidence. Fact identity remains separate from native cache action/result
identity.

Check mode does not edit manifests, but analysis may update cache and report files under `target/cargo-rail/`.
`cargo rail unify --check -f json` exposes `evidence_cache` hits, misses, and reasons.

## Storage and cleanup

Setup records the exact local authority base and positive byte bound in its receipt. The default base is the effective
Cargo home and the default result bound is 10 GiB; `--local-dir` and `--max-size` select explicit alternatives. Runtime
environment variables do not override the receipt, so setup, status, compiler reuse, and cleanup share one authority.
The private CAS validates its opaque owner marker before every use and never follows linked authority paths.

```bash
cargo rail cache setup --check
cargo rail cache status
cargo rail cache clean --scope workspace --check
cargo rail cache clean --scope local --check
cargo rail cache remove --check
```

Workspace cleanup removes reconstructible state for the current checkout. Local cleanup resolves only the currently
installed receipt authority, validates its owner marker, waits for in-flight readers, and removes that cross-workspace
CAS. Removing active L1 leaves setup drift that the next cold compiler invocation bypasses safely; `cache setup`
repairs the same root. `cache remove` losslessly removes only the receipt-owned Cargo field, wrapper, session state, and
receipt; it preserves CAS data. It refuses changed or unowned state. A legacy `local-cas-v1` is reclaim-only and never
becomes v2 authority. Use `--scope all` only when both cache-cleanup effects are intended. `cargo rail clean --cache`
remains a combined compatibility alias. Do not remove individual CAS objects or Cargo fingerprints by hand.

## Execution and reuse support

Execution support and cache reuse are independent. A cache bypass still executes Cargo normally. Native reuse is
structural and exact-toolchain-keyed.

### Hosts and targets

| Target | Native execution | Cross-target compilation | Release artifact | Native compiler-result cache |
|---|---|---|---|---|
| `aarch64-pc-windows-msvc` | Advertised; full-suite CI required (`windows-11-arm`) | — | Native artifact required | Active for structurally eligible `exact_rustc_result` units; exact compiler identity is part of every key |
| `aarch64-unknown-linux-gnu` | Advertised; full-suite CI required (`ubuntu-24.04-arm`) | — | Native artifact required | Active for structurally eligible `exact_rustc_result` units and certified default-ELF-linker producers/final artifacts; exact compiler identity is part of every key |
| `aarch64-unknown-linux-musl` | Not a native host | Required compatibility build | Cross-built artifact required | Bypass: `cross_target_toolchain_evidence_unavailable` |
| `thumbv7em-none-eabihf` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_toolchain_evidence_unavailable` |
| `wasm32-unknown-unknown` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_toolchain_evidence_unavailable` |
| `wasm32-wasip1` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_toolchain_evidence_unavailable` |
| `wasm32v1-none` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_toolchain_evidence_unavailable` |
| `x86_64-pc-windows-msvc` | Advertised; full-suite CI required (`windows-2022`) | — | Native artifact required | Active for structurally eligible `exact_rustc_result` units; exact compiler identity is part of every key |
| `x86_64-unknown-linux-gnu` | Advertised; full-suite CI required (`ubuntu-24.04`) | — | Native artifact required | Active for structurally eligible `exact_rustc_result` units and certified default-ELF-linker producers/final artifacts; exact compiler identity is part of every key |
| `x86_64-unknown-linux-musl` | Not a native host | Required compatibility build | Cross-built artifact required | Bypass: `cross_target_toolchain_evidence_unavailable` |

Linux musl rows are release cross-builds, not native Linux host evidence.

### Filesystems

| Profile | Runner | Filesystem | Case behavior | Required evidence |
|---|---|---|---|---|
| Default `aarch64-pc-windows-msvc` | `windows-11-arm` | `ntfs` | Insensitive | Full endpoint suite and native probe |
| Default `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | `ext4` | Sensitive | Full endpoint suite and native probe |
| Default `x86_64-pc-windows-msvc` | `windows-2022` | `ntfs` | Insensitive | Full endpoint suite and native probe |
| Default `x86_64-unknown-linux-gnu` | `ubuntu-24.04` | `ext4` | Sensitive | Full endpoint suite and native probe |
| linux-tmpfs | `ubuntu-24.04` | `tmpfs` | Sensitive | Front-door corpus, CAS/atomicity suite, ENOSPC, and cleanup |
| windows-ntfs-vhd | `windows-2022` | `ntfs` | Insensitive | Front-door corpus, CAS/atomicity suite, ENOSPC, and cleanup |

Alternate profiles use bounded temporary volumes and detach them after success or failure.

### Deferred native hosts

| Platform | Target | Execution status | Cache status |
|---|---|---|---|
| IBM Power | `powerpc64le-unknown-linux-gnu` | Blocked: `native_powerpc64le_hardware_access_unavailable` | Structurally active when the exact compiler identity is captured |
| RISC-V 64 | `riscv64gc-unknown-linux-gnu` | Blocked: `native_riscv64gc_hardware_access_unavailable` | Structurally active when the exact compiler identity is captured |
| IBM Z | `s390x-unknown-linux-gnu` | Blocked: `native_s390x_hardware_access_unavailable` | Structurally active when the exact compiler identity is captured |

IBM Power, IBM Z, and RISC-V need native hardware before Cargo-Rail can claim tested execution. Each row retains the
exact hardware-access gate; none is excluded by a runtime platform allowlist.

### Open Task 9 evidence gates

| Scope | Target or provider | Required evidence gate |
|---|---|---|
| Native host | `aarch64-apple-darwin` | `task9_aarch64_apple_darwin_cache_off_performance_incomplete` |
| Native host | `aarch64-pc-windows-msvc` | `task9_aarch64_pc_windows_msvc_native_evidence_unavailable` |
| Native host | `aarch64-unknown-linux-gnu` | `task9_aarch64_unknown_linux_gnu_native_evidence_unavailable` |
| Native host | `powerpc64le-unknown-linux-gnu` | `native_powerpc64le_hardware_access_unavailable` |
| Native host | `riscv64gc-unknown-linux-gnu` | `native_riscv64gc_hardware_access_unavailable` |
| Native host | `s390x-unknown-linux-gnu` | `native_s390x_hardware_access_unavailable` |
| Native host | `x86_64-apple-darwin` | `task9_x86_64_apple_darwin_native_evidence_unavailable` |
| Native host | `x86_64-pc-windows-msvc` | `task9_x86_64_pc_windows_msvc_native_evidence_unavailable` |
| Native host | `x86_64-unknown-linux-gnu` | `task9_x86_64_unknown_linux_gnu_native_evidence_unavailable` |
| Remote provider | `aws-s3` | `task9_aws_s3_correctness_evidence_unavailable`, `task9_aws_s3_performance_evidence_unavailable` |
| Remote provider | `azure-blob` | `task9_azure_blob_correctness_evidence_unavailable` |
| Remote provider | `cloudflare-r2` | `task9_cloudflare_r2_correctness_evidence_unavailable` |

These rows are explicit completion blockers, not unsupported-target declarations. Remove a gate only after retaining
the exact native or real-provider corpus required by the Task 9 contract.

### Linkers and codegen backends

| Capability | Advertised non-default implementations | Current contract |
|---|---|---|
| Linker | None | The default Apple driver/linker chain and a Linux `cc`-selected ELF linker with GNU-compatible dependency-file evidence are certified. Windows COFF linking reports `coff_linker_evidence_unavailable`; explicit linker selection or arguments report their own evidence boundary and execute unchanged. |
| Codegen backend | None | Bundled named backends are bound by the complete sysroot identity and compiler arguments. An external backend path bypasses until its executable bytes are content-identified. |

Pass-through execution is not cache graduation. A non-default implementation needs a named compatibility fixture on
every applicable native host before Cargo-Rail advertises it.

### Native compilation classes

| Class | Reuse status | Boundary |
|---|---|---|
| Dependency and workspace library metadata/rlib | Active for any exact, content-identified native toolchain | One physical-root-bound session and unit source namespace, complete bounded source topology and bytes, exact compiler environment, containment observation, `.rmeta`, optional `.rlib`, dependency contents, and exact native-static search namespaces |
| Incremental compilation | Bypassed before session or CAS acquisition | Cargo owns freshness and incremental policy; no stable compiler-owned validation interface exists for transported work products (`moved_root_compiler_work_product_validation_unavailable`) |
| Certified linked producers and final binaries | Active for the default Apple chain and a dependency-file-capable default ELF linker on Linux | One typed platform witness binds found/missing lookup namespaces, driver/linker and tool bytes, symlink resolution, dependency archives, certified rustc-generated objects, and byte-stable linked output; COFF and unknown providers retain explicit cold reasons |
| Tests, examples, and benchmarks | Active through a certified Apple or Linux ELF linker | Test harnesses are typed executable results; exact linker evidence and the normal compiler action authority bind their output |
| `dylib` and `cdylib` | Active through certified Apple or Linux ELF linking | COFF, custom, cross, signing, and unobserved post-link boundaries remain cold |
| `staticlib` | Active as a compiler-owned archive result | Rustc's archive builder owns the operation; the action binds source, upstream Rust/native archives, toolchain/backend, arguments, environment, and target, then validates the exact dep-info and archive bytes |
| Proc-macro producers | Metadata-only producer `.rmeta` is active directly; the producer dylib is active through certified Apple or Linux ELF linking | Neither result certifies later macro execution |
| Native proc-macro consumers | Bypassed before context acquisition | Compile-time filesystem, environment, process, network, clock, and randomness reads are not completely observed; unsafe sccache hits are not copied |
| Build-script executable compilation | Active through certified Apple or Linux ELF linking | The compiler result is reusable; build-script execution and its generated outputs remain Cargo-owned cold work |
| Native dependencies and `links` contracts | Exact native-static consumers may reuse metadata/rlib; native tools execute cold as typed child operations | The coverage graph identifies compiler probes, native compilation, assembly/preprocessing, archive steps, outputs, and downstream Rust consumers; incomplete dependency files, inherited environment, and archive mutation transactions retain specific cold reasons |
| Clippy diagnostics | Bypassed; Clippy executes | The compiler-mode ledger requires `clippy_diagnostic_result_authority_unavailable` before cache or remote acquisition |
| rustdoc and doctests | Bypassed; rustdoc/test executes | Stable Cargo output does not enumerate the complete documentation tree, and doctest execution is a separate result authority; the compiler-mode ledger proves both cold boundaries |
| Cross compilation and custom targets | Bypassed; compiler executes | Host/target tools, runners, SDKs, and target specifications are not graduated |
| Existing workspace wrapper | Preserved; Cargo-Rail reuse bypassed | The selected wrapper chain remains authoritative and is never double-cached; ambiguous and recursive chains are rejected |
| Existing global or environment wrapper | Setup refused or shadowed | Setup never silently replaces another `rustc-wrapper` authority |
| Custom Cargo target directory | Bypassed; Cargo executes | The wrapper cannot prove one physical workspace root from the standard `<root>/target` output layout |
| Per-invocation Cargo CLI `--config` | May shadow setup for that invocation | Cargo precedence remains authoritative; status and setup detect persistent environment and workspace shadowing |

## Benchmark evidence

The fixture contains registry and Git dependencies, build scripts, a proc macro, native code, a compiler-owned static
archive, Rust and C dynamic libraries, workspace libraries, binaries, tests, an example, and a benchmark target:

```bash
just bench-native-cache-smoke
just bench-native-cache 20
```

The current workflow invokes check, release-build, and test-target compilation directly under one isolated setup
receipt. It measures intact-target L0 overhead, empty-target L1 restoration, and cold-path overhead independently. The
comparison rotates lane order, preserves raw samples, records tool/host/configuration identity and usage counters, and
checks exact outputs before accepting a sample. The final report requires every compiler class represented by the
check/build/test corpus, so a fast subset cannot hide cold crate types. A separate compiler-mode ledger proves
acquisition-free Clippy, rustdoc, and doctest execution.

The canonical performance corpus contains 20 accepted interleaved samples per lane. Shared-hit superiority qualifies
only when transparent empty-target L1 is at least 10% faster than the pinned local `sccache` lane at both p50 and p95;
the target is 15%. The broader performance result also requires Cargo L0 and cache-off p95 overhead within the declared
bound and clean correctness and coverage acceptance. A failed corpus is a valid result: retain it and do not claim the
failed property. One accepted group is only a workflow smoke test; it is not performance qualification.

When evaluating another workspace, record the repository commit, tool and host/target identities, wrappers, flags,
target-state policy, disabled/cold/warm/sccache timings, hit and byte counts, bypass reasons, and byte-identity findings.
Never combine different commits, toolchains, targets, cache protocols, policies, or machines into one timing
population.
