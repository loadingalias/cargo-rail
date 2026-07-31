# Changelog









## [0.20.1](https://github.com/loadingalias/cargo-rail/compare/v0.20.0...v0.20.1) - 2026-07-31

- Consolidate planning and caching documentation, correct the release-source example, and refresh Rust dependencies and
  CI action pins.


## [0.20.0](https://github.com/loadingalias/cargo-rail/compare/v0.19.1...v0.20.0) - 2026-07-29

- Raised the repository and package toolchain to Rust 1.97.1, updated Rust dependencies, the native-cache fixture
  dependency graph, GitHub Actions, the CI planner's cargo-rail version, and the native-cache comparator's sccache
  installation to their current releases. Release rebuilds now use the exact package toolchain, immutable assets are
  verified instead of overwritten, and action verification binds every workflow pin to the action lock.

- Prevented bulk Git object reads from deadlocking when request and response pipes fill.

- Hardened backup, release, split, and sync mutation boundaries against path escape, symlink traversal, cross-operation
  plan reuse, and exact-release checkout drift. Split and sync check modes now distinguish clean state from pending work,
  configuration validation rejects malformed unify globs and empty split branches, and release checks report skipped
  evidence separately from passed checks.

- Added exact generated native-cache capability authority and `cargo rail doctor native-cache`, kept uncertified hosts on
  stable fail-closed bypasses, hardened Windows cache boundaries against reparse points and transient reader conflicts,
  and preserved Cargo's workspace path spelling so Windows compiler outputs remain byte-exact.

- Published one generated execution, cache, and performance support matrix, added continuous full-suite CI for all six
  advertised native host/architecture pairs, and added macOS x86-64 plus required Linux musl release inventories.

- Presented the `cargo-rail` crate and CLI as Cargo-Rail, the Rust workspace engine, and aligned the README, CLI help,
  package metadata, and public documentation around its shared Cargo and Git decision model without renaming technical
  interfaces.


## [0.19.1](https://github.com/loadingalias/cargo-rail/compare/v0.19.0...v0.19.1) - 2026-07-25

- Hardened exact-SHA release readiness to reject all-skipped GitHub rollups and run release commits through normal CI. `cargo rail config migrate` now removes the inert `release.require_clean` and `release.publish_delay` fields, and release previews no longer claim to delay between publishes. Added explicit cache capability and evaluation guidance.


## [0.19.0](https://github.com/loadingalias/cargo-rail/compare/v0.18.0...v0.19.0) - 2026-07-24

- Make split and sync snapshot-native by replacing path ownership with Cargo member names, persisting versioned
  `Rail-Origin` provenance in ordinary Git history, migrating legacy notes, preserving exact Git trees and commit
  metadata, and binding planner and release output to the shared snapshot.

- Add fail-closed action-key diagnostics over exact source, resolution, toolchain, executable, Cargo configuration, argv,
  typed environment, and verified dependency-result identities. Transparent rustdoc observation preserves the selected
  tool and HTML output while recording stable dep-info. Build-script compilation separates its non-circular
  pre-execution action identity from the ordered instructions, environment reads, generated tree, and execution evidence
  in its result identity. Incomplete boundaries remain explicitly non-reusable while ordinary unsupported execution stays
  available.

  Add `cargo rail run --all --action build --hermetic` for the graduated pure-Rust Cargo-check class. It performs an
  explicit locked fetch, captures immutable crates.io, remote registry-mirror, or Git dependency sources, then checks
  locked/offline in fresh read-only source and isolated output roots with logical path remapping and a controlled
  environment. macOS enforces filesystem and network denial and can issue a verified action/result manifest; other hosts
  remain platform-limited. Build scripts, proc macros, docs, linked/native/cross-target work, custom tool boundaries, and
  sccache fail closed. Cargo fingerprints and incremental state are never restored. Action plans and decision receipts
  use schema version 4.

- Add a bounded machine-local action/output cache for eligible macOS hermetic Cargo checks. Verified hits restore exact
  declared outputs into a clean root without starting Cargo or rustc; changed inputs, corrupt objects, unsupported
  classes, and other platforms remain fail-closed. Add `--no-cache` and extend `run --explain`, diagnostics, and
  `clean --cache` with local-cache decisions.

- Add portable, verified native compiler-result caching for non-incremental dependency and workspace library
  metadata/rlib units on Apple Silicon macOS and ARM64 Linux with Cargo/rustc 1.97.1. Ordinary `cargo rail run` check
  and build actions can reuse byte-exact outputs across clean roots without restoring or fabricating Cargo target state,
  incremental state, or fingerprints.

  Preserve custom wrappers and sccache, keep incremental builds and unproven linker/build-script/proc-macro classes
  explicitly bypassed, and fail closed on input, toolchain, environment, SDK/linker, cache-object, and output mutations.
  Add a representative registry/Git/native/proc-macro fixture plus reproducible cold/warm benchmarks and cache evidence.

- Make reviewed change files authoritative for release planning and add exact-SHA, resumable, tags-last release execution.

- Make planner impact semantic and target-aware, bind run actions to exact Cargo resolution views, and add explainable dependency-unification diagnostics.


## [0.18.0](https://github.com/loadingalias/cargo-rail/compare/v0.17.3...v0.18.0) - 2026-07-19

- Capture complete, stable source state for deterministic planning from Git worktrees or declared Cargo filesystem roots; reject concurrent Git, byte, directory, or metadata drift; keep historical ranges object-only; support nested and no-Git Cargo workspaces; and exclude resolved Cargo and cargo-rail generated state.

- Preserve every Cargo package as an exact `PackageId`-keyed graph node and build dependency edges from Cargo's resolved graph, retaining distinct versions, renamed dependencies, dependency kinds, and target conditions while keeping inactive declarations out of resolved topology and confining package-name lookup to ambiguity-aware workspace selection.

- Add lazy exact Cargo resolution views keyed by package, feature, target, toolchain, and sanitized Cargo configuration; replace filename heuristics with deterministic PackageId ownership; and introduce opt-in immutable workspace snapshots over exact source, manifests, lockfile, configuration, toolchain, and target inputs without slowing native/default commands.

- Replace hard-coded run surfaces with a bounded, snapshot-bound action graph. Built-in and repository actions now share
  one shell-free expansion and stable topological order across local execution, JSON/GitHub CI plans, and version-2
  decision receipts. Repository generators declare exclusive outputs plus separate check/regenerate commands; paths,
  dependencies, tokens, environment capabilities, cycles, and portable ownership collisions fail closed before
  execution. Ownership validation remains fast at the configured action/path limits, and command startup retains safe
  stack headroom on Windows as the action CLI grows.

- Make rail.toml sparse, add config explain and semantic migrations, and replace invalid option matrices with typed policies.

### BREAKING CHANGES

- **run**: [**breaking**] replace surfaces with a bounded action graph ([71a5972](https://github.com/loadingalias/cargo-rail/commit/71a5972c26388b286dc10175101a6a7100e36af3))
- **config**: [**breaking**] make repository policy sparse ([577f55b](https://github.com/loadingalias/cargo-rail/commit/577f55b1a9b520c69927a3f08adc726c6ea2ecf0))

### Features

- **workspace**: bind commands to canonical snapshots ([7b70568](https://github.com/loadingalias/cargo-rail/commit/7b70568a6d450988299e5cc93ffaf7b354ca1a7b))
- **workspace**: establish exact resolution snapshots ([fec7444](https://github.com/loadingalias/cargo-rail/commit/fec7444ae5cc10ffb393cc4e515ea942e716dbb1))
- **workspace**: stabilize source and package identity ([5399da4](https://github.com/loadingalias/cargo-rail/commit/5399da4e5361a9cad613ade6338f732c3b6f0650))
- **planner**: capture complete worktree source state ([9de889f](https://github.com/loadingalias/cargo-rail/commit/9de889f9f417c79d6aa5cc5e273414eb8eb50905))

### Bug Fixes

- **cli**: prevent Windows startup stack overflow ([2726bd9](https://github.com/loadingalias/cargo-rail/commit/2726bd9fb468938a598b1e171064350730334016))
- **workspace**: report directory drift deterministically ([3c99742](https://github.com/loadingalias/cargo-rail/commit/3c997426cfe87037e93735be4a3ca62952eff698))
- **workspace**: normalize snapshot paths on Windows ([dfe18d4](https://github.com/loadingalias/cargo-rail/commit/dfe18d404684ec1d582cd72de002953b96c72a0e))


## [0.17.3](https://github.com/loadingalias/cargo-rail/compare/v0.17.2...v0.17.3) - 2026-07-14

- Fixed crates.io publication checks so local workspace packages cannot masquerade as published versions. Release publishing now targets crates.io explicitly, requires the committed lockfile, rejects dirty package contents, and excludes Finder metadata.

### Bug Fixes

- **release**: verify crates.io publication explicitly ([1bd5c68](https://github.com/loadingalias/cargo-rail/commit/1bd5c68efe44ca4e9c39616bae1f568a5d11d20d))


## [0.17.2](https://github.com/loadingalias/cargo-rail/compare/v0.17.1...v0.17.2) - 2026-07-14

- Fixed release Git operations to preserve the caller environment for hooks, expose standard cargo-rail release context, and retain complete hook diagnostics. Removed the hook-bypassing push dry run while keeping one atomic branch-and-tag push.

### Bug Fixes

- **release**: preserve hook context and diagnostics ([61da35d](https://github.com/loadingalias/cargo-rail/commit/61da35d4da0964618d95d0de2031a6516003bf84))


## [0.17.1](https://github.com/loadingalias/cargo-rail/compare/v0.17.0...v0.17.1) - 2026-07-12

- Fixed unify graph verification to compare pre- and post-edit metadata with the same target platform filter. Cargo-synthesized optional-dependency features are no longer treated as writable manifest feature keys.

- Allowed release abort to reconcile an atomic push rejected before any remote refs changed. Increased the strict nextest leak deadline to avoid false failures from loaded macOS process teardown while continuing to fail persistent inherited-process leaks.

- Kept immutable release recovery from being blocked by Clippy lints added after a tag was published. Normal tag-triggered releases still require a clean Clippy run.

- Synchronized upgrade policy, aligned CI and examples with cargo-rail-action v5.1.0, removed deprecated Intel macOS distribution, tightened dependency checks, and reduced CI duplication while preserving Linux, Windows, ARM, MSRV, and cross-OS test coverage.

### Bug Fixes

- **release**: recover locally rejected atomic pushes ([7340378](https://github.com/loadingalias/cargo-rail/commit/73403782bdf8921097c168f6911e2b3f00947d50))
- **workspace**: harden graph cleanup and release readiness ([3c8b7d4](https://github.com/loadingalias/cargo-rail/commit/3c8b7d410d12701c4207ee4e745578c34a5371c0))
- **release**: recover immutable tags from lint drift ([a10f176](https://github.com/loadingalias/cargo-rail/commit/a10f1763e3d0d54f0c982880304913dcd3d24808))


## [0.17.0](https://github.com/loadingalias/cargo-rail/compare/v0.16.0...v0.17.0) - 2026-07-12

- Made `cargo rail unify` faster and more exact with shared indexed Cargo metadata, workspace-only compiler evidence, source-derived feature checks, and compilation-unit cache reuse. Analysis now covers configured targets, default/no-default/all-feature builds, conditional feature selections, generated and macro-expanded source, every Cargo target kind, and target-scoped declarations.

  Graph-removing decisions now carry deterministic proof certificates with repository-relative paths normalized across platforms. Apply verifies the exact declaration edits and resulting portable Cargo graph before writing. Closed-world cleanup of dormant private features and optional dependencies requires the explicit `consumer_scope = "workspace"` contract; published feature APIs remain preserved.

- Fixed release archive verification and added recovery for an existing immutable tag.

- Restored the changelog introduction, preserved it above future releases, updated dependencies and CI action pins, and documented unavoidable duplicate graph dependencies.

### Features

- **unify**: add compiler-backed graph cleanup ([74ae271](https://github.com/loadingalias/cargo-rail/commit/74ae27107f4325a59a2010fe70333647da19fd07))

### Bug Fixes

- **unify**: normalize proof paths on Windows ([eee5446](https://github.com/loadingalias/cargo-rail/commit/eee54464d5779c0389e36680c7ce1976249456b4))
- **release**: finish patch release housekeeping ([9c355e3](https://github.com/loadingalias/cargo-rail/commit/9c355e30bc9d73de1c244e44c780cf60d29e28be))
- **release**: recover immutable release assets ([5608728](https://github.com/loadingalias/cargo-rail/commit/56087284d869818f6d37713ff4e6cc8e2722280d))



This file records user-visible changes. Git tags and [GitHub Releases](https://github.com/loadingalias/cargo-rail/releases) retain the complete release history.

## [0.16.0](https://github.com/loadingalias/cargo-rail/compare/v0.15.0...v0.16.0) - 2026-07-11

- Added Cargo-ready planner scope args, automation-safe change status output, and a commit-time change-file coverage check.

- Changed public Rust APIs for mutation contracts, release execution, split/sync safety, and test-runner selection; downstream library users must update constructors and method calls.

- Made command output formats exact, published the planner v3 JSON Schema, and added checkout-independent plan identities.

- Curated the historical changelog and required reviewed release intent for future releases.

- Bounded release, split, and sync mutations to approved repository paths, made sync conflicts resumable, and preserved exact split history and mappings.

- Skipped crates.io preflight checks when every crate in a release plan has publishing disabled.

- Made releases resumable, verified and distributed the exact tagged commit, and made Cargo, nextest, filter, and test-harness arguments backend-correct.

- Fixed Windows path normalization for release, split, sync, and portable planner identities.

### Features

- **workspace**: make control-plane operations verifiable and recoverable ([6bd64fb](https://github.com/loadingalias/cargo-rail/commit/6bd64fb6f028bee13a372ff23d4f4b789a5562b3))

### Bug Fixes

- **release**: harden release readiness and curate history ([3679801](https://github.com/loadingalias/cargo-rail/commit/367980186587210735bbecf9a7b6e3485cf2985b))
- **git**: normalize Windows paths at repository boundaries ([7936232](https://github.com/loadingalias/cargo-rail/commit/793623232c23e25d2f92734ca673421052c40b4a))

### Documentation

- **release**: record v0.16 library API breaks ([3595b4f](https://github.com/loadingalias/cargo-rail/commit/3595b4f1f4c0e6c4f89f4b66688a597bbad0f61b))

## [0.15.0](https://github.com/loadingalias/cargo-rail/compare/v0.14.0...v0.15.0) - 2026-07-06

### Added

- Added the built-in changelog engine used by the release workflow.

### Fixed

- Made change-file path assertions portable across operating systems.

## [0.14.0](https://github.com/loadingalias/cargo-rail/compare/v0.13.4...v0.14.0) - 2026-07-06

### Added

- Added graph-aware commit attribution for per-crate changelogs.

## [0.13.4](https://github.com/loadingalias/cargo-rail/compare/v0.13.3...v0.13.4) - 2026-06-01

### Fixed

- Prevented release dry runs from invoking pre-push hooks.

## [0.13.3](https://github.com/loadingalias/cargo-rail/compare/v0.13.2...v0.13.3) - 2026-06-01

### Fixed

- Made release CI wait for cargo-rail to create the GitHub Release before uploading assets.

## [0.13.2](https://github.com/loadingalias/cargo-rail/compare/v0.13.1...v0.13.2) - 2026-06-01

### Added

- Added the end-to-end publishing lane for release commits, tags, forge releases, and crates.io publication.

## [0.13.1](https://github.com/loadingalias/cargo-rail/compare/v0.13.0...v0.13.1) - 2026-05-21

### Fixed

- Allowed `cargo rail unify --check` to run outside a Git repository.

## [0.13.0](https://github.com/loadingalias/cargo-rail/compare/v0.12.0...v0.13.0) - 2026-04-18

### Changed

- Finalized planner scope semantics and raised the MSRV to Rust 1.95.
- Added per-dependency unification decisions and stricter action contract validation.
- Unified change detection under the planner surface taxonomy.

## [0.12.0](https://github.com/loadingalias/cargo-rail/compare/v0.11.0...v0.12.0) - 2026-04-17

### Changed

- Made custom planner surfaces additive instead of replacing built-in classifications.
- Added bounded summaries for plans affecting large crate sets.

### Fixed

- Updated cargo-rail-action compatibility and pinned its workflow reference.

## [0.11.0](https://github.com/loadingalias/cargo-rail/compare/v0.10.12...v0.11.0) - 2026-04-09

### Added

- Added stable execution scope and ready-to-pass Cargo package arguments for CI consumers.

### Fixed

- Isolated the bootstrap target directory on Windows.
- Normalized workspace paths in cross-platform tests.

## Historical releases

Releases before `0.11.0` were generated from raw commit subjects. The table keeps the user-facing milestones while the linked comparisons preserve exact history.

| Series | Dates | User-visible milestones |
| --- | --- | --- |
| [`0.10.x`](https://github.com/loadingalias/cargo-rail/compare/v0.9.1...v0.10.12) | 2026-02-14 to 2026-02-19 | Replaced `affected`/`test` with `plan`/`run`; added workspace-member cohort safety, compiler-backed unused-dependency detection, and multi-target fixes. `0.10.11` contained only release bookkeeping. |
| [`0.9.x`](https://github.com/loadingalias/cargo-rail/compare/v0.8.1...v0.9.1) | 2026-02-03 to 2026-02-10 | Added binary-crate filtering, metadata-cache invalidation, check-mode output files, and release checksums. |
| [`0.8.x`](https://github.com/loadingalias/cargo-rail/compare/v0.7.3...v0.8.1) | 2025-12-18 | Added MSRV inheritance, workspace lint integration, and optional-feature check-mode fixes. |
| [`0.7.x`](https://github.com/loadingalias/cargo-rail/compare/v0.6.0...v0.7.3) | 2025-12-14 to 2025-12-16 | Added configurable dependency sorting and corrected CI and release behavior. |
| [`0.6.0`](https://github.com/loadingalias/cargo-rail/compare/v0.5.3...v0.6.0) | 2025-12-14 | Hardened split/sync, removed production panics, and revised CLI and configuration output. This version had a GitHub Release but was not published to crates.io. |
| [`0.5.x`](https://github.com/loadingalias/cargo-rail/compare/v0.4.2...v0.5.3) | 2025-12-12 | Added borrowed-feature detection and repair, removed cargo-udeps, and corrected MSRV handling. |
| [`0.4.x`](https://github.com/loadingalias/cargo-rail/compare/v0.3.0...v0.4.2) | 2025-12-11 | Added configuration synchronization and fixed release lockfile handling and target matching. |
| [`0.3.0`](https://github.com/loadingalias/cargo-rail/compare/v0.2.2...v0.3.0) | 2025-12-11 | Expanded target discovery, feature exclusions, MSRV analysis, and Cargo argument output. |
| [`0.2.x`](https://github.com/loadingalias/cargo-rail/compare/v0.1.0...v0.2.2) | 2025-12-05 to 2025-12-10 | Corrected nested-workspace change detection and completed the first public CI integration. |
| [`0.1.0`](https://github.com/loadingalias/cargo-rail/releases/tag/v0.1.0) | 2025-12-05 | First published release with dependency unification, change detection, split/sync, and initial release automation. |
