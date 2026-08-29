# Caching

> Auto-generated from executable support registries and native-cache gates. Do not edit manually.
>
> Regenerate with `just gen-docs`. Support manifest schema: `9`;
> native-cache compiler-identity schema: `11`.

Cargo-Rail preserves Cargo as the executor. Each layer may remove only the work it can prove reusable.

| Layer | Authority | Result |
|---|---|---|
| Cargo L0 | Cargo fingerprints and incremental state | Cargo skips fresh work |
| Local L1 | Exact compiler action, result, and stored bytes | One compiler result is restored |
| Remote L2 | The same verified result pack under machine-owned provider authority | A result enters L1, then restores |
| Distributed miss | A pinned worker capability and validated response | One eligible miss executes remotely |
| Compiler evidence | Revalidated diagnostic observations | `unify` skips diagnostic collection |

A lookup is not authority. Missing, stale, ambiguous, unsupported, or corrupt evidence executes the normal compiler
path with a stable miss or bypass reason.

## Set up local reuse

One setup enables L1 for ordinary Cargo, nextest, Just, IDE, and CI commands using the same effective Cargo home:

```bash
cargo rail cache setup --check
cargo rail cache setup
cargo rail cache status --scope local
cargo rail doctor native-cache
```

Setup previews and then owns one global `build.rustc-wrapper`, private launcher and worker bytes, a receipt, and a
bounded CAS. It refuses another global wrapper, persistent shadowing, ambiguous Cargo homes, linked authority paths,
or changed receipt-owned state. Repeating setup verifies or repairs only the same authority.

Cargo freshness and incremental compilation remain L0. An L1 action binds the compiler, toolchain, arguments, target,
environment, dependencies, source topology and bytes, native-search inputs, physical workspace root, and declared
outputs. Stored descriptors and every output byte are reverified before restore. The bounded source capture
deliberately over-invalidates when it cannot prove that an unused path was irrelevant.

A result is:

- a `hit` only after current inputs and stored bytes verify;
- a `miss` when no authoritative result exists and successful cold output may populate the CAS;
- a `bypass` when the invocation is outside the supported class and executes normally; or
- a `conflict` when one action produced distinct semantic results, in which case neither result restores.

Disable reuse for one process tree without changing setup:

```bash
CARGO_RAIL_CACHE=off cargo check --locked
```

## Share results remotely

Remote selection is machine state, never repository configuration. Persist L2 during setup, then use ordinary Cargo:

```bash
cargo rail cache setup --check --remote   's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012'   --remote-mode read-write
cargo rail cache setup --remote   's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012'   --remote-mode read-write
```

Accepted URL families are:

```text
s3://BUCKET/PREFIX?region=REGION&owner=AWS_ACCOUNT_ID
r2://ACCOUNT_ID/BUCKET/PREFIX
azure://ACCOUNT/CONTAINER/PREFIX
```

### Cloudflare R2

Use a private, [default-jurisdiction bucket](https://developers.cloudflare.com/r2/reference/data-location/) with
public access disabled. Create an [R2 API token](https://developers.cloudflare.com/r2/api/tokens/) scoped to Object
Read & Write for that bucket. Its S3 access key ID and secret access key must be present for setup and every compiler
process that should use L2; a Wrangler login or general Cloudflare API token is not an S3 credential pair.

```bash
export AWS_ACCESS_KEY_ID='<R2 access key ID>'
export AWS_SECRET_ACCESS_KEY='<R2 secret access key>'

cargo rail cache normalize   'r2://0123456789abcdef0123456789abcdef/cargo-rail-cache'
cargo rail cache setup --check --remote   'r2://0123456789abcdef0123456789abcdef/cargo-rail-cache'   --remote-mode read-write --root-portability remap
cargo rail cache setup --remote   'r2://0123456789abcdef0123456789abcdef/cargo-rail-cache'   --remote-mode read-write --root-portability remap
cargo rail cache probe --format json
```

Use distinct bucket-scoped credential pairs for CI and developer machines even when they share one R2 authority.
Keep the protocol marker at `native-v6/protocol`; an [object lifecycle
rule](https://developers.cloudflare.com/r2/buckets/object-lifecycles/) may expire `native-v6/entries/` without
deleting the marker. Scope the prefix relative to the selected URL root when the URL includes a prefix.

Cargo-Rail currently models only R2's default jurisdiction and derives its standard account endpoint. It deliberately
has no jurisdiction or arbitrary-endpoint syntax. Use a default-jurisdiction bucket until a real consumer requires a
typed jurisdiction contract.

`cache probe` uses the persisted authority and standard AWS credential environment, including a session token when
present. It proves authenticated marker compatibility and may initialize an absent marker only in `read-write` mode;
its JSON output contains the provider, mode, readiness, and marker state but no URL, object key, or credential value.

R2 includes bounded monthly Standard-storage and operation allowances and does not charge direct R2 egress, but
storage and Class A/Class B operations beyond those allowances are billed. Treat it as metered infrastructure, not
unlimited free storage; verify the current [R2 pricing](https://developers.cloudflare.com/r2/pricing/).

Use `cargo rail cache normalize URL` to validate a URL without resolving credentials or contacting storage.
Credentials stay outside URLs, repository configuration, result packs, diagnostics, compiler arguments, and cache
keys. Prefer a machine or container role, OIDC, or a preconfigured profile.

`--remote-mode read` requires an existing compatible protocol marker and never writes. `read-write` adds conditional
protocol and entry publication. For an authority rooted at `PREFIX`, provider permissions are bounded to:

| Mode | Objects | Operations |
|---|---|---|
| `read` | `PREFIX/native-v6/protocol`, `PREFIX/native-v6/entries/*` | Object read |
| `read-write` | The same objects | Object read and conditional write |

Build credentials do not need list, delete, lifecycle, multipart-upload, or administrative authority. Keep provider
cleanup and lifecycle policy outside build credentials.

L1 remains authoritative, so an L1 hit makes no remote request. Absence, conflict, corruption, credential failure,
throttling, or outage executes the compiler. `--local-only` removes persisted L2 selection while preserving L1.

Physical-root mode is the default. It shares only checkouts at the same canonical path. Cross-root reuse requires
`--root-portability remap`; that mode admits only certified workspace Rust metadata and library results. Existing
remaps, external or generated source namespaces, native-search inputs, ambiguous roots, and unsupported output classes
bypass cross-root reuse.

Additional L2 environment names must be reviewed and non-secret. Select them with repeated `--remote-environment`
options during setup. Only value digests enter identity; raw values are not uploaded.

## Distribute eligible misses

Distributed execution runs below Cargo L0, L1, and L2. It accepts only bounded compiler-only Rust operations with
complete source and dependency inputs. Linked outputs, build scripts, generated namespaces, native dependencies,
unmodeled options, and newly observed compiler environments remain local.

The client requires one complete mTLS worker authority:

```bash
cargo rail cache setup --check   --distributed-endpoint '10.0.0.20:39443'   --distributed-server-name worker.example.internal   --distributed-capability 'worker-capability-v3:sha256:CAPABILITY_DIGEST'   --distributed-authority /etc/cargo-rail/server-ca.pem   --distributed-client-certificate /etc/cargo-rail/client.pem   --distributed-client-private-key /etc/cargo-rail/client.key
```

Run setup without `--check` only after reviewing the authority. The default `automatic` policy stays local until
fresh class-specific measurements predict a critical-path win. `qualification` sends every eligible miss to collect
evidence and may be slower.

Deploy the direct worker only on a dedicated single-tenant host or ephemeral VM. The qualified Linux mode pins the
compiler and worker capability, uses mutual TLS, runs each attempt inside a resource-bounded cgroup and Bubblewrap
sandbox, and inherits no operator or provider credentials. Use the repository's
`qualify-distributed-execution-*` recipes before serving a worker.

A transport, worker, lease, sandbox, or pre-commit validation failure executes the normalized operation locally once.
A successful response still passes the native-cache validation and restore transaction before Cargo sees output.

## Compiler-evidence cache

`cargo rail unify --check` may reuse compiler observations after revalidating their compiler, source, manifests,
targets, features, Cargo configuration, dependencies, outputs, executable identity, and observed environment reads.
This store contains diagnostic evidence, not restorable Cargo artifacts.

Check mode may update evidence under `target/cargo-rail/`. Inspect `evidence_cache` in JSON output for hits, misses,
and reasons.

## Inspect, clean, or remove

```bash
cargo rail cache status --scope local --format json
cargo rail cache clean --scope workspace --check
cargo rail cache clean --scope local --check
cargo rail cache remove --check
```

Workspace cleanup removes reconstructible state for the current checkout. Local cleanup removes only the
receipt-selected shared CAS after validating ownership and waiting for readers; rerun `cache setup` afterward.
`cache remove` removes the owned Cargo field and private setup state but preserves CAS data. Every operation refuses
changed, shadowed, linked, or unowned authority. Do not delete individual CAS objects or Cargo fingerprints by hand.

## Execution and reuse support

Execution support and cache reuse are independent. A bypass still executes Cargo normally.

### Hosts and targets

| Target | Native execution | Cross-target compilation | Release artifact | Native compiler-result cache |
|---|---|---|---|---|
| `aarch64-apple-darwin` | Advertised; local full-suite qualification required | — | Native artifact required | Active for eligible `exact_rustc_result` units; exact compiler identity is part of every key |
| `aarch64-pc-windows-msvc` | Advertised; full-suite CI required (`windows-11-arm`) | — | Native artifact required | Active for eligible `exact_rustc_result` units; exact compiler identity is part of every key |
| `aarch64-unknown-linux-gnu` | Advertised; full-suite CI required (`ubuntu-24.04-arm`) | — | Native artifact required | Active for eligible `exact_rustc_result` units; certified default-ELF linked outputs are also active; exact compiler identity is part of every key |
| `aarch64-unknown-linux-musl` | Not a native host | Required compatibility build | Cross-built artifact required | Bypass: `cross_target_toolchain_evidence_unavailable` |
| `thumbv7em-none-eabihf` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_toolchain_evidence_unavailable` |
| `wasm32-unknown-unknown` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_toolchain_evidence_unavailable` |
| `wasm32-wasip1` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_toolchain_evidence_unavailable` |
| `wasm32v1-none` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_toolchain_evidence_unavailable` |
| `x86_64-pc-windows-msvc` | Advertised; full-suite CI required (`windows-2022`) | — | Native artifact required | Active for eligible `exact_rustc_result` units; exact compiler identity is part of every key |
| `x86_64-unknown-linux-gnu` | Advertised; full-suite CI required (`ubuntu-24.04`) | — | Native artifact required | Active for eligible `exact_rustc_result` units; certified default-ELF linked outputs are also active; exact compiler identity is part of every key |
| `x86_64-unknown-linux-musl` | Not a native host | Required compatibility build | Cross-built artifact required | Bypass: `cross_target_toolchain_evidence_unavailable` |

Linux musl rows are release cross-builds, not native Linux host evidence.

### Filesystems

| Profile | Runner | Filesystem | Case behavior | Required evidence |
|---|---|---|---|---|
| Default `aarch64-apple-darwin` | Local native host | `apfs` | Insensitive | Local full endpoint suite, native probe, and benchmarks |
| Default `aarch64-pc-windows-msvc` | `windows-11-arm` | `ntfs` | Insensitive | Full endpoint suite and native probe |
| Default `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | `ext4` | Sensitive | Full endpoint suite and native probe |
| Default `x86_64-pc-windows-msvc` | `windows-2022` | `ntfs` | Insensitive | Full endpoint suite and native probe |
| Default `x86_64-unknown-linux-gnu` | `ubuntu-24.04` | `ext4` | Sensitive | Full endpoint suite and native probe |
| linux-tmpfs | `ubuntu-24.04` | `tmpfs` | Sensitive | Front-door corpus, CAS/atomicity suite, ENOSPC, and cleanup |
| windows-ntfs-vhd | `windows-2022` | `ntfs` | Insensitive | Front-door corpus, CAS/atomicity suite, cross-volume staging, ENOSPC, and cleanup |

### Deferred native hosts

| Platform | Target | Execution status | Cache status |
|---|---|---|---|
| IBM Power | `powerpc64le-unknown-linux-gnu` | Blocked: `native_powerpc64le_hardware_access_unavailable` | Structurally active when the exact compiler identity is captured |
| RISC-V 64 | `riscv64gc-unknown-linux-gnu` | Blocked: `native_riscv64gc_hardware_access_unavailable` | Structurally active when the exact compiler identity is captured |
| IBM Z | `s390x-unknown-linux-gnu` | Blocked: `native_s390x_hardware_access_unavailable` | Structurally active when the exact compiler identity is captured |

Deferred hosts need native hardware before Cargo-Rail can claim tested execution.

### Compiler classes

| Class | Reuse boundary |
|---|---|
| Metadata and Rust libraries | Active with exact toolchain, source, dependency, environment, and output identity |
| Certified Apple and Linux linked outputs | Active only with complete default-linker input evidence and byte-stable output |
| Tests, examples, benchmarks, `dylib`, and `cdylib` | Active only through a certified linker path |
| `staticlib` | Active as an exact compiler-owned archive result |
| Proc-macro producers | Metadata is active; linked producer output needs a certified linker; later macro execution is not covered |
| Native proc-macro consumers | Bypass because compile-time external reads are incomplete |
| Build-script compilation | Compiler result may reuse; script execution and generated output remain Cargo-owned cold work |
| Native dependencies and `links` | Rust consumers may reuse only with complete native-search evidence; native tools execute cold |
| Incremental, Clippy, rustdoc, and doctests | Bypass before result acquisition |
| Cross targets, custom targets or target directories, and unsupported wrappers | Execute through the selected compiler chain |

The default Apple linker chain and a Linux `cc`-selected ELF linker with GNU-compatible dependency evidence are
certified. Windows COFF linking, explicit linker selection, custom linker arguments, and external codegen backends
execute normally until their complete input boundary is graduated.

## Benchmark evidence

Use [the benchmarking contract](benchmarking.md) for smoke, qualification, correctness, evidence retention, and claim
requirements. Generated support rows describe tested authority; they are not performance claims.
