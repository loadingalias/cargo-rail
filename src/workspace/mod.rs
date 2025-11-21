//! Workspace context and state management
//!
//! This module unifies Git, Cargo, and Graph into a single WorkspaceContext.
//!
//! # Architecture
//!
//! WorkspaceContext = GitState + CargoState + DependencyGraph + Config
//!
//! Built once at startup, passed by reference to all commands.

/// Cargo workspace state management
pub mod cargo_state;
/// Change impact analysis
pub mod change_analyzer;
/// Unified workspace context
pub mod context;
/// File path utilities
pub mod files;
/// Git repository state management
pub mod git_state;

// Re-export change analysis types
pub use change_analyzer::ChangeImpact;

pub use cargo_state::CargoState;
pub use context::WorkspaceContext;
pub use git_state::GitState;
