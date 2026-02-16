//! Extract crates from monorepo to standalone repositories.
//!
//! This module provides:
//! - Deterministic git history extraction
//! - Commit filtering and recreation
//! - Cargo.toml transformations for split repos
//! - Git-notes based commit mapping

/// Split engine implementation
pub mod engine;

pub use engine::{SplitEngine, SplitParams};
