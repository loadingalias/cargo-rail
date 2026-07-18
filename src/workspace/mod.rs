//! Workspace context and state management
//!
//! This module unifies optional Git, Cargo, graph, configuration, and one
//! explicitly requested immutable snapshot in a single `WorkspaceContext`.
//!
//! # Architecture
//!
//! WorkspaceContext = optional GitState + CargoState + DependencyGraph + Config + optional WorkspaceSnapshot
//!
//! Built once at startup, passed by reference to all commands.

use std::path::{Path, PathBuf};

/// Change impact analysis
pub mod change_analyzer;
/// Unified workspace context (includes optional GitState and CargoState)
pub mod context;
/// File path utilities
pub mod files;
/// Immutable authoritative workspace capture.
pub mod snapshot;
/// High-level façade for crate information queries
pub mod view;

// Re-export change analysis types
pub use change_analyzer::ChangeImpact;

// Re-export workspace types from context module
pub use context::{CargoState, GitState, WorkspaceContext};
pub use snapshot::{
  LockedPackageIdentity, LockfileSnapshot, SnapshotFile, SnapshotId, SnapshotPackage, WorkspaceSnapshot,
};

// Re-export view types
pub use view::{CrateInfo, WorkspaceView};

/// Return the workspace-owned root for generated cargo-rail state.
pub(crate) fn cargo_rail_state_root(workspace_root: &Path) -> PathBuf {
  workspace_root.join("target").join("cargo-rail")
}
