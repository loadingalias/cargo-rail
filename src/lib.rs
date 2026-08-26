//! Rust monorepo orchestration: CI optimization, dependency unification, release automation.

#![cfg_attr(docsrs, feature(doc_cfg))]

/// Backup and restore for undo operations.
pub mod backup;
pub(crate) mod build_script;
pub(crate) mod cache;
/// Cargo workspace metadata and manifest operations.
pub mod cargo;
pub(crate) mod change_detection;
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
/// Diagnostic-only performance instrumentation.
#[doc(hidden)]
pub mod instrumentation;
/// Deterministic mutation plan/apply framework.
pub mod mutation;
/// Centralized output control (quiet mode, progress messages).
pub mod output;
pub(crate) mod planning;
/// Release planning and publishing.
pub mod release;
pub(crate) mod remote_cache;
/// Canonical Git-backed or filesystem-backed source state.
pub mod source;
/// Crate extraction to standalone repos.
pub mod split;
pub(crate) mod surface;
/// Bidirectional monorepo sync.
pub mod sync;
/// Target triple detection.
pub mod targets;
/// TOML editing utilities.
pub mod toml;
/// Shared utilities.
pub mod utils;
/// Audited Win32 primitives unavailable through stable `std`.
#[expect(unsafe_code, reason = "this audited module is the sole Win32 FFI boundary")]
#[cfg(windows)]
pub(crate) mod windows_fs;
/// Unified workspace context.
pub mod workspace;

// Re-export commonly used types
pub use error::{RailError, RailResult};
