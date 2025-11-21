//! Git operations and commit mapping storage.
//!
//! This module provides:
//! - SystemGit backend using system git binary
//! - Git-notes based commit mapping (rebase-safe)
//! - Low-level git operations (rev-parse, cat-file, push, pull, etc.)
//! - Smart defaults for git references

/// Smart defaults for git references
pub mod defaults;
/// Git-notes based commit mapping storage
pub mod mappings;
/// Git operations (commit, branch, push, pull, etc.)
pub mod ops;
/// SystemGit backend using system git binary
pub mod system;

pub use defaults::detect_default_base_ref;
pub use system::{CommitInfo, SystemGit};
