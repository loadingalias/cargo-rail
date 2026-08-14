# Caching

> Auto-generated from executable CI/release registries and native-cache production gates. Do not edit manually.
>
> Regenerate with: `./scripts/docs/generate.sh`. Support manifest schema: `5`;
> native-cache compiler-identity schema: `9`.

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
clippy, rustdoc, response-file, information, test-mode, cross-target, custom-target-directory, native proc-macro
consumer, explicitly configured linker, and otherwise unsupported shapes execute the selected compiler chain before
session or CAS acquisition. Eligible metadata/rlib units, including exact native-static consumers, use L1 only when
Cargo-Rail can identify the exact toolchain, complete bounded source and native-search namespaces, compiler
environment, dependency artifacts, arguments, outputs, and physical workspace root. On macOS, the graduated default
Apple-linker path also admits build-script executables, proc-macro producers, ordinary final binaries, `dylib`, and
`cdylib` outputs after linker certification. Build-script execution itself is never cached.

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
environment, dependencies, generated inputs, and native-search state. Linked Apple actions use that value only as a
non-authoritative candidate selector until the cold linker certificate closes found and missing lookup paths, driver
and linker bytes, SDK inputs, exact dependency archives, and rustc-endogenous objects. The witnessed action then binds
one result descriptor and exact output/stream objects. L1 verifies the same action, witness, result association, and
objects at restore time. L2 transports the same pack and cannot redefine its authority.

Apple certification is deliberately narrow. Cargo-Rail invokes the selected default `/usr/bin/cc` chain, asks the
installed Apple linker for dependency evidence, and records direct driver inputs in a private sidecar. For LTO, the
adapter snapshots owner-controlled rustc temporary namespaces that are not writable by group or other users before
driver execution, certifies new regular objects reported by the linker before those namespaces disappear, and treats
only non-user-selected temporary aggregate archives passed by the exact rustc driver as endogenous. Cargo-Rail uses
`-oso_prefix` only for that certified random temporary prefix. A user link argument, custom linker, unknown certificate
record, preexisting or unbound temporary object/archive, appeared negative lookup, changed symlink resolution,
SDK/tool change, or non-byte-stable result runs cold and receives no linked cache authority.

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
| `aarch64-apple-darwin` | Advertised; full-suite CI required (`macos-15`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units and certified default-Apple-linker producers/final artifacts; exact compiler identity is part of every key |
| `aarch64-pc-windows-msvc` | Advertised; full-suite CI required (`windows-11-arm`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units; exact compiler identity is part of every key |
| `aarch64-unknown-linux-gnu` | Advertised; full-suite CI required (`ubuntu-24.04-arm`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units; exact compiler identity is part of every key |
| `aarch64-unknown-linux-musl` | Not a native host | Required compatibility build | Cross-built artifact required | Bypass: `cross_target_not_graduated` |
| `thumbv7em-none-eabihf` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` |
| `wasm32-unknown-unknown` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` |
| `wasm32-wasip1` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` |
| `wasm32v1-none` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` |
| `x86_64-apple-darwin` | Advertised; full-suite CI required (`macos-15-intel`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units and certified default-Apple-linker producers/final artifacts; exact compiler identity is part of every key |
| `x86_64-pc-windows-msvc` | Advertised; full-suite CI required (`windows-2022`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units; exact compiler identity is part of every key |
| `x86_64-unknown-linux-gnu` | Advertised; full-suite CI required (`ubuntu-24.04`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units; exact compiler identity is part of every key |
| `x86_64-unknown-linux-musl` | Not a native host | Required compatibility build | Cross-built artifact required | Bypass: `cross_target_not_graduated` |

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

| Platform | Target | Execution status | Cache status |
|---|---|---|---|
| IBM Power | `powerpc64le-unknown-linux-gnu` | Native execution evidence is not retained yet | Structurally active when the exact compiler identity is captured |
| IBM Z | `s390x-unknown-linux-gnu` | Native execution evidence is not retained yet | Structurally active when the exact compiler identity is captured |

IBM Power and IBM Z need native runners before Cargo-Rail can claim tested execution. They are not excluded by a
runtime platform allowlist.

### Linkers and codegen backends

| Capability | Advertised non-default implementations | Current contract |
|---|---|---|
| Linker | None | The installed default Apple driver/linker is certified for the explicit macOS linked class. Other defaults remain pass-through for linked outputs; an explicitly configured linker always keeps the complete linked action on Cargo's path. |
| Codegen backend | None | Bundled named backends are bound by the complete sysroot identity and compiler arguments. An external backend path bypasses until its executable bytes are content-identified. |

Pass-through execution is not cache graduation. A non-default implementation needs a named compatibility fixture on
every applicable native host before Cargo-Rail advertises it.

### Native compilation classes

| Class | Reuse status | Boundary |
|---|---|---|
| Dependency and workspace library metadata/rlib | Active for any exact, content-identified native toolchain | One physical-root-bound session and unit source namespace, complete bounded source topology and bytes, exact compiler environment, containment observation, `.rmeta`, optional `.rlib`, dependency contents, and exact native-static search namespaces |
| Incremental compilation | Bypassed before session or CAS acquisition | Cargo owns freshness and incremental policy; Cargo-Rail never disables incremental compilation |
| Certified Apple linked producers and final binaries | Active for the installed default Apple driver/linker on macOS | The witnessed action binds the linker certificate's found/missing namespace, driver/linker and SDK bytes, symlink resolution, dependency archives, certified rustc-generated objects, and byte-stable linked output |
| Tests and unsupported binary/example/benchmark shapes | Bypassed; compiler/linker executes | Test mode and shapes outside the explicit Apple linked class are not graduated |
| `dylib` and `cdylib` | Structurally admitted only through certified Apple linking | Other native linker, SDK, runtime, and post-link boundaries remain incomplete |
| `staticlib` | Bypassed; compiler/archive creation executes | Archive creation is a distinct deterministic boundary and is not covered by native-static consumption evidence |
| Proc-macro producers | Metadata-only producer `.rmeta` is active directly; the producer dylib is active only through certified Apple linking | Neither result certifies later macro execution |
| Native proc-macro consumers | Bypassed before context acquisition | Compile-time filesystem, environment, process, network, clock, and randomness reads are not completely observed; unsafe sccache hits are not copied |
| Build-script executable compilation | Active only through certified Apple linking | The compiler result is reusable; build-script execution and its generated outputs remain Cargo-owned cold work |
| Native dependencies and `links` contracts | Exact native-static consumers may reuse metadata/rlib; native tools still execute | Search order, modifiers, missing candidates, shadowing, replacement, symlinks, archives, and generated namespaces are bound; external tool execution is not cached |
| rustdoc and doctests | Bypassed; rustdoc/test executes | Stable Cargo output does not enumerate the complete documentation tree |
| Cross compilation and custom targets | Bypassed; compiler executes | Host/target tools, runners, SDKs, and target specifications are not graduated |
| Existing workspace wrapper | Preserved; Cargo-Rail reuse bypassed | The selected wrapper chain remains authoritative and is never double-cached; ambiguous and recursive chains are rejected |
| Existing global or environment wrapper | Setup refused or shadowed | Setup never silently replaces another `rustc-wrapper` authority |
| Custom Cargo target directory | Bypassed; Cargo executes | The wrapper cannot prove one physical workspace root from the standard `<root>/target` output layout |
| Per-invocation Cargo CLI `--config` | May shadow setup for that invocation | Cargo precedence remains authoritative; status and setup detect persistent environment and workspace shadowing |

## Benchmark evidence

The fixture contains registry and Git dependencies, build scripts, a proc macro, native code, workspace libraries, and
a binary:

```bash
just bench-native-cache-smoke
just bench-native-cache 5
```

The current workflow invokes Cargo directly under one isolated setup receipt and measures three different questions
independently: intact-target L0 overhead,
empty-target L1 restoration, and cold-path overhead. The comparison rotates lane order, preserves raw samples, records
tool/host/configuration identity and usage counters, and checks exact outputs before accepting a sample.

The canonical performance corpus contains five accepted interleaved samples per lane. It qualifies only when
transparent empty-target L1 is strictly faster than the pinned local `sccache` lane at both p50 and p95, Cargo L0 and
cache-off p95 overhead are within the declared bound, and correctness and coverage acceptance are clean. A failed
corpus is a valid result: retain it and do not claim superiority. One accepted group is only a workflow smoke test; it
is not performance qualification.

When evaluating another workspace, record the repository commit, tool and host/target identities, wrappers, flags,
target-state policy, disabled/cold/warm/sccache timings, hit and byte counts, bypass reasons, and byte-identity findings.
Never combine different commits, toolchains, targets, cache protocols, policies, or machines into one timing
population.
