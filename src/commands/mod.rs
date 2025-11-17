//! CLI commands for cargo-rail
//!
//! This module contains all user-facing command implementations:
//!
//! ## Graph Operations
//! - **affected**: Find crates affected by changes
//! - **test**: Run tests for affected crates only
//! - **check**: Run cargo check for affected crates
//! - **clippy**: Run clippy for affected crates
//!
//! ## Split & Sync
//! - **split**: Split monorepo crates to separate repositories
//! - **sync**: Bidirectional sync between monorepo and split repos
//!
//! ## Inspection
//! - **status**: Show split/sync status for all crates
//!
//! All commands accept `&WorkspaceContext` to avoid redundant workspace loads.

pub mod affected;
pub mod check;
pub mod clippy;
pub mod split;
pub mod status;
pub mod sync;
pub mod test;

pub use affected::run_affected;
pub use check::run_check;
pub use clippy::run_clippy;
pub use split::run_split;
pub use status::run_status;
pub use sync::run_sync;
pub use test::run_test;
