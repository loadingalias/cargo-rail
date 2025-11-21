//! CLI commands for cargo-rail
//!
//! This module contains all user-facing command implementations:
//!
//! ## Dependency Unification
//! - **unify**: Eliminate workspace-hack crates via native workspace dependency unification
//!
//! ## Configuration Management
//! - **init**: Initialize cargo-rail configuration (rail.toml)
//! - **config**: Sync configuration files (rust-toolchain.toml, etc.)
//!
//! ## Split & Sync
//! - **split**: Split monorepo crates to separate repositories
//! - **sync**: Bidirectional sync between monorepo and split repos
//!
//! ## Inspection
//! - **affected**: Find crates affected by changes (used by split/sync)
//! - **status**: Show split/sync status for all crates
//!
//! All commands accept `&WorkspaceContext` to avoid redundant workspace loads.

pub mod affected;
pub mod common;
pub mod init;
pub mod release;
pub mod split;
pub mod status;
pub mod sync;
pub mod test;
pub mod unify;
pub mod watch;

pub use affected::run_affected;
pub use init::{run_init, run_init_standalone};
pub use release::{run_release_check, run_release_plan, run_release_publish};
pub use split::run_split;
pub use status::run_status;
pub use sync::run_sync;
pub use test::run_test;
pub use unify::{run_unify_analyze, run_unify_apply};
pub use watch::run_test_watch;
