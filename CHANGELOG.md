# Changelog

This file records user-visible changes. Git tags and [GitHub Releases](https://github.com/loadingalias/cargo-rail/releases) retain the complete release history.

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
