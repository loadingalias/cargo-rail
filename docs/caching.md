# Caching

> Auto-generated from executable CI/release registries and native-cache production gates. Do not edit manually.
>
> Regenerate with: `./scripts/docs/generate.sh`. Support manifest schema: `5`;
> native-cache toolchain-identity schema: `5`.

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
- the invocation is an eligible dependency or workspace library whose complete bounded source namespace, exact
  compiler-visible environment, selected-input proof, metadata, optional rlib output, and Rust-only dependency
  artifacts remain inside the graduated class, with no linker responsibility.

```bash
cargo rail run --all --action build --explain
cargo rail run --all --action distribution --explain
cargo rail doctor native-cache --format json
```

Each command session validates one physical source root. Reusable action and result identities replace that root with
a versioned portable root, so equivalent source, toolchain, environment, dependency, target, and output evidence can
reuse across arbitrary checkout roots. Each compiler unit binds its exact rustc arguments and cfg, dependency artifact
contents, every entry and regular-file byte below the crate-root directory, and every compiler-visible environment
name and value after removing only Cargo-Rail's exact private controls. Rustc observation then proves that the
successful invocation selected no input outside those capabilities. The action identity is available before lookup;
lookup work does not grow with retained source history.

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
source and output roots, so eligible cold invocations use a versioned compiler remap. The CAS stores reversible tokens
for verified dep-info and JSON compiler-stream paths, including their Windows separator and escaping form, then binds
them to the current source and output roots after verification. Output names and materialized bytes remain exact;
ambiguous root spellings or unmodeled cached paths fail closed.
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
- `miss` means the exact action has no authoritative result; successful cold output may populate the local CAS; and
- `bypass` means the session or invocation is outside the graduated class and rustc executed normally.

The corrected local store resolves an exact action to zero or one authoritative result. A second semantic result for
the same action becomes a durable conflict; malformed or transaction-ambiguous state becomes quarantined. Neither
state restores cached output. Restore uses one durable marker for the exact destination set. Failure before the first
visible replacement falls back to the original compiler; failure after that boundary cleans every owned destination
and fails without running rustc over a partial commit.

Corrupt or incompatible native entries never authorize reuse. Cargo still owns its fingerprints; Cargo-Rail never
restores a target directory or synthesizes Cargo freshness. Use `--no-cache` for an intentional cold baseline.

## Shared native cache (L2)

Select one machine target by alias; L1 remains enabled by default:

```toml
[cache]
l2 = "team"
```

An accepted L1 hit is network-free. On an L1 miss, the command-owned coordinator can stream one S3 result into private
L1 staging. The ordinary L1 proof must accept the imported result before restore. A `read_write` target publishes a
verified local pack before its action association. The compiler wrapper receives only a loopback capability, never S3
credentials or a bucket name.

Set `CARGO_RAIL_CACHE_TARGETS_FILE` to an absolute machine-owned JSON file outside the checkout. This is the complete
version 1 schema; unknown fields are rejected:

```json
{
  "version": 1,
  "targets": {
    "team": {
      "protocol": "s3",
      "region": "us-east-1",
      "expected_bucket_owner": "123456789012",
      "bucket": "company-cargo-rail-cache",
      "prefix": "rust/team",
      "role": "read_write",
      "shareable_environment": ["CARGO", "CARGO_CRATE_NAME", "LANG", "PATH"]
    }
  }
}
```

Aliases use lowercase ASCII letters, digits, `-`, and `_`, and start with a letter. `read` permits `GetObject` only;
`read_write` also permits conditional `PutObject`. Restrict the AWS principal to the selected bucket prefix.
`shareable_environment` is a sorted, unique list of non-secret compiler-environment names that may participate in L2
reuse. An action with any other selected environment name stays local.

Before granting client access, create `<prefix>/native-v3/protocol` with the exact 27-byte body
`cargo-rail-native-cache-v3
`. Keep this marker and selector/action objects permanent; apply expiration only to
`<prefix>/native-v3/results/`. Require TLS and reject every `PutObject` that has neither `If-Match` nor
`If-None-Match`. Clients need no list, delete, ACL, bucket-policy, or lifecycle authority. The marker is required
because S3 reports a missing object as `403` when a caller has `GetObject` but not `ListBucket`; Cargo-Rail validates
the marker before treating `403` on a content-addressed cache key as a miss.

The command parent loads credentials from the standard AWS SDK chain. The target map contains no credentials.
Cargo-Rail pins the expected bucket owner and the AWS SDK's official regional HTTPS endpoint; custom endpoints and
S3-compatible services are not supported. Endpoint overrides, access points, multi-region access points, S3 Express,
FIPS, dual-stack, and accelerated endpoints are disabled or rejected.

Before starting Cargo, an L2-enabled command removes the target-map variable, AWS credential and provider-selector
variables, and AWS endpoint-override variables from the child environment. This prevents accidental inheritance; it
is not a sandbox. A same-UID build script or proc macro can still read default AWS credential files or contact a
workload metadata service. Run untrusted builds under a principal or container with no L2 credentials, or omit
`cache.l2`.

```bash
cargo rail cache status --scope local --format json
cargo rail doctor native-cache --format json
```

`cache status` validates the selected target without resolving credentials. `doctor native-cache` resolves
credentials and validates the immutable protocol marker. A missing target, unavailable credentials, authentication error,
timeout, service failure, or remote miss compiles cold through L1 and opens one command-local remote circuit when
applicable. A remote conflict, malformed object, or action/result mismatch is an integrity failure and restores
nothing. Publication failure is reported after local admission and does not change successful compilation.

The S3 protocol and local coordinator are implemented, but live cross-host S3 reuse has not yet been retained as
release evidence.

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

The default local authority root is `$CARGO_HOME/cargo-rail/local-cas-v2` or
`$HOME/.cargo/cargo-rail/local-cas-v2`. `CARGO_RAIL_CACHE_DIR` selects another machine-authorized base;
`CARGO_RAIL_CACHE_MAX_BYTES` changes the positive 10 GiB result bound. The owner directory contains a generated opaque
trust-domain marker, and the CAS validates the matching root marker before every use. An explicit
`CARGO_RAIL_CACHE_TRUST_DOMAIN` selects `local-cas-v2-<id>` so protected CI, untrusted jobs, and managed interactive
workloads can use physically isolated roots. The variable names authority; it is not isolation by itself.

```bash
cargo rail cache status
cargo rail cache clean --scope workspace --check
cargo rail cache clean --scope local
```

Workspace cleanup removes reconstructible state for the current checkout. Local cleanup resolves only the currently
selected trust domain, validates its owner marker, waits for in-flight readers, and removes that cross-workspace CAS.
A legacy `local-cas-v1` is reclaim-only and never becomes v2 authority. Use `--scope all` only when both effects are
intended. `cargo rail clean --cache` remains a combined compatibility
alias. Do not remove individual CAS objects or Cargo fingerprints by hand.

## Execution and reuse support

Execution support and cache reuse are independent. A cache bypass still executes Cargo normally. Native reuse is
structural and exact-toolchain-keyed.

### Hosts and targets

| Target | Native execution | Cross-target compilation | Release artifact | Native compiler-result cache |
|---|---|---|---|---|
| `aarch64-apple-darwin` | Advertised; full-suite CI required (`macos-15`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units; exact toolchain identity is part of every key |
| `aarch64-pc-windows-msvc` | Advertised; full-suite CI required (`windows-11-arm`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units; exact toolchain identity is part of every key |
| `aarch64-unknown-linux-gnu` | Advertised; full-suite CI required (`ubuntu-24.04-arm`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units; exact toolchain identity is part of every key |
| `aarch64-unknown-linux-musl` | Not a native host | Required compatibility build | Cross-built artifact required | Bypass: `cross_target_not_graduated` |
| `thumbv7em-none-eabihf` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` |
| `wasm32-unknown-unknown` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` |
| `wasm32-wasip1` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` |
| `wasm32v1-none` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` |
| `x86_64-apple-darwin` | Advertised; full-suite CI required (`macos-15-intel`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units; exact toolchain identity is part of every key |
| `x86_64-pc-windows-msvc` | Advertised; full-suite CI required (`windows-2022`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units; exact toolchain identity is part of every key |
| `x86_64-unknown-linux-gnu` | Advertised; full-suite CI required (`ubuntu-24.04`) | — | Native artifact required | Active for structurally eligible `library_metadata_rlib` units; exact toolchain identity is part of every key |
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
| IBM Power | `powerpc64le-unknown-linux-gnu` | Native execution evidence is not retained yet | Structurally active when the exact toolchain identity is captured |
| IBM Z | `s390x-unknown-linux-gnu` | Native execution evidence is not retained yet | Structurally active when the exact toolchain identity is captured |

IBM Power and IBM Z need native runners before Cargo-Rail can claim tested execution. They are not excluded by a
runtime platform allowlist.

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
| Dependency and workspace library metadata/rlib | Active for any exact, content-identified native toolchain | One live-root-bound session, one portable declared crate root, complete bounded source topology and bytes, exact compiler environment, containment observation, `.rmeta`, optional `.rlib`, Rust-only dependencies, no linker responsibility |
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
just bench-native-cache-smoke
just bench-native-cache 10
```

The v6 execution contract invalidates v5 measurements. V6 binds the command's effective default regular-file creation
mode (the Unix umask result) into session and action identity, and binds each exact output mode into result identity,
the canonical descriptor, and the result pack. Different effective creation modes cannot cross-hit. Do not compare
measurements from different execution contracts.

The benchmark now seeds and measures Cargo-Rail in one authoritative source root with a clean target directory and a
fresh copy of the populated CAS. Acceptance hashes every `.d`, `.rmeta`, and `.rlib` byte without a root-bound
exclusion, and requires identical action censuses, runtime behavior, compiler-event identities, cache accounting, and
measured/proof-replay outcomes. Specialist comparisons may still use distinct roots, but they do not measure
Cargo-Rail's arbitrary-root reuse path; the independent-root fixture carries that correctness proof.

When evaluating another workspace, record the repository commit, tool and host/target identities, linker, runner,
wrappers, flags, exact action argv, clean-root method, native/disabled/cold/warm timings, hit and byte counts, all bypass
reasons, and byte-identity findings. Choose the group count for the decision being made; one accepted interleaved group
is a correctness smoke test, while repeated groups support p50/p95 comparisons.
The benchmark pins the current stable `sccache` release from the CI tool registry and measures both its server and
opt-in client-side local-disk paths. Preserve raw output and never combine different commits, toolchains, targets,
policies, or cache modes.
