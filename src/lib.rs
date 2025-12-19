//! Rust monorepo orchestration: CI optimization, dependency unification, release automation.

#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

/// Backup and restore for undo operations.
pub mod backup;
/// Cargo workspace metadata and manifest operations.
pub mod cargo;
/// Git-based change classification.
pub mod change_detection;
/// CLI command implementations.
pub mod commands;
/// Configuration file parsing.
pub mod config;
/// Error types and result aliases.
pub mod error;
/// Git operations via system git.
pub mod git;
/// Dependency graph analysis.
pub mod graph;
/// Centralized output control (quiet mode, progress messages).
pub mod output;
/// Release planning and publishing.
pub mod release;
/// Crate extraction to standalone repos.
pub mod split;
/// Bidirectional monorepo sync.
pub mod sync;
/// Target triple detection.
pub mod targets;
/// Test runner integration.
pub mod test;
/// TOML editing utilities.
pub mod toml;
/// Shared utilities.
pub mod utils;
/// Unified workspace context.
pub mod workspace;

// Re-export commonly used types
pub use error::{RailError, RailResult};
