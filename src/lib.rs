//! Split Rust crates from Cargo workspaces into standalone repos with bidirectional sync.
//!
//! cargo-rail manages monorepo workflows: split crates into standalone repositories,
//! maintain bidirectional sync, unify dependencies, and coordinate releases.
//!
//! # Quick Start
//!
//! ```rust,no_run
//! use cargo_rail::{RailResult, workspace::WorkspaceContext};
//! use std::path::Path;
//!
//! # fn main() -> RailResult<()> {
//! // Load workspace context
//! let ctx = WorkspaceContext::build(Path::new("."))?;
//!
//! // Analyze workspace structure
//! let crates = ctx.cargo().packages();
//! assert!(!crates.is_empty());
//! # Ok(())
//! # }
//! ```
//!
//! # Modules
//!
//! - [`cargo`] — Workspace metadata, manifest transformation, dependency unification
//! - [`graph`] — Dependency graph analysis and affected crate detection
//! - [`workspace`] — Unified workspace context (Git + Cargo + Graph)
//! - [`split`] — Split crates into standalone repositories
//! - [`sync`] — Bidirectional sync between monorepo and split repos
//! - [`release`] — Coordinated release planning and publishing
//! - [`change_detection`] — Git-based change classification and impact analysis
//! - [`error`] — Error types with contextual help messages

#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

/// Cargo workspace integration and transformations
pub mod cargo;
pub mod change_detection;
pub mod commands;
/// Configuration file parsing and types
pub mod config;
pub mod error;
pub mod git;
pub mod graph;
pub mod release;
pub mod split;
pub mod sync;
/// Target triple detection for workspace validation
pub mod targets;
pub mod test;
pub mod utils;
pub mod workspace;

// Re-export commonly used types
pub use error::{RailError, RailResult};
