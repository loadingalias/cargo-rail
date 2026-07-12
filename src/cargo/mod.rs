//! Cargo workspace operations - CLEAN implementation
//!
//! This module provides all cargo-related functionality using the HYBRID approach:
//! - Multi-target metadata loading (parallel, cached)
//! - Manifest analysis for feature classification
//! - Clean unification with minimal features

// Core modules
pub mod cargo_transform;
pub mod feature_scanner;
pub mod manifest_analyzer;
mod manifest_ops; // Internal - used only by manifest_writer and cargo_transform
pub mod manifest_writer;
pub mod multi_target_metadata;
mod unify; // Internal - implementation details for UnifyAnalyzer
pub mod unify_analyzer;
pub mod unify_report;
pub mod unify_types;

// Re-export main types for convenience
pub use cargo_transform::{CargoTransform, TransformContext};
pub use feature_scanner::{FeatureScanResult, FeatureScanner};
pub use manifest_analyzer::{DepKey, DepKind, DepUsage, ManifestAnalyzer};
pub use manifest_writer::ManifestWriter;
pub use multi_target_metadata::{ComputedMsrv, FragmentedTransitive, MsrvSourceUsed, MultiTargetMetadata};
pub use unify_analyzer::UnifyAnalyzer;
pub use unify_report::UnifyReport;
pub use unify_types::{
  DependencyProof, DuplicateCleanup, IssueSeverity, MemberEdit, OptionalFeature, PrunedFeature, ReachableFeature,
  TransitivePin, UndeclaredFeature, UnificationPlan, UnifiedDep, UnifyDecision, UnifyDecisionCode, UnifyDecisionReason,
  UnifyDecisionSubject, UnifyIssue, UnusedDep, UnusedReason, ValidationResult, VersionMismatch,
};
