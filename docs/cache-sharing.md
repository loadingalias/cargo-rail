# Share compiler reuse across workspaces

Cargo-Rail's transparent compiler cache is a private, user-wide L1. One setup enables eligible reuse for ordinary
Cargo, nextest, Just, IDE, and CI invocations that use the same effective Cargo home:

```bash
cargo rail cache setup --check
cargo rail cache setup
cargo rail cache status --scope local --format json
```

The setup receipt selects one local CAS authority and byte bound. Workspaces under the same OS user and receipt can
share exact compiler results, but action identity remains bound to the canonical physical workspace root because Rust
metadata and diagnostics can contain source paths. A second checkout therefore misses first and then reuses only its
own root-bound variant.

## What is shared

The CAS can reuse graduated dependency and workspace-library `.rmeta`, optional `.rlib`, compiler-owned static
archives, metadata-only proc-macro producer `.rmeta`, dep-info, and captured diagnostic output, including actions that
consume exact native-static search namespaces. Certified default Apple and Linux ELF linker paths can additionally
restore build-script executables, proc-macro producer dylibs, ordinary binaries, tests, examples, benchmarks, `dylib`,
and `cdylib` outputs. Every hit revalidates the action/witness/result binding and stored bytes before replacing an
output. Cargo still owns fingerprints and incremental compilation; an intact target normally removes the compiler
invocation at L0 before L1 is involved.

Native proc-macro consumers, build-script execution and generated output production, Clippy diagnostics, rustdoc,
doctests, cross-target work, incremental compilation, nonstandard target layouts, COFF-linked output, custom linkers,
and ambiguous wrapper composition bypass L1 with a specific missing-capability reason. Rust-required native compiler,
assembler, preprocessing, archive, and probe children remain typed cold operations until their complete input and
mutation authority exists. A bypass executes the selected tool chain and is not a build failure.

## Isolate a machine role

Use a separate Cargo home when CI, an untrusted job, and an interactive user must not share cache authority. For a
dedicated authority with the same Cargo home, select it during setup:

```bash
cargo rail cache setup --local-dir /var/cache/cargo-rail/ci --max-size 20GiB --check
cargo rail cache setup --local-dir /var/cache/cargo-rail/ci --max-size 20GiB
```

The directory must be a real private path. Cargo-Rail records it in the setup receipt; runtime environment variables
cannot silently redirect reuse. Do not copy a CAS between trust domains or expose it through a shared filesystem.

## Disable reuse for one process tree

```bash
CARGO_RAIL_CACHE=off cargo check
```

The wrapper immediately executes the original compiler command without loading installation context, session state,
or CAS data. It preserves argv, working directory, inherited environment—including `CARGO_RAIL_CACHE=off`—streams,
and exit status.

## Cleanup and removal

```bash
cargo rail cache clean --scope local --check
cargo rail cache clean --scope local
cargo rail cache setup                 # repair the selected empty authority

cargo rail cache remove --check
cargo rail cache remove
```

Local cleanup removes the validated receipt-selected CAS and leaves setup intentionally unhealthy until repaired.
Removal deletes only the Cargo field and private setup state named by the same receipt; it preserves the CAS. Both
commands revalidate bytes immediately before mutation and refuse changed, shadowed, linked, or unowned state.

## Add setup-once team sharing beneath L1

Repository `[cache]` configuration is rejected because a checkout must not select a network write destination. Persist
one machine-owned authority during setup, then use ordinary Cargo:

```bash
cargo rail cache setup --check --remote \
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012' \
  --remote-mode read-write
cargo rail cache setup --remote \
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012' \
  --remote-mode read-write
cargo build --workspace --all-features --locked
```

In GitHub Actions, the same contract is one input. Configure provider credentials first; every later Cargo command in
that job uses the installed receipt:

```yaml
- uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
- uses: loadingalias/cargo-rail-action/cache@47e86bde928ce420b85efa5f8d3b5feb96fd0ffc # v7.0.0
  with:
    version: 0.22.1
    url: ${{ vars.CARGO_RAIL_CACHE_URL }}
    mode: read
- run: cargo check --workspace --all-features --locked
```

Invoke the Action in each execution job because GitHub-hosted jobs do not share a machine or Cargo home.

Validate or canonicalize a URL without resolving credentials or contacting storage:

```bash
cargo rail cache normalize \
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012'
```

`--remote-mode` accepts `read` or `read-write` and defaults to `read-write` when a URL is selected. Read mode
requires an existing compatible protocol marker and never issues a write. Read-write mode conditionally creates that
marker and one compressed, bounded entry per base action. The entry binds the selected environment names, exact
action, result identity, and verified result pack. A second distinct identity conditionally replaces it with a
terminal metadata-only conflict. Build clients use only object reads and conditional writes; they do not list or
delete objects. `--local-only` removes the persisted remote authority while preserving L1. Transient environment
selection remains available for qualification and advanced automation, and overrides the receipt for that process
tree.

L1 remains authoritative. A verified L1 hit performs no remote request. Only an L1 miss contacts a private short-lived
loopback coordinator that shares one AWS SDK client, credential resolution, and connection pool across rustc processes.
It retains no build-result memory cache and exits after bounded inactivity. Startup or IPC failure falls back to direct
transport. An empty-L1 hit needs one entry GET after the coordinator's protocol check. Absence, conflict, corruption,
throttling, credential failure, or outage still runs the compiler.

Official S3 URLs require a region and expected 12-digit bucket-owner account. Cargo-Rail pins both on every object
operation and rejects configured endpoint overrides for official AWS authority. Credentials remain outside URLs,
configuration, result packs, compiler arguments, diagnostics, and cache keys. A domain-separated digest separates live
coordinators by credential authority without storing raw credential values. Cargo-Rail removes the URL and credential
environment from every compiler child it launches. Prefer a machine/container role, OIDC, or a preconfigured profile;
keep exported provider credentials job-scoped and least-privilege.

`CARGO_RAIL_CACHE_REMOTE_ENVIRONMENT` may add a sorted comma-separated set of reviewed, non-secret compiler
environment names to the built-in `CARGO_PKG_NAME`, `CARGO_PKG_VERSION_PATCH`, and `OUT_DIR` policy. Only value digests
enter action identity; raw
values are never uploaded. An unapproved name bypasses L2 for that compiler unit without disabling an existing valid
L1 result.

Status schema 11 reports provider, protocol, mode, approved-name count, distributed mode and policy, retained-history
state, and a redacted authority identity as `direct_transport_selected`; it never prints the URL. Status and doctor
remain network-free.

## Grant only object access

For `s3://company-cargo-rail-cache/rust/team?...`, the build identity needs `s3:GetObject` on exactly these resources:

```text
arn:aws:s3:::company-cargo-rail-cache/rust/team/native-v5/protocol
arn:aws:s3:::company-cargo-rail-cache/rust/team/native-v5/entries/*
```

Trusted producers also need `s3:PutObject` on the same resources. Consumers configured with `mode: read` do not.
Cargo-Rail does not need bucket listing, deletion, multipart upload, bucket administration, or wildcard access. Keep
bucket lifecycle cleanup outside build credentials.

For R2, create an S3 API token restricted to the selected bucket with Object Read or Object Read & Write, then expose
its Access Key ID and Secret Access Key through the job's standard AWS credential environment. The `r2://` URL derives
the account endpoint and region `auto`; do not add the endpoint or credentials to the URL. R2 documents `GetObject`,
`PutObject`, and the `If-Match`/`If-None-Match` conditional operations used by this protocol as supported.

AWS S3 and R2 use the same bounded conditional-object protocol; Azure Blob Storage implements the same cache protocol
through its native conditional operations. Cleartext transport exists only for deterministic loopback protocol
fixtures and is not a supported remote provider. No remote superiority claim is made until retained real-backend
measurements satisfy the published benchmark contract.
