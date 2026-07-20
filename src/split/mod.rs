//! Extract crates from monorepo to standalone repositories.
//!
//! This module provides:
//! - Deterministic git history extraction
//! - Commit filtering and recreation
//! - Cargo.toml transformations for split repos
//! - Git-native commit origin mapping

/// Split engine implementation
pub mod engine;
/// Repository-boundary validation for split and sync mutations.
pub mod paths;

pub use engine::{ReleaseBoundary, SplitEngine, SplitOwnership, SplitParams};
pub use paths::SplitPathCapabilities;
