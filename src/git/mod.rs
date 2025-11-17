//! Git operations and commit mapping storage.
//!
//! This module provides:
//! - SystemGit backend using system git binary
//! - Git-notes based commit mapping (rebase-safe)
//! - Low-level git operations (rev-parse, cat-file, push, pull, etc.)

pub mod mappings;
pub mod ops;
pub mod system;

pub use mappings::MappingStore;
pub use system::{SystemGit, CommitInfo};
