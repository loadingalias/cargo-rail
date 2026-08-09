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
pub(crate) mod impact;
pub mod query;
pub(crate) mod universe;

pub use core::WorkspaceGraph;
pub(crate) use impact::{ImpactDomain, ImpactFallback, ImpactPropagation, ImpactStep};
pub use query::{AffectedAnalysis, analyze};
pub(crate) use universe::{DEPENDENCY_UNIVERSE_MODE, DependencyUniverse};
