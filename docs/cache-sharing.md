# Share compiler reuse across workspaces

Cargo-Rail's compiler cache is a private, user-scoped L1. One setup enables eligible reuse for Cargo, nextest, Just,
IDE, and CI invocations that use the same effective Cargo home:

```bash
cargo rail cache setup --check
cargo rail cache setup
cargo rail cache status --scope local --format json
```

The setup receipt selects one local CAS authority and byte bound. Workspaces under the same OS user and receipt share
exact compiler results. Action identity remains bound to the canonical physical workspace root because Rust metadata
and diagnostics can contain source paths. A second checkout first misses, then reuses its own root-bound variant.

## What is shared

L1 stores exact compiler results, not a copied target directory. It can reuse eligible metadata, libraries, archives,
diagnostics, and certified Apple or Linux linked outputs. Before restoring a file, Cargo-Rail rechecks the action,
toolchain, inputs, result identity, and stored bytes. Cargo still owns fingerprints and incremental compilation.

Unsupported work runs normally and reports why it bypassed the cache. This includes build-script execution, native
proc-macro consumers, Clippy, rustdoc, doctests, cross-target work, incremental state, custom linkers, and COFF-linked
outputs. See the generated [execution and reuse matrix](caching.md#execution-and-reuse-support) for the exact boundary.

## Separate trust domains

Use separate Cargo homes when CI, untrusted jobs, and interactive users must not share cache authority. To select a
dedicated authority within one Cargo home, configure it during setup:

```bash
cargo rail cache setup --local-dir /var/cache/cargo-rail/ci --max-size 20GiB --check
cargo rail cache setup --local-dir /var/cache/cargo-rail/ci --max-size 20GiB
```

The directory must be a private real path. Cargo-Rail records it in the setup receipt, and runtime environment
variables cannot redirect it. Do not copy a CAS between trust domains or expose it through a shared filesystem.

## Disable reuse for one process tree

```bash
CARGO_RAIL_CACHE=off cargo check --locked
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

Local cleanup removes the validated receipt-selected CAS and leaves setup unhealthy until repaired.
Removal deletes only the Cargo field and private setup state named by the same receipt; it preserves the CAS. Both
commands revalidate bytes immediately before mutation and refuse changed, shadowed, linked, or unowned state.

## Add remote sharing

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

In GitHub Actions, configure provider credentials first. Every later Cargo command in that job uses the installed
receipt:

```yaml
- uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
- uses: loadingalias/cargo-rail-action/cache@47e86bde928ce420b85efa5f8d3b5feb96fd0ffc # v7.0.0
  with:
    version: 0.23.0
    url: ${{ vars.CARGO_RAIL_CACHE_URL }}
    mode: read
- run: cargo check --workspace --all-features --locked
```

Invoke the action in each execution job because GitHub-hosted jobs do not share a machine or Cargo home.

Validate or canonicalize a URL without resolving credentials or contacting storage:

```bash
cargo rail cache normalize \
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012'
```

`--remote-mode` accepts `read` or `read-write` and defaults to `read-write` when a URL is selected. Read mode requires
an existing compatible protocol marker and never writes. Read-write mode conditionally creates that
marker and one compressed, bounded entry per base action. The entry binds the selected environment names, exact
action, result identity, and verified result pack. A second distinct identity conditionally replaces it with a
terminal metadata-only conflict. Build clients use only object reads and conditional writes; they do not list or
delete objects. `--local-only` removes the persisted remote authority while preserving L1. Transient environment
selection remains available for qualification and advanced automation, and overrides the receipt for that process
tree.

L1 remains authoritative. A verified L1 hit makes no remote request. Only an L1 miss contacts a private short-lived
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

`CARGO_RAIL_CACHE_REMOTE_ENVIRONMENT` adds a sorted comma-separated set of reviewed, non-secret compiler environment
names to the built-in `CARGO_PKG_NAME`, `CARGO_PKG_VERSION_PATCH`, and `OUT_DIR` policy. Only value digests enter action
identity; raw values are not uploaded. An unapproved name bypasses L2 for that compiler unit without disabling a valid
L1 result.

Status schema 11 reports provider, protocol, mode, approved-name count, distributed mode and policy, retained-history
state, and a redacted authority identity as `direct_transport_selected`; it never prints the URL. Status and doctor
remain network-free.

## Limit provider permissions

For `s3://company-cargo-rail-cache/rust/team?...`, the build identity needs `s3:GetObject` on exactly these resources:

```text
arn:aws:s3:::company-cargo-rail-cache/rust/team/native-v5/protocol
arn:aws:s3:::company-cargo-rail-cache/rust/team/native-v5/entries/*
```

Trusted producers also need `s3:PutObject` on the same resources. Consumers configured with `mode: read` do not.
Cargo-Rail does not need bucket listing, deletion, multipart upload, administration, or wildcard access. Keep
bucket lifecycle cleanup outside build credentials.

For R2, create an S3 API token restricted to the selected bucket with Object Read or Object Read & Write, then expose
its Access Key ID and Secret Access Key through the job's standard AWS credential environment. The `r2://` URL derives
the account endpoint and region `auto`; do not add the endpoint or credentials to the URL. R2 documents `GetObject`,
`PutObject`, and the `If-Match`/`If-None-Match` conditional operations used by this protocol as supported.

AWS S3 and R2 use the same bounded conditional-object protocol; Azure Blob Storage implements the same cache protocol
through its native conditional operations. Cleartext transport exists only for deterministic loopback protocol
fixtures and is not a supported remote provider. No remote superiority claim is made until retained real-backend
measurements satisfy the published benchmark contract.
