# Cargo-Rail

Cargo-Rail is a Rust workspace engine for affected-work planning, verified compiler reuse, dependency repair, Surface
analysis, releases, and crate split/sync. Cargo, nextest, Just, and CI remain the executors; Cargo-Rail gives them one
captured workspace model and exact scope.

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail)
[![CI](https://img.shields.io/github/actions/workflow/status/loadingalias/cargo-rail/commit.yaml?branch=main)](https://github.com/loadingalias/cargo-rail/actions/workflows/commit.yaml)
[![MSRV](https://img.shields.io/crates/msrv/cargo-rail)](https://github.com/loadingalias/cargo-rail/blob/main/Cargo.toml)

## What Cargo-Rail removes

| Stop paying for | Cargo-Rail |
|---|---|
| Compiler work already completed | Verified local, remote, and selectively distributed compiler reuse |
| CI jobs and packages unaffected by a change | Deterministic, graph-aware plans with exact execution scope |
| Public Rust APIs no product can reach | Complete compiler-derived Surface analysis and exact visibility fixes |
| Dependency, changelog, and release glue | Coherent manifests, reviewed `.changes/`, exact-SHA releases, and resume |
| Heavy monorepo split/sync infrastructure | Cargo-aware history movement and bidirectional Git three-way sync |

Cargo-Rail replaces separate path filters, dependency linters, cache wrappers, changelog tools, release bots, and
repository sync scripts. Every decision comes from one captured Cargo workspace model.

## Installation

macOS on Apple Silicon and Linux:

```bash
curl --proto '=https' --tlsv1.2 -fsSL \
  https://github.com/loadingalias/cargo-rail/releases/latest/download/cargo-rail-installer.sh | sh
```

Windows PowerShell:

```powershell
irm https://github.com/loadingalias/cargo-rail/releases/latest/download/cargo-rail-installer.ps1 | iex
```

The installer verifies the native archive and every selected component. Native archives for Apple silicon, GNU
Linux, musl Linux, and Windows include cache helpers and authenticated Surface authority. Linux musl archives use a
static core CLI and a native dynamically loaded compiler-fact driver.
The driver relies on the host's standard musl `libgcc_s` runtime, as rustc host tools do.
GNU Linux archives require glibc 2.35 or newer. The installer verifies that floor before it downloads an archive or
replaces an existing installation.

`cargo install cargo-rail --locked` and `cargo binstall cargo-rail` remain available. They cannot prepare or run Surface
analysis; `surface --schema` still works. The native installer supplies Cargo-Rail's authenticated Surface driver and
offline source authority. When the workspace-selected toolchain lacks `rustc-dev`, Surface can install that component
through `rustup`; non-rustup toolchains work when the matching compiler development files are already present.

## Start here

1. Enable transparent local compiler reuse:

   ```bash
   cargo rail cache setup --check
   cargo rail cache setup
   cargo rail cache status
   ```

2. Inspect exactly what a branch affects:

   ```bash
   cargo rail plan
   ```

3. Audit the workspace's real Rust surface:

   ```bash
   cargo rail surface --prepare
   cargo rail surface --check --explain
   cargo rail surface --fix --dry-run --explain
   ```

The commands above have different effects:

- `cache setup --check` does not write and exits `1` when setup or repair is pending.
- `cache setup` owns Cargo's global `build.rustc-wrapper` and enrolls this workspace in a private cache profile. It
  rejects another global wrapper or any environment or workspace setting that would shadow it.
- `surface --prepare` may install `rustc-dev` for the selected rustup toolchain. It does not change the default
  toolchain.
- `plan` and the Surface inspection commands do not edit tracked source.

## Reuse verified compiler work

Cargo-Rail caches compiler results, not copied target directories. Each hit revalidates the compiler action, inputs, environment, deps, outputs, and stored bytes before Cargo sees a result.

| Layer | Decision |
|---|---|
| Cargo L0 | Cargo freshness and incremental compilation stay authoritative |
| Local L1 | Reuse verified compiler results across ordinary Cargo, nextest, Just, IDE, and CI commands |
| Remote L2 | Share the same verified result through AWS S3, Cloudflare R2, or Azure Blob Storage |
| Distributed miss | Automatic placement requires fresh evidence of a material win; qualification mode collects that evidence |

Unsupported or incompletely observed work runs through normal Cargo. Crucially, no guessed hit can become a build result.

With root portability set to `remap`, Cargo-Rail discovers the regular repository files that rustc actually reads,
persists only that bounded selector, and revalidates exact bytes and metadata before lookup. Physical checkout and
`CARGO_TARGET_DIR` locations stay executor-local, so eligible work can reuse across independent roots without making
a same-size input mutation look unchanged.

`cargo clean` intentionally leaves the selected profile's local CAS intact, so an empty target tree can still reuse
verified compiler work. Local result storage has a 10 GiB default byte bound. Before an incoming result would exceed
it, Cargo-Rail removes the oldest eligible action authorities while protecting leased or in-flight results.
`CARGO_RAIL_CACHE=off` provides a cold baseline without touching the CAS. Inspect
`cargo rail cache status --scope local --json` and preview complete CAS removal with
`cargo rail cache clean --scope local --check`; after local cleanup, rerun `cargo rail cache setup` to repair the
empty authority. See the [cache contract and cleanup policy](docs/caching.md#inspect-clean-detach-or-uninstall) and the
[benchmark contract](docs/benchmarking.md#claim-requirements).

## Audit product reachability

`cargo rail surface` merges real compiler facts across products, libraries, build scripts, proc macros, doctests, features, and configured targets. It reports dead public declarations and visibility wider than actual consumers need.

Surface can apply proven visibility reductions with `--fix`; dead code remains report-only. With `--explain`, report
contract v3 separates raw observations from merged declarations, shows bounded retention examples, and measures the
findings suppressed by one conservative reason without adding that graph work to the normal path. `rail.toml` defines
analysis policy, while source mutation always requires explicit CLI authorization.

## Give each executor exact work

`cargo rail plan` combines semantic source and configuration changes, Cargo target ownership, declared dependency edges, observed inputs, and repository-owned work declarations. Incomplete evidence widens only its owning work item instead of skipping it.

Every required Cargo work decision receives an exact `cargo_args` array. Pass that array to Cargo, nextest, Just, CI, etc. Do not rebuild scope from path globs or explanation fields.

Variant catalogs can bind deliverables to typed Cargo roots and external inputs without subscribing every row to a
conservative build. Named Cargo work can also project exact runtime-artifact prerequisites separately from the tests
that consume them.

```text
changed source
  → Cargo ownership and semantic manifest changes
  → reverse dep impact
  → evidence-backed named work decisions
  → exact per-work package, target, or CI variant scope
  → Cargo, nextest, Just, CI, etc.
```

Cross-process consumers must validate contract v8 and its content-derived identity, then verify that the current head
and captured source match the saved plan before executing typed selectors. Comparing `HEAD` alone is insufficient.
Planner machine identities remain provenance; executor-local Cargo, toolchain, and platform state cannot rewrite the
decision. This repository's Commit workflow transfers one plan artifact and validates it with
[`scripts/plan/read.py`](scripts/plan/read.py). See [Planning](docs/planning.md).

### GitHub Actions

The v8 Action runs the planner once and exposes the validated plan, required work IDs, and strict reader:

```yaml
- uses: loadingalias/cargo-rail-action@v8
  id: rail

- name: Test affected packages
  if: contains(fromJSON(steps.rail.outputs.required-work), 'cargo.test')
  shell: bash
  env:
    PLAN_FILE: ${{ steps.rail.outputs.plan-file }}
    PLAN_READER: ${{ steps.rail.outputs.plan-reader }}
  run: |
    python3 "$PLAN_READER" verify-checkout "$PLAN_FILE"
    CARGO_ARGS=()
    while IFS= read -r -d '' arg; do CARGO_ARGS+=("$arg"); done \
      < <(python3 "$PLAN_READER" cargo-args "$PLAN_FILE" cargo.test)
    cargo nextest run "${CARGO_ARGS[@]}" --locked
```

Use `loadingalias/cargo-rail-action/cache@v8` separately in each execution job that needs remote compiler reuse.
Its `mode` input is required: use `read` for untrusted jobs and grant `read-write` only to trusted seed jobs. The
Action exposes typed root portability and an optional strict authenticated provider probe. See the [Action
guide](https://github.com/loadingalias/cargo-rail-action).

This repository dogfoods the same boundary. Local Just commands use the installed release; trusted `main` and release
jobs use the v8 cache action against one private Cloudflare R2 authority. Pull requests remain local-only. R2
credentials are attached only to steps that execute compiler work, while CI and developer machines use distinct
bucket-scoped credentials for the same remote authority. The Commit workflow builds the checked-out planner once—
necessary because that source may introduce the next plan contract—then transfers both its exact v8 plan and planner
binary to fail-closed consumers. A release tag reuses the archives already built, smoke-tested, and attested by that
exact-SHA Commit run, building only release-only targets unless an explicit recovery run must reconstruct the full
set.

## Carry release intent through the workflow

- `cargo rail unify --check` derives one reviewable dependency repair from the captured workspace; `cargo rail unify apply --backup` applies it reversibly.
- `cargo rail change` records bump and release-note intent in `.changes/` during the change itself.
- `cargo rail release` carries that intent through versioning, changelogs, exact auxiliary Cargo lockfiles, exact-SHA readiness, tags, publication, and durable resume state.
- `cargo rail split` moves relevant crate history into an OSS repository; `cargo rail sync` maps later changes in both directions and stops with a resumable receipt when Git three-way merge needs a human.

Registry publication is denied by default. Mutations bind the captured snapshot, revalidate drift, and write only authorized paths.

## Used in Rust workspace CI

[Apache Iggy](https://github.com/apache/iggy/pull/3095) replaced a custom affected-crates script with Cargo-Rail and uses the planner to scope Cargo, nextest, and Docker work while retaining conservative fallbacks.
[Prosody](https://github.com/prosody-events/prosody/blob/fd622e78e9b60a7535321c5966e20e6248089192/.github/workflows/quality.yaml) uses `cargo-rail-action` to route build, test, and infrastructure jobs.

## Status and direction

Cargo-Rail is under active pre-1.0 development. Breaking CLI, configuration, and machine-contract changes should be
expected. Security fixes target the latest release; keep Cargo-Rail and its GitHub Action current and compatible.

Current priorities are stable/nightly release lines, smaller configuration and CLI surfaces, and lighter remote-provider
dependencies. Report cache hit/miss/bypass evidence and minimized failures from real workspaces. Contributions that
remove complexity or strengthen evidence are welcome.

## Documentation and support

Start with [Planning](docs/planning.md), the [cache contract](docs/caching.md), or
[Troubleshooting](docs/troubleshooting.md). [Configuration](docs/config.md) explains the repository policy boundary;
[the v0.26 migration guide](docs/migration-v0.26.md) covers the bounded v0.25 upgrade. Use
`cargo rail <command> --help` for the exact CLI. Contributors can start with [Architecture](docs/architecture.md).

Cargo-Rail is licensed under [MIT](LICENSE). See [Contributing](CONTRIBUTING.md), the
[security policy](SECURITY.md), [releases](https://github.com/loadingalias/cargo-rail/releases), and the [issue tracker](https://github.com/loadingalias/cargo-rail/issues).
