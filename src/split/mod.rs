//! Extract crates from monorepo to standalone repositories.
//!
//! This module provides:
//! - Deterministic git history extraction
//! - Commit filtering and recreation
//! - Cargo.toml transformations for split repos
//! - Git-notes based commit mapping

/// Split engine implementation
pub mod engine;
/// Repository-boundary validation for split and sync mutations.
pub mod paths;

pub use engine::{SplitEngine, SplitParams};
pub use paths::SplitPathCapabilities;
