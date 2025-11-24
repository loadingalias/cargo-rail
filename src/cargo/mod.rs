//! Cargo workspace operations - CLEAN implementation
//!
//! This module provides all cargo-related functionality using the HYBRID approach:
//! - Multi-target metadata loading (parallel, cached)
//! - Manifest analysis for feature classification
//! - Clean unification with minimal features

// Core modules
pub mod cargo_transform;
pub mod manifest_analyzer;
pub mod manifest_ops;
pub mod manifest_writer;
pub mod multi_target_metadata;
pub mod unify_analyzer;

// Re-export main types for convenience
pub use cargo_transform::{CargoTransform, TransformContext};
pub use manifest_analyzer::{DepKey, DepKind, DepUsage, ManifestAnalyzer};
pub use manifest_writer::ManifestWriter;
pub use multi_target_metadata::{FragmentedTransitive, MultiTargetMetadata};
pub use unify_analyzer::{
  IssueSeverity, MemberEdit, UnificationPlan, UnifiedDep, UnifyAnalyzer, UnifyIssue, UnifyReport, ValidationResult,
};
