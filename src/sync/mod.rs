//! Bidirectional sync between monorepo and split repositories.
//!
//! This module provides:
//! - Bidirectional change synchronization
//! - Conflict detection and resolution strategies
//! - Merge strategies (ours, theirs, manual, union)

/// Conflict detection and resolution strategies
pub mod conflict;
/// Sync engine implementation
pub mod engine;

pub use conflict::ConflictStrategy;
pub use engine::{SyncConfig, SyncDirection, SyncEngine};
