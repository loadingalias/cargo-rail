//! Graph-aware workspace analysis
//!
//! This module provides:
//! - Workspace dependency graph construction (petgraph-based)
//! - Affected crate analysis (changed files → impacted crates)
//! - Graph queries (transitive dependents, paths, cycles, etc.)
//!
//! Built on cargo_metadata + petgraph for direct control and minimal abstraction.
//! No guppy - we own our domain types and queries.

pub mod core;
pub mod query;

pub use core::WorkspaceGraph;
pub use query::{AffectedAnalysis, analyze};
