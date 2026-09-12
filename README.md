# Cargo-Rail

Cargo-Rail is a Rust workspace engine for affected-work planning, verified compiler reuse,
dependency repair, Surface analysis, releases, and crate split/sync.
Cargo, nextest, Just, and CI remain the executors;
Cargo-Rail gives them one captured workspace model and exact scope.

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail)
[![MSRV](https://img.shields.io/crates/msrv/cargo-rail)](https://github.com/loadingalias/cargo-rail/blob/main/Cargo.toml)

## Workspace operations

| Operation                      | Result |
| ------------------------------ | ------ |
| Compiler reuse                 | Verified local and remote compiler results, with measured placement for distributed misses |
| Affected-work planning         | Dependency-aware plans with exact package, target, and variant selectors |
| Surface analysis               | Compiler-derived product reachability and proven visibility reductions |
| Dependency repair and releases | Coherent manifests, `.changes/` release intent, and resumable publication |
| Split and sync                 | Cargo-aware Git history extraction and bidirectional three-way sync |

## Installation

The v0.26.0 release workflow packages these native archives:

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
1. Extract the archive into a private installation directory.
   Keep all files in its `cargo-rail/` directory together, including the compiler driver, source bundle, helpers,
   and component manifest.
   On Unix, preserve executable permissions.
1. Add that directory to `PATH`, then run `cargo rail --version` to confirm the selected installation.

The archives contain authenticated compiler components for cache reuse and Surface analysis.
GNU Linux archives require glibc 2.39 or newer.
The workflow does not publish standalone shell or PowerShell installers, musl archives,
or archives for every host eligible for native caching.

`cargo install cargo-rail --locked` builds the general CLI.
`cargo binstall cargo-rail` can obtain a prebuilt CLI, but does not install the complete companion component set.
Use the full native archive for Surface and authenticated compiler reuse.
`surface --schema` also works without those components.
When the workspace-selected rustup toolchain lacks `rustc-dev`, Surface preparation can install it;
non-rustup toolchains require the matching compiler development files to be present already.

Before replacing an older cache installation, read the [cache upgrade and recovery instructions](docs/troubleshooting.md#compiler-reuse-did-not-happen).

## Start here

1. Enable transparent local compiler reuse:

   ```bash
   cargo rail cache setup --check
   cargo rail cache setup
   cargo rail cache status
   ```

1. Inspect exactly what a branch affects:

   ```bash
   cargo rail plan
   ```

1. Audit the workspace's real Rust surface:

   ```bash
   cargo rail surface --prepare
   cargo rail surface --check --explain
   cargo rail surface --fix --dry-run --explain
   ```

The commands above have different effects:

- `cache setup --check` does not write and exits `1` when setup or repair is pending.
- `cache setup` owns Cargo's global `build.rustc-wrapper` and enrolls this workspace in a private cache profile.
  It rejects another global wrapper or any environment or workspace setting that would shadow it.
- `surface --prepare` may install `rustc-dev` for the selected rustup toolchain.
  It does not change the default toolchain.
- `plan` and the Surface inspection commands do not edit tracked source.

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
With `--explain`, report contract v3 separates raw observations from merged declarations,
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

The native v9 Action accepts Cargo-Rail `0.26.PATCH` releases, runs the planner once,
and exposes the validated plan plus exact required-work selectors:

```yaml
- uses: loadingalias/cargo-rail-action@v9
  id: rail
  with:
    version: 0.26.0

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

Use `loadingalias/cargo-rail-action/cache@v9` with `version: 0.26.0` separately in each execution job that needs remote compiler reuse.
Its `mode` input is required: use `read` for untrusted jobs and grant `read-write` only to trusted seed jobs.
The Action exposes typed root portability and an optional strict authenticated provider probe.
See the [Action guide](https://github.com/loadingalias/cargo-rail-action).

## Carry release intent through the workflow

- `cargo rail unify --check` derives one reviewable dependency repair from the captured workspace;
  `cargo rail unify apply --backup` applies it with backups for recovery.
- `cargo rail change` records bump and release-note intent in `.changes/` during the change itself.
- `cargo rail release` carries that intent through versioning, changelogs, exact auxiliary Cargo lockfiles,
  exact-SHA readiness, tags, publication, and durable resume state.
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

Report cache hit/miss/bypass evidence and minimized failures from real workspaces.
Contributions that remove complexity or strengthen correctness checks are welcome.

## Documentation and support

Start with [Planning](docs/planning.md), the [cache contract](docs/caching.md), or [Troubleshooting](docs/troubleshooting.md).
[Configuration](docs/config.md) explains the repository policy boundary.
Use `cargo rail <command> --help` or the [command reference](docs/commands/README.md) for the exact CLI.
Contributors can start with [Architecture](docs/architecture.md).

Cargo-Rail is licensed under [MIT](LICENSE).
See [Contributing](CONTRIBUTING.md), the [security policy](SECURITY.md), [releases](https://github.com/loadingalias/cargo-rail/releases), and the [issue tracker](https://github.com/loadingalias/cargo-rail/issues).
