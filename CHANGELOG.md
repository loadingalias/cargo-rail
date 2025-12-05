# Changelog

All notable changes to this project will be documented in this file.

## [0.1.0] - 2025-12-05

Initial release.

### Added

- **`cargo rail affected`** - Graph-aware change detection with multiple output formats
- **`cargo rail test`** - Run tests for affected crates only, auto-detects nextest
- **`cargo rail unify`** - Resolution-based `[workspace.dependencies]` management
  - Multi-target resolution via `cargo metadata --filter-platform`
  - MSRV computation from dependency graph
  - Unused dependency detection and removal
  - Dead feature pruning
  - Transitive pinning (workspace-hack replacement)
- **`cargo rail split`** - Extract crates to standalone repos with git history
- **`cargo rail sync`** - Bidirectional monorepo/split repo synchronization
- **`cargo rail release`** - Version bump, changelog generation, tagging, publishing
- **`cargo rail init`** - Generate rail.toml configuration
- **`cargo rail clean`** - Remove generated artifacts
- **`cargo rail config validate`** - Validate configuration

### Tested On

- tikv (83 crates, 57 deps unified)
- meilisearch (19 crates, 46 deps unified)
- helix (13 crates, 16 deps unified)
- tokio (10 crates, 10 deps unified)
- ripgrep (10 crates, 9 deps unified)
- polars, ruff, jj, iced, vello, and more

[0.1.0]: https://github.com/loadingalias/cargo-rail/releases/tag/v0.1.0
