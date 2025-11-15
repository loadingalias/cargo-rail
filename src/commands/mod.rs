//! CLI commands for cargo-rail
//!
//! This module contains all user-facing command implementations:
//!
//! ## Setup & Inspection
//! - **init**: Initialize rail.toml configuration for a workspace
//! - **doctor**: Run health checks and validation
//! - **status**: Show split/sync status for all crates
//! - **mappings**: View git commit mappings for split crates
//!
//! ## Split & Sync (Pillar 2)
//! - **split**: Split monorepo crates to separate repositories
//! - **sync**: Bidirectional sync between monorepo and split repos
//!
//! ## Graph Operations (Pillar 1)
//! - **affected**: Find crates affected by changes
//! - **test**: Run tests for affected crates only
//! - **check**: Run cargo check for affected crates
//! - **clippy**: Run clippy for affected crates
//!
//! ## Linting (Pillar 3)
//! - **lint**: Workspace policy enforcement (deps, versions, manifest)
//!
//! ## Releases (Pillar 4)
//! - **release**: Release planning and execution with changelog generation
//!
//! All commands accept `&WorkspaceContext` to avoid redundant workspace loads.

pub mod affected;
pub mod check;
pub mod clippy;
pub mod doctor;
pub mod init;
pub mod lint;
pub mod mappings;
pub mod release;
pub mod split;
pub mod status;
pub mod sync;
pub mod test;

pub use affected::run_affected;
pub use check::run_check;
pub use clippy::run_clippy;
pub use doctor::run_doctor;
pub use init::run_init;
pub use lint::{run_lint_deps, run_lint_manifest, run_lint_versions};
pub use mappings::run_mappings;
pub use release::{run_release_apply, run_release_plan};
pub use split::run_split;
pub use status::run_status;
pub use sync::run_sync;
pub use test::run_test;
