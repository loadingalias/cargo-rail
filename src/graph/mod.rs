//! Graph-aware workspace analysis.
//!
//! This module provides:
//! - Workspace dependency graph construction
//! - Affected crate analysis (changed files → impacted crates)
//! - Graph queries (transitive dependents, paths, etc.)
//!
//! Built on cargo_metadata + petgraph for direct control and minimal abstraction.
//! No guppy - we own our domain types and queries.

pub mod builder;
pub mod query;

pub use query::{AffectedAnalysis, analyze};
