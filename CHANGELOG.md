# Changelog

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
