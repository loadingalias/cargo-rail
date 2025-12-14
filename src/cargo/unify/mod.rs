//! Dependency unification analysis and planning
//!
//! This module contains the core unification logic, split into focused components:
//! - `version_utils` - Pure version parsing and comparison utilities
//! - `version_planner` - Version resolution, exact pins, conflict detection
//! - `feature_planner` - Feature intersection/union, default-features policy
//! - `transitive_planner` - Fragmented transitive dependency detection & pins
//! - `feature_pruner` - Dead/optional feature scanning
//! - `unused_finder` - Unused dependency detection
//! - `iteration` - CandidateIterator with ByDepKey/ByPackage strategies

// Submodules
pub mod feature_pruner;
pub mod iteration;
pub mod transitive_planner;
pub mod unused_finder;
pub mod version_utils;

// Re-export main types
pub use feature_pruner::FeaturePruner;
pub use iteration::CandidateIterator;
pub use transitive_planner::TransitivePlanner;
pub use unused_finder::UnusedDepFinder;
