# Caching

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

Setup enrolls the current workspace for ordinary Cargo, nextest, Just, IDE, and CI commands. Workspaces that share
one effective Cargo home share the installed wrapper, but each enrollment owns a separate cache profile:

```bash
cargo rail cache setup --check
cargo rail cache setup
cargo rail cache status --scope local
cargo rail doctor native-cache
```

Setup previews and then owns one global `build.rustc-wrapper`, private launcher and worker bytes, and an installation
receipt. It also binds the exact physical workspace root to one private profile with its own bounded CAS trust domain.
Running setup in another workspace creates another profile; it does not replace the first profile. An unenrolled
workspace executes normally without L1 or L2 reuse. Setup refuses another global wrapper, persistent shadowing,
ambiguous Cargo homes, linked authority paths, or changed owned state.

Cargo freshness and incremental compilation remain L0. An L1 action binds the compiler, toolchain, arguments, target,
environment, dependencies, source topology and bytes, native-search inputs, and declared outputs. Physical mode also
binds the canonical workspace root. Remap mode replaces that root only for certified portable result classes while
keeping the executor's physical output directory out of the portable operation identity. Stored descriptors and every
output byte are reverified before restore. The bounded source capture deliberately over-invalidates when it cannot
prove that an unused path was irrelevant.

A result is:

- a `hit` only after current inputs and stored bytes verify;
- a `miss` when no authoritative result exists and successful cold output may populate the CAS;
- a `bypass` when the invocation is outside the supported class and executes normally; or
- a `conflict` when one action produced distinct semantic results, in which case neither result restores.

Disable reuse for one process tree without changing setup:

```bash
CARGO_RAIL_CACHE=off cargo check --locked
```

## Native host eligibility

Cache setup accepts these operating-system and architecture pairs:

| Operating system | Native architecture | Rust architecture |
|---|---|---|
| Linux | x86-64 | `x86_64` |
| Linux | Arm64 | `aarch64` |
| Linux | RISC-V 64 | `riscv64` |
| Linux | IBM Z | `s390x` |
| Linux | IBM POWER | `powerpc64` |
| macOS | x86-64 | `x86_64` |
| macOS | Apple silicon | `aarch64` |
| Windows | x86-64 | `x86_64` |
| Windows | Arm64 | `aarch64` |

Rust reports the architecture of a `powerpc64le-unknown-linux-gnu` host as `powerpc64`. The exact rustc host target
remains part of cache identity, and distributed-worker identity also binds endianness. Every other operating-system
and architecture pair fails cache setup closed and leaves Cargo execution unchanged.

This eligibility applies to local L1, remote L2, and the distributed client. It does not provide a release archive:
the runner still needs a native Cargo-Rail build with the cache component binaries required by the selected mode.
Remote objects also remain bound to the exact compiler action and platform identity.

Compiler-result reuse is native-target only. If Cargo passes an explicit `--target` that differs from rustc's host
target, Cargo-Rail records `cross_target_toolchain_evidence_unavailable` and executes rustc normally. A native RISC-V,
IBM Z, or IBM POWER runner is therefore eligible for L1 and L2; an x86-64 runner cross-compiling to one of those
targets is not. A distributed worker must match the client's architecture, endianness, operating system, rustc host
target, compiler, and sysroot.

## Share results remotely

Remote selection is workspace-bound machine state, never repository configuration. Persist L2 in the current
workspace profile during setup, then use ordinary Cargo:

```bash
cargo rail cache setup --check --remote \
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012' \
  --remote-mode read-write
cargo rail cache setup --remote \
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012' \
  --remote-mode read-write
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

cargo rail cache normalize \
  'r2://0123456789abcdef0123456789abcdef/cargo-rail-cache'
cargo rail cache setup --check --remote \
  'r2://0123456789abcdef0123456789abcdef/cargo-rail-cache' \
  --remote-mode read-write --root-portability remap
cargo rail cache setup --remote \
  'r2://0123456789abcdef0123456789abcdef/cargo-rail-cache' \
  --remote-mode read-write --root-portability remap
cargo rail cache probe --json
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
`--root-portability remap`; that mode admits only certified workspace Rust metadata and library results. Rustc reads
of regular repository files outside the package source root become a bounded dynamic selector. Before lookup,
Cargo-Rail revalidates each selected path, file kind, byte length, content digest, and executable mode. Symlink or
reparse crossings, inputs outside repository authority, generated namespaces, native-search inputs, ambiguous roots,
user-selected remaps, and unsupported output classes bypass cross-root reuse.

External `CARGO_TARGET_DIR` locations are supported for eligible native results. The cache identity uses one stable
logical output directory, while local compilation and restore continue to use Cargo's exact physical output parent.
Changing only a checkout or target root therefore preserves portable identity; changing a selected input, including a
same-size edit, produces a miss.

Additional L2 environment names must be reviewed and non-secret. Select them with repeated `--remote-environment`
options during setup. Only value digests enter identity; raw values are not uploaded.

## Distribute eligible misses

Distributed execution runs below Cargo L0, L1, and L2. It accepts only bounded compiler-only Rust operations with
complete source, dependency, and selected repository inputs. Linked outputs, build scripts, generated namespaces,
native dependencies, unmodeled options, and newly observed compiler environments remain local.

The client requires one complete mTLS worker authority:

```bash
cargo rail cache setup --check \
  --distributed-endpoint '10.0.0.20:39443' \
  --distributed-server-name worker.example.internal \
  --distributed-capability 'worker-capability-v3:sha256:CAPABILITY_DIGEST' \
  --distributed-authority /etc/cargo-rail/server-ca.pem \
  --distributed-client-certificate /etc/cargo-rail/client.pem \
  --distributed-client-private-key /etc/cargo-rail/client.key
```

Run setup without `--check` only after reviewing the authority. The default `automatic` policy stays local until
fresh class-specific measurements predict a critical-path win. `qualification` sends every eligible miss to collect
evidence and may be slower.

Deploy the direct worker only on a dedicated single-tenant host or ephemeral VM. Pin the compiler and worker
capability, use mutual TLS, run each attempt inside a resource-bounded sandbox, and inherit no operator or provider
credentials.

A transport, worker, lease, sandbox, or pre-commit validation failure executes the normalized operation locally once.
A successful response still passes the native-cache validation and restore transaction before Cargo sees output.

## Compiler-evidence cache

`cargo rail unify --check` may reuse compiler observations after revalidating their compiler, source, manifests,
targets, features, Cargo configuration, dependencies, outputs, executable identity, and observed environment reads.
This store contains diagnostic evidence, not restorable Cargo artifacts.

Check mode may update evidence under `target/cargo-rail/`. Inspect `evidence_cache` in JSON output for hits, misses,
and reasons.

## Inspect, clean, detach, or uninstall

```bash
cargo rail cache status --scope local --json
cargo rail cache profiles --json
cargo rail cache clean --scope workspace --check
cargo rail cache clean --scope local --check
cargo rail cache detach --check
cargo rail cache drop-profile --profile PROFILE_ID --check
cargo rail cache drop-unbound --check
cargo rail cache uninstall --check
```

Workspace cleanup removes reconstructible state for the current checkout. Local cleanup removes only the current
profile's CAS after validating ownership and waiting for readers; rerun `cache setup` afterward. `cache detach`
removes the current root binding but preserves the profile and CAS. `cache drop-profile` accepts an opaque ID from
`cache profiles` and removes only a detached profile with no enrolled roots. `cache uninstall` removes the global
wrapper, Cargo field, and installation receipt while preserving profiles and their CAS data.

An upgrade from v0.25 retains its machine-global policy as unbound pre-profile state because that receipt cannot prove
which workspace owned its remote. Runtime selection never uses that state. Enroll the intended workspace explicitly,
then preview `cache drop-unbound` before removing the retained CAS. Every operation refuses changed, shadowed, linked,
or unowned authority. Do not edit profile records, individual CAS objects, or Cargo fingerprints by hand.

Status schema 15 reports the selected profile ID, workspace binding, trust domain, and redacted remote selection
source. It reports stable native failure-reason counters separately from the bounded 65,536-event usage ledger,
so capture, identity, and post-execution witness failures remain visible after that ledger fills. If the counter file
cannot be validated, `failure_reason_counts_available` is `false` instead of reporting invented zeroes. Verbose status
also reports the fixed 64-shard native restore-lock namespace and any staging residue.

## Benchmark evidence

Use [the benchmarking contract](benchmarking.md) for smoke, qualification, correctness, evidence retention, and claim
requirements.

### Aggregate cache measurements

`cargo rail cache report --start /absolute/recording.json -f json` creates a new private recording. Set the
machine-owned `CARGO_RAIL_CACHE_REPORT` environment variable to that path for the commands in the reporting interval.
After their compiler processes exit, `cargo rail cache report --finish /absolute/recording.json -f json` closes the
interval and emits the [`cache-report-v1` contract](../schemas/cache-report-v1.schema.json). Finishing again returns
the recorded totals; starting over an existing file is rejected.

The recording contains bounded aggregate counters and reason counts. Concurrent wrapper outcomes are serialized;
report storage never authorizes a restore or changes a cache key. Known recording errors and counter overflow mark
the measurements incomplete. Corrupt or unreadable recordings fail collection. These are observed wrapper outcomes,
not Cargo freshness counts or an estimate of time saved. Cache problems can cause fallback within a miss or bypass;
wrapper failures are a separate outcome and must not be added to reason counts as disjoint categories.

The companion Action opens the interval during cache setup, collects one small record per job after compilation,
and publishes one final workflow report. The final report requires the expected job labels and identifies missing
jobs and incomplete measurements. Local storage remains per job; shared storage is not summed across runners.
