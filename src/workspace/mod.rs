//! Workspace context and state management
//!
//! This module unifies Git, Cargo, and Graph into a single WorkspaceContext.
//!
//! # Architecture
//!
//! WorkspaceContext = GitState + CargoState + DependencyGraph + Config
//!
//! Built once at startup, passed by reference to all commands.

/// Change impact analysis
pub mod change_analyzer;
/// Unified workspace context (includes GitState and CargoState)
pub mod context;
/// File path utilities
pub mod files;

// Re-export change analysis types
pub use change_analyzer::ChangeImpact;

// Re-export workspace types from context module
pub use context::{CargoState, GitState, WorkspaceContext};
