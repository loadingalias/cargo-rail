//! Release management and publishing orchestration
//!
//! This module implements Pillar 4: Release & Publishing Orchestration.
//!
//! # Core Invariants
//!
//! 1. **Releases always driven from monorepo**
//!    - Split repos are mirrors, never sources
//!    - Version bumps happen in mono first
//!    - Tags created in mono, then synced to splits
//!
//! 2. **Changelogs are per-thing (crate or product), not global**
//!    - Each crate tracks its own changes
//!    - No monolithic workspace CHANGELOG.md
//!    - Products can have separate changelogs from their crates
//!
//! 3. **Every released thing has: name, version, last_sha anchor**
//!    - `name`: Identifier for the release channel
//!    - `version`: Current semver version
//!    - `last_sha`: Git SHA of last release (anchor point for next)
//!
//! # Architecture
//!
//! - **Configuration**: `[[releases]]` in rail.toml defines what can be released
//! - **Metadata**: Embedded in rail.toml (last_version, last_sha, last_date)
//! - **Commands**:
//!   - `cargo rail release plan`: Analyze changes, suggest version bump
//!   - `cargo rail release apply`: Execute release (bump, tag, sync)
//!
//! # Example rail.toml
//!
//! ```toml
//! [[releases]]
//! name = "lib-core"
//! crate = "crates/lib-core"
//! split = "lib_core"  # optional: links to [[splits]]
//! last_version = "0.3.1"
//! last_sha = "abc123..."
//! last_date = "2025-01-15"
//! ```

pub mod metadata;
pub mod plan;

pub use metadata::ReleaseTracker;
pub use plan::{ReleasePlan, VersionBump};
