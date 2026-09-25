# Cargo-Rail

Cargo-Rail makes Rust workspaces faster to change and safer to release.
It selects required work, reuses verified compiler results, repairs dependency drift,
and carries reviewed per-crate intent through durable releases.
Cargo, nextest, Just, and CI remain the executors.

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail)
[![MSRV](https://img.shields.io/crates/msrv/cargo-rail)](https://github.com/loadingalias/cargo-rail/blob/main/Cargo.toml)

## Workspace operations

| Operation                    | Result |
| ---------------------------- | ------ |
| Compiler reuse               | Restore verified results across ordinary Cargo, nextest, Just, IDE, and CI work |
| Affected-work planning       | Select exact jobs, packages, targets, and variants from one captured workspace |
| Dependency coherence         | Review one dependency, feature, and MSRV repair before explicit apply |
| Rust changesets and releases | Record per-crate intent in `.changes/` and resume the exact release transaction |
| Surface analysis             | Derive product reachability and proven visibility reductions from compiler facts |
| Split and sync               | Extract Cargo-aware Git history and synchronize later changes in both directions |

## Installation

The release workflow packages these native archives:

| Host                 | Archive target |
| -------------------- | -------------- |
| macOS, Apple Silicon | `aarch64-apple-darwin` |
| Linux, x86-64        | `x86_64-unknown-linux-gnu` |
| Linux, Arm64         | `aarch64-unknown-linux-gnu` |
| Windows, x86-64      | `x86_64-pc-windows-msvc` |

1. Download `cargo-rail-<target>.zip` and `SHA256SUMS` from the same
   [release](https://github.com/loadingalias/cargo-rail/releases).
1. Compare the archive's SHA-256 with its entry in `SHA256SUMS`.
   Use `sha256sum` on Linux, `shasum -a 256` on macOS, or `Get-FileHash -Algorithm SHA256` in PowerShell.
1. Verify the archive's build provenance with `gh attestation verify cargo-rail-<target>.zip -R loadingalias/cargo-rail`.
1. Extract the archive into a private installation directory.
   Keep all files in its `cargo-rail/` directory together, including the compiler driver, source bundle, helpers,
   and component manifest.
   On Unix, preserve executable permissions.
1. Add that directory to `PATH`, or link `cargo-rail` from a directory already on `PATH`.
   Cargo-Rail finds its components beside the real file.
   Run `cargo rail --version` to confirm the selected installation.

Each check proves something different.
`SHA256SUMS` comes from the same release as the archive,
so it detects corrupted or inconsistent bytes but does not identify the publisher.
Releases are immutable, so their assets cannot be replaced after publication.
The attestation proves that the packaging workflow built the archive from a recorded commit.
The companion GitHub Action verifies checksums, the component manifest, and the license on install,
but not attestations.

The archives contain authenticated compiler components for cache reuse and Surface analysis.
GNU Linux archives require glibc 2.39 or newer.
The workflow does not publish standalone shell or PowerShell installers, musl archives,
or archives for every host eligible for native caching.

`cargo install cargo-rail --locked` builds the general CLI without the remote cache providers; add `--features s3,azure` to include them.
Native archives include both.
`cargo binstall cargo-rail` can obtain a prebuilt CLI, but does not install the complete companion component set.
Use the full native archive for immediately available Surface and authenticated compiler reuse,
or select an independently authenticated [compiler adapter pack](docs/caching.md#select-an-independent-compiler-adapter).
`cargo rail surface --schema` also works without those components.
When the workspace-selected rustup toolchain lacks `rustc-dev`, Surface preparation can install it;
non-rustup toolchains require the matching compiler development files to be present already.

Before replacing an older cache installation, read the [cache upgrade and recovery instructions](docs/troubleshooting.md#compiler-reuse-did-not-happen).

### Installation paths

| Path | Provides | Rust needed to install |
| --- | --- | --- |
| Native archive | CLI, remote cache providers, authenticated compiler driver and its source | None |
| `cargo binstall cargo-rail` | Prebuilt CLI only | None |
| `cargo install cargo-rail --locked` | CLI without remote cache providers; add `--features s3,azure` | Cargo-Rail's `rust-version` or newer |
| GitHub Action (planner, `setup`, `cache`) | Authenticated archive components for the version in the Action's lock | None |
| Compiler adapter pack | A compiler driver for installations without one | The exact compiler it serves |

Install one exact version with `cargo install cargo-rail --locked --version X.Y.Z`, then confirm it with `cargo rail --version`.

Three Rust versions stay separate:

- **Cargo-Rail's build version.**
  A source install needs at least the `rust-version` in Cargo-Rail's manifest.
  Prebuilt archives and the Action need no Rust to install Cargo-Rail.
- **The workspace toolchain.**
  Cargo-Rail runs the Cargo and rustc that your workspace selects, such as through `rust-toolchain.toml`.
  Your MSRV does not have to match Cargo-Rail's build version.
- **Compiler driver compilers.**
  The archive's driver matches the exact compiler that built the release.
  For another workspace compiler,
  Cargo-Rail builds the authenticated driver source or an adapter pack with that exact compiler,
  which needs its `rustc-dev` component.
  A pack sets a minimum compiler release and no maximum.
  Without a working driver, compiler work runs normally without reuse, and Surface fails.

## Start here

Inspect the current branch first:

```bash
cargo rail plan
```

`plan` selects required work without editing tracked source.
Then use the workflows that match the change.

### Reuse compiler work

```bash
cargo rail cache setup --check
cargo rail cache setup
cargo rail cache ready
cargo rail cache status
```

`cache setup --check` previews enrollment and exits `1` when work is pending.
`cache setup` installs the global Cargo wrapper and enrolls this workspace.
`cache ready` proves one cold miss and one verified warm restore.
After setup, ordinary Cargo, nextest, Just, IDE,
and CI commands on that machine use the same verified cache path.

### Keep dependencies coherent

```bash
cargo rail unify --show-diff
cargo rail unify --check
```

Both commands leave manifests unchanged.
After review, apply the exact repair with a recovery backup:

```bash
cargo rail unify apply --backup
```

### Record and check release intent

```bash
cargo rail change add my-crate --bump patch --message "Fixed connection retries"
cargo rail change check --merge-base
cargo rail release check --all --publication
```

`change add` writes the crate bump and release note to `.changes/` for review.
`release check` validates the release plan without publishing.
Publication still requires explicit `--publish` authority.

### Check product reachability

```bash
cargo rail surface --prepare
cargo rail surface --check --explain
cargo rail surface --fix --dry-run --explain
```

`surface --prepare` may install `rustc-dev` for the selected rustup toolchain.
The inspection and dry-run commands do not edit tracked source.

## Reduce work at each layer

```text
all declared workflow work
└─ plan keeps required jobs, packages, targets, and variants
   └─ cache restores compatible compiler results
      └─ Cargo runs freshness checks and the remaining misses
```

These reductions stack because they remove different work.
Unify can also resolve duplicate dependency declarations before the next build.
Reviewed change intent bounds version, changelog,
and publication work to the selected crates and required dependents.
Use the plan summary and cache report to measure the actual result;
Cargo-Rail does not invent a time-saved estimate.

## Reuse verified compiler work

Cargo-Rail caches compiler results, not copied target directories.
Each hit revalidates the compiler action, inputs, environment, deps, outputs,
and stored bytes before Cargo sees a result.

| Layer            | Decision |
| ---------------- | -------- |
| Cargo L0         | Cargo freshness and incremental compilation stay authoritative |
| Local L1         | Reuse verified compiler results across ordinary Cargo, nextest, Just, IDE, and CI commands |
| Remote L2        | Share the same verified result through AWS S3, Cloudflare R2, or Azure Blob Storage |
| Distributed miss | Automatic placement requires fresh evidence of a material win; qualification mode collects that evidence |

Unsupported or incompletely observed work runs through normal Cargo.

With root portability set to `remap`,
Cargo-Rail discovers the regular repository files that rustc actually reads,
persists only that bounded selector, and revalidates exact bytes and metadata before lookup.
Physical checkout and `CARGO_TARGET_DIR` locations stay executor-local,
so eligible work can reuse across independent roots without making a same-size input mutation look
unchanged.

`cargo clean` intentionally leaves the selected profile's local CAS intact,
so an empty target tree can still reuse verified compiler work.
Local result storage has a 10 GiB default byte bound.
Before an incoming result would exceed it,
Cargo-Rail removes the oldest eligible action authorities while protecting leased
or in-flight results.
`CARGO_RAIL_CACHE=off` provides a cold baseline without touching the CAS.
Inspect `cargo rail cache status --scope local --json` and preview complete CAS removal with `cargo rail cache clean --scope local --check`; after local cleanup,
rerun `cargo rail cache setup` to repair the empty authority.
See the [cache contract and cleanup policy](docs/caching.md#inspect-clean-detach-or-uninstall) and the [benchmark contract](docs/benchmarking.md#claim-requirements).

Cache setup accepts x86-64 and Apple Silicon macOS; x86-64 and Arm64 Windows; and x86-64, Arm64,
RISC-V 64, IBM Z, and IBM POWER Linux.
Explicit targets are eligible when their compiler and target inputs are fully captured;
linked outputs also require complete linker and output evidence.
Windows currently bypasses native result reuse even though setup
and compiler-fact acquisition are supported.
Runtime eligibility does not imply that a release archive exists for that host.
See [native host eligibility](docs/caching.md#native-host-eligibility).

**IBM validation is incomplete.**
Native caching and execution remain implemented for IBM Z (`s390x`) and little-endian IBM POWER (`powerpc64le`) Linux,
but end-to-end validation on these hosts is deferred while repository runner access is resolved.
Treat these targets as unvalidated until their native qualification checks pass.

## Audit product reachability

`cargo rail surface` merges real compiler facts across products, libraries, build scripts, proc macros, doctests,
features, and configured targets.
It reports dead public declarations and visibility wider than actual consumers need.

Surface can apply proven visibility reductions with `--fix`; dead code remains report-only.
With `--explain`, report contract v4 separates raw observations from merged declarations,
shows bounded retention examples,
and measures the findings suppressed by one conservative reason without adding
that graph work to the normal path.
`rail.toml` defines analysis policy, while source mutation always requires explicit CLI authorization.

## Give each executor exact work

`cargo rail plan` combines semantic source and configuration changes, Cargo target ownership,
declared dependency edges, observed inputs, and repository-owned work declarations.
Incomplete evidence widens only its owning work item instead of skipping it.

Every required Cargo work decision receives an exact `cargo_args` array.
Pass that array to Cargo, nextest, Just, CI, etc.
Do not rebuild scope from path globs or explanation fields.

Variant catalogs can bind deliverables to typed Cargo roots
and external inputs without subscribing every row to a conservative build.
Named Cargo work can also project exact runtime-artifact prerequisites separately from the tests
that consume them.

```text
changed source
  → Cargo ownership and semantic manifest changes
  → reverse dep impact
  → evidence-backed named work decisions
  → exact per-work package, target, or CI variant scope
  → Cargo, nextest, Just, CI, etc.
```

Cross-process consumers must validate contract v9 and its content-derived identity,
then verify that the current head and captured source match the saved plan
before executing typed selectors.
Comparing `HEAD` alone is insufficient.
Planner machine identities remain provenance; executor-local Cargo, toolchain,
and platform state cannot rewrite the decision.
The companion GitHub Action owns the independent strict consumer.
See [Planning](docs/planning.md).

### GitHub Actions

The native v10 Action installs the exact Cargo-Rail release in its lock, runs the planner once,
and exposes the validated plan plus exact required-work selectors:

```yaml
- uses: loadingalias/cargo-rail-action@v10
  id: rail

- name: Test affected packages
  if: contains(fromJSON(steps.rail.outputs.required-work), 'cargo.test')
  shell: bash
  env:
    PLAN_FILE: ${{ steps.rail.outputs.plan-file }}
  run: |
    ARGS_FILE="$(mktemp "$RUNNER_TEMP/cargo-rail-args.XXXXXX")"
    cargo-rail-action plan cargo-args "$PLAN_FILE" cargo.test > "$ARGS_FILE" || exit "$?"
    CARGO_ARGS=()
    while IFS= read -r -d '' argument; do CARGO_ARGS+=("$argument"); done < "$ARGS_FILE"
    rm -- "$ARGS_FILE"
    cargo nextest run "${CARGO_ARGS[@]}" --locked
```

Use `loadingalias/cargo-rail-action/cache@v10` separately in each execution job that needs remote compiler reuse.
Both actions install the exact Cargo-Rail release recorded by the Action by default.
Set an exact `version` only when the workflow needs a reproducible pin.
Its `mode` input is required.
Do not provide remote credentials to untrusted jobs.
Use `read` for trusted jobs that must not publish, and grant `read-write` only to trusted seed jobs.
The Action exposes typed root portability and an optional strict authenticated provider probe.
See the [Action guide](https://github.com/loadingalias/cargo-rail-action).
To match CI locally, install the version that the Action's `version` output reports,
as shown in [installation paths](#installation-paths).

## Carry release intent through the workflow

See [Releasing a workspace](docs/releases.md) for hosted execution, review, and recovery.

- `cargo rail unify --check` derives one reviewable dependency repair from the captured workspace;
  `cargo rail unify apply --backup` applies it with backups for recovery.
- `cargo rail change` records bump and release-note intent in `.changes/` during the change itself.
- `cargo rail release` carries that intent through versioning, changelogs, exact auxiliary Cargo lockfiles,
  exact-SHA validation and native assets, tags, publication, and durable resume state.
  Configured hosted requests survive terminal closure; reviewed merges continue the same intent.
- `cargo rail split` moves relevant crate history into an OSS repository;
  `cargo rail sync` maps later changes in both directions and stops with a resumable receipt
  when Git three-way merge needs a human.

Registry publication is denied by default.
Mutations bind the captured snapshot, revalidate drift, and write only authorized paths.

## Integration examples

[Apache Iggy’s integration](https://github.com/apache/iggy/pull/3095) uses Cargo-Rail’s dependency graph to scope Cargo and nextest work.
[Prosody](https://github.com/prosody-events/prosody/blob/fd622e78e9b60a7535321c5966e20e6248089192/.github/workflows/quality.yaml) shows `cargo-rail-action` routing build, test, and infrastructure jobs.

## Status and direction

Cargo-Rail is under active pre-1.0 development.
Breaking CLI, configuration, and machine-contract changes should be expected.
Security fixes target the latest release;
keep Cargo-Rail and its GitHub Action current and compatible.
Each Action release supports exactly the Cargo-Rail version in its lock;
see the Action's [supported version pairs](https://github.com/loadingalias/cargo-rail-action#supported-version-pairs).

Report cache hit/miss/bypass evidence and minimized failures from real workspaces.
Contributions that remove complexity or strengthen correctness checks are welcome.

## Documentation and support

Start with [Planning](docs/planning.md), the [cache contract](docs/caching.md), or [Troubleshooting](docs/troubleshooting.md).
[Configuration](docs/config.md) explains the repository policy boundary.
Use `cargo rail <command> --help` for the exact CLI.
Contributors can start with [Architecture](docs/architecture.md).

Cargo-Rail is licensed under [MIT](LICENSE).
See [Contributing](CONTRIBUTING.md), the [security policy](SECURITY.md), [releases](https://github.com/loadingalias/cargo-rail/releases), and the [issue tracker](https://github.com/loadingalias/cargo-rail/issues).
