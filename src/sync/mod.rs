//! Bidirectional sync between monorepo and split repositories.
//!
//! This module provides:
//! - Bidirectional change synchronization
//! - Conflict detection and resolution strategies
//! - Merge strategies (ours, theirs, manual, union)

pub mod conflict;
pub mod engine;

pub use conflict::ConflictStrategy;
pub use engine::{SyncEngine, SyncDirection};
