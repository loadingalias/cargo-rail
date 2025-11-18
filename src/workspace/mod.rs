//! Workspace context and state management
//!
//! This module unifies Git, Cargo, and Graph into a single WorkspaceContext.
//!
//! # Architecture
//!
//! WorkspaceContext = GitState + CargoState + DependencyGraph + Config
//!
//! Built once at startup, passed by reference to all commands.

pub mod cargo_state;
pub mod change_impact;
pub mod context;
pub mod files;
pub mod git_state;

pub use cargo_state::CargoState;
#[allow(unused_imports)] // Public API exports
pub use change_impact::{ChangeCategories, ChangeImpact, ImpactReport};
pub use context::WorkspaceContext;
pub use git_state::GitState;
