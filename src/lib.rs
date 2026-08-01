//! Rust monorepo orchestration: CI optimization, dependency unification, release automation.

#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

pub(crate) mod action;
pub(crate) mod action_key;
/// Backup and restore for undo operations.
pub mod backup;
pub(crate) mod build_script;
pub(crate) mod cache;
/// Cargo workspace metadata and manifest operations.
pub mod cargo;
/// Git-based change classification.
pub mod change_detection;
/// CLI command implementations.
pub mod commands;
/// Compiler diagnostics collection and caching.
pub mod compiler;
/// Configuration file parsing.
pub mod config;
/// Error types and result aliases.
pub mod error;
pub(crate) mod executable;
/// Git operations via system git.
pub mod git;
/// Dependency graph analysis.
pub mod graph;
pub(crate) mod hermetic;
/// Diagnostic-only performance instrumentation.
#[doc(hidden)]
pub mod instrumentation;
/// Deterministic mutation plan/apply framework.
pub mod mutation;
/// Centralized output control (quiet mode, progress messages).
pub mod output;
/// Release planning and publishing.
pub mod release;
/// Canonical Git-backed or filesystem-backed source state.
pub mod source;
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
