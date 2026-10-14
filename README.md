# Cargo-Rail

Cargo-Rail makes Rust workspaces faster to change and safer to release.
It selects the required work, reuses verified compiler results, repairs dependency drift,
and carries reviewed per-crate intent through one-command releases.
Cargo, nextest, Just, and CI stay the executors.

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail)
[![MSRV](https://img.shields.io/crates/msrv/cargo-rail)](https://github.com/loadingalias/cargo-rail/blob/main/Cargo.toml)

## Workspace operations

| Operation                    | Result |
| ---------------------------- | ------ |
| Compiler reuse               | Restore verified results across ordinary Cargo, nextest, Just, IDE, and CI work |
| Affected-work planning       | Select exact jobs, packages, targets, and variants from one captured workspace |
| Dependency coherence         | Review one dependency, feature, and MSRV repair before an explicit apply |
| Rust changesets and releases | Record per-crate intent in `.changes/`, then release it with one command that finishes on rerun |
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

Each release also publishes `cargo-rail-archive-hosts-v1.json`.
Its `archives` map names the supported native archive hosts for that exact `version`.
For other hosts, including Windows ARM64, `unsupported_host_installation = "source"` selects source installation.
`SHA256SUMS` and the GitHub build-provenance attestation include the manifest.
Verify the manifest.
Make sure that its version matches the selected release.
Use its archive name exactly.
The workspace target-resolution catalog does not declare archive availability.

1. Download `cargo-rail-<target>.zip` and `SHA256SUMS` from the same
   [release](https://github.com/loadingalias/cargo-rail/releases).
1. Compare the SHA-256 of the archive with its entry in `SHA256SUMS`.
   Use `sha256sum` on Linux, `shasum -a 256` on macOS, or `Get-FileHash -Algorithm SHA256` in PowerShell.
1. Verify the build provenance of the archive with `gh attestation verify cargo-rail-<target>.zip -R loadingalias/cargo-rail`.
1. Extract the archive into a private installation directory.
   Keep all files in its `cargo-rail/` directory together, including the compiler driver, source bundle, helpers,
   and component manifest.
   On Unix, keep the executable permissions.
1. Add that directory to `PATH`, or link `cargo-rail` from a directory that is already on `PATH`.
   Cargo-Rail finds its components beside the real file.
   To confirm the selected installation, run `cargo rail --version`.

Each check proves a different property.
Because `SHA256SUMS` comes from the same release as the archive,
it detects corrupted or inconsistent bytes but does not identify the publisher.
Releases are immutable, so their assets cannot be replaced after publication.
The attestation proves that the packaging workflow built the archive from a recorded commit.
When the companion GitHub Action installs Cargo-Rail, it verifies checksums, the component manifest,
and the license.
It does not verify attestations.

The archives contain authenticated compiler components for cache reuse and Surface analysis.
GNU Linux archives require glibc 2.39 or newer.
The workflow does not publish standalone shell or PowerShell installers, musl archives,
or archives for all hosts that are eligible for native caching.

`cargo install cargo-rail --locked` builds the general CLI without the remote cache providers.
To include them, add `--features s3,azure`.
Native archives include both.
`cargo binstall cargo-rail` can get a prebuilt CLI.
It does not install the complete companion component set.
To make Surface and authenticated compiler reuse available immediately, use the full native archive.
Alternatively, select an independently authenticated [compiler adapter pack](docs/caching.md#select-an-independent-compiler-adapter).
`cargo rail surface --schema` also works without those components.
If the rustup toolchain that the workspace selects does not have `rustc-dev`,
Surface preparation can install it.
A toolchain that is not from rustup must already have the matching compiler development files.

Before you replace an older cache installation, read the [cache upgrade and recovery instructions](docs/troubleshooting.md#compiler-reuse-did-not-happen).

### Installation paths

| Path | Provides | Rust needed to install |
| --- | --- | --- |
| Native archive | CLI, remote cache providers, authenticated compiler driver and its source | None |
| `cargo binstall cargo-rail` | Prebuilt CLI only | None |
| `cargo install cargo-rail --locked` | CLI without remote cache providers; add `--features s3,azure` | Cargo-Rail's `rust-version` or newer |
| GitHub Action (planner, `setup`, `cache`) | Authenticated archive components for the version in the Action's lock | None |
| Compiler adapter pack | A compiler driver for installations without one | The exact compiler that it serves |

To install one exact version, run `cargo install cargo-rail --locked --version X.Y.Z`.
Then confirm the version with `cargo rail --version`.

Three Rust versions are separate:

- **Cargo-Rail's build version.**
  A source install needs at least the `rust-version` in Cargo-Rail's manifest.
  Prebuilt archives and the Action do not need Rust to install Cargo-Rail.
- **The workspace toolchain.**
  Cargo-Rail runs the Cargo and rustc that your workspace selects, for example through `rust-toolchain.toml`.
  Your MSRV does not have to match Cargo-Rail's build version.
- **Compiler driver compilers.**
  The driver in the archive matches the exact compiler that built the release.
  For a different workspace compiler,
  Cargo-Rail uses that exact compiler to build the authenticated driver source or an adapter pack.
  That compiler needs its `rustc-dev` component.
  A pack sets a minimum compiler release but no maximum.
  CI builds and tests the driver against the current stable, beta, and nightly.
  Without a working driver, compiler work runs normally without reuse, and Surface fails.

## Start here

First, examine the current branch:

```bash
cargo rail plan
```

`plan` selects the required work.
It does not edit tracked source.
Then use the workflows that match the change.

### Reuse compiler work

```bash
cargo rail cache setup --check
cargo rail cache setup
cargo rail cache ready
cargo rail cache status
```

`cache setup --check` previews enrollment.
It exits `1` when work is pending.
`cache setup` installs the global Cargo wrapper and enrolls this workspace.
If the selected rustup toolchain does not have `rustc-dev`, `cache ready` installs it first.
Then `cache ready` proves one cold miss and one verified warm restore.
After setup, ordinary Cargo, nextest, Just, IDE,
and CI commands on that machine use the same verified cache path.

### Keep dependencies coherent

```bash
cargo rail unify --show-diff
cargo rail unify --check
```

Neither command changes manifests.
After review, use this command to apply the exact repair with a recovery backup:

```bash
cargo rail unify apply --backup
```

### Record and check release intent

```bash
cargo rail change add my-crate --bump patch --message "Fixed connection retries"
cargo rail change check --merge-base
cargo rail release --dry-run
```

`change add` writes the crate bump and release note to `.changes/` for review.
`release --dry-run` previews the release and runs every check.
It does not change anything.

### Check product reachability

```bash
cargo rail surface --prepare
cargo rail surface --check --explain
cargo rail surface --fix --dry-run --explain
```

`surface --prepare` can install `rustc-dev` for the selected rustup toolchain.
The inspection and dry-run commands do not edit tracked source.

## Reduce work at each layer

```text
all declared workflow work
└─ plan keeps required jobs, packages, targets, and variants
   └─ cache restores compatible compiler results
      └─ Cargo runs freshness checks and the remaining misses
```

These reductions combine because each one removes different work.
Unify can also resolve duplicate dependency declarations before the next build.
Reviewed change intent limits version, changelog,
and publication work to the selected crates and their required dependents.
Use the plan summary and the cache report to measure the actual result.
Cargo-Rail does not invent an estimate of saved time.

## Reuse verified compiler work

Cargo-Rail caches compiler results, not copied target directories.
Before Cargo sees a result, each hit revalidates the compiler action, inputs, environment, deps,
outputs, and stored bytes.

| Layer            | Decision |
| ---------------- | -------- |
| Cargo L0         | Cargo freshness and incremental compilation stay authoritative |
| Local L1         | Reuse verified compiler results across ordinary Cargo, nextest, Just, IDE, and CI commands |
| Remote L2        | Share the same verified result through AWS S3, Cloudflare R2, or Azure Blob Storage |
| Distributed miss | Automatic placement requires fresh evidence of a material gain; qualification mode collects that evidence |

Unsupported or incompletely observed work runs through normal Cargo.

When root portability is `remap`, Cargo-Rail finds the regular repository files that rustc actually reads.
It persists only that bounded selector.
Before lookup, it revalidates the exact bytes and metadata.
Physical checkout and `CARGO_TARGET_DIR` locations stay local to each executor.
As a result, eligible work can reuse results across independent roots.
This reuse does not make a same-size change to an input look unchanged.

By design, `cargo clean` keeps the local CAS of the selected profile intact,
so an empty target tree can still reuse verified compiler work.
Local result storage has a default bound of 10 GiB.
If an incoming result would exceed this bound,
Cargo-Rail first removes the oldest eligible action authorities.
It does not remove leased or in-flight results.
`CARGO_RAIL_CACHE=off` gives a cold baseline and leaves the CAS unchanged.
To examine local storage, run `cargo rail cache status --scope local --json`.
To preview the removal of the complete CAS, run `cargo rail cache clean --scope local --check`.
After local cleanup, run `cargo rail cache setup` again to repair the empty authority.
See the [cache contract and cleanup policy](docs/caching.md#inspect-clean-detach-or-uninstall) and the [benchmark contract](docs/benchmarking.md#claim-requirements).

Cache setup accepts these hosts:

- macOS: x86-64 and Apple Silicon.
- Windows: x86-64 and Arm64.
- Linux: x86-64, Arm64, RISC-V 64, IBM Z, and IBM POWER.

An explicit target is eligible when Cargo-Rail fully captures its compiler and target inputs.
Linked outputs also require complete linker and output evidence.
On Windows, Cargo-Rail supports setup and compiler-fact acquisition,
but it currently bypasses native result reuse.
Runtime eligibility does not mean that a release archive exists for that host.
See [native host eligibility](docs/caching.md#native-host-eligibility).

On every push to `main`, CI qualifies native caching on real Linux runners for IBM Z (`s390x`),
little-endian IBM POWER (`powerpc64le`), and RISC-V 64.
Each runner runs the compiler driver tests and the library
and cache suites from an archive built on x86-64.
No release archive exists for these hosts.
For native reuse, install from source and select an [adapter pack](docs/caching.md#select-an-independent-compiler-adapter).

## Audit product reachability

`cargo rail surface` merges real compiler facts across products, libraries, build scripts, proc macros, doctests,
features, and configured targets.
It reports dead public declarations.
It also reports visibility that is wider than the actual consumers need.

With `--fix`, Surface can apply proven visibility reductions.
Surface reports dead code but does not change it.
With `--explain`, report contract v5 does these things:

- It separates raw observations from merged declarations.
- It shows bounded retention examples.
- It measures the findings that one conservative reason suppresses.

The normal path does not do that graph work.
`rail.toml` defines analysis policy.
Source mutation always requires explicit CLI authorization.

## Give each executor exact work

`cargo rail plan` combines semantic source and configuration changes, Cargo target ownership,
declared dependency edges, observed inputs, and repository-owned work declarations.
Incomplete evidence widens only the work item that owns it, and it does not skip that item.

Each required Cargo work decision gets an exact `cargo_args` array.
Pass that array to Cargo, nextest, Just, CI, or another executor.
Do not rebuild scope from path globs or explanation fields.

Variant catalogs can bind deliverables to typed Cargo roots and external inputs.
This binding does not subscribe every row to a conservative build.
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

A cross-process consumer must validate contract v9 and its content-derived identity.
It must also verify that the current head and captured source match the saved plan.
It must do both checks before it executes typed selectors.
A comparison of `HEAD` alone is not sufficient.
Planner machine identities stay as provenance.
The Cargo, toolchain, and platform state that is local to the executor cannot rewrite the decision.
The companion GitHub Action owns the independent strict consumer.
See [Planning](docs/planning.md).

### GitHub Actions

The native v12 Action installs the exact Cargo-Rail release in its lock.
It runs the planner once.
It exposes the validated plan and the exact required-work selectors:

```yaml
- uses: loadingalias/cargo-rail-action@v12
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

Use `loadingalias/cargo-rail-action/cache@v12` separately in each execution job that needs remote compiler reuse.
By default, both actions install the exact Cargo-Rail release that the Action records.
Set an exact `version` only when the workflow needs a reproducible pin.
The cache action requires its `mode` input.
Do not give remote credentials to untrusted jobs.
Read access does not protect confidentiality.
A job that can read the remote cache can read every result in it.
Use `read` for trusted jobs that must not publish.
Give `read-write` only to trusted seed jobs.
The Action exposes typed root portability and an optional strict authenticated provider probe.
See the [Action guide](https://github.com/loadingalias/cargo-rail-action).
To match CI on your local machine, install the version that the `version` output of the Action reports.
Use the procedure in [installation paths](#installation-paths).

## Carry release intent through the workflow

For CI, protected branches, binaries, and recovery, see [Releasing a workspace](docs/releases.md).

- `cargo rail unify --check` derives one reviewable dependency repair from the captured workspace.
  `cargo rail unify apply --backup` applies that repair with backups for recovery.
- `cargo rail change` records bump and release-note intent in `.changes/` during the change itself.
- After one preview, `cargo rail release` carries that intent through versions, changelogs,
  exact auxiliary Cargo lockfiles, tags, registry publication, attested GitHub release assets,
  and GitHub releases.
  It keeps no state.
  After any interruption, run it again, locally or in CI, and it finishes the release.
- `cargo rail split` moves the applicable crate history into an OSS repository.
  `cargo rail sync` maps later changes in both directions.
  When a Git three-way merge needs a human, sync stops with a resumable receipt.

A release shows its registry and GitHub effects and asks once before the first effect.
Mutations bind the captured snapshot, revalidate drift, and write only authorized paths.

## Integration examples

[Apache Iggy’s integration](https://github.com/apache/iggy/pull/3095) uses Cargo-Rail’s dependency graph to scope Cargo and nextest work.
[Prosody](https://github.com/prosody-events/prosody/blob/fd622e78e9b60a7535321c5966e20e6248089192/.github/workflows/quality.yaml) shows how `cargo-rail-action` routes build, test, and infrastructure jobs.

## Status and direction

Cargo-Rail is under active pre-1.0 development.
Expect breaking changes to the CLI, configuration, and machine contracts.
Security fixes are for the latest release.
Keep Cargo-Rail and its GitHub Action current and compatible.
Each Action release supports exactly the Cargo-Rail version in its lock.
See the Action's [supported version pairs](https://github.com/loadingalias/cargo-rail-action#supported-version-pairs).

Report cache hit/miss/bypass evidence and minimized failures from real workspaces.
Contributions that remove complexity or make correctness checks stronger are welcome.

## Documentation and support

Start with [Planning](docs/planning.md), the [cache contract](docs/caching.md), or [Troubleshooting](docs/troubleshooting.md).
[Configuration](docs/config.md) explains the repository policy boundary.
Use `cargo rail <command> --help` for the exact CLI.
Contributors can start with [Architecture](docs/architecture.md).

Cargo-Rail is licensed under [MIT](LICENSE).
See [Contributing](CONTRIBUTING.md), the [security policy](SECURITY.md), [releases](https://github.com/loadingalias/cargo-rail/releases), and the [issue tracker](https://github.com/loadingalias/cargo-rail/issues).
