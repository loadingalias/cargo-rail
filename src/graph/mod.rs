//! Graph-aware workspace analysis
//!
//! Built on cargo_metadata + petgraph for direct control and minimal abstraction.
//! No guppy - we own our domain types and queries.

pub mod affected;
pub mod workspace_graph;

pub use affected::AffectedAnalysis;
pub use workspace_graph::WorkspaceGraph;
