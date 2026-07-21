//! Compiler diagnostics subsystem used by target-aware dependency analysis.

pub mod cfg_eval;
pub mod collector;
pub mod diagnostics_store;
pub mod model;
pub(crate) mod observation;
/// Workspace-only rustc wrapper used by compiler diagnostics collection.
pub mod wrapper;

pub(crate) use collector::CompilerCacheIdentity;
pub use collector::{
  CompilerCandidate, CompilerDiagnosticsCollector, standalone_missing_features, verify_standalone_member,
};
pub use model::{DependencyEvidenceState, DependencyIdentity, FeatureSelection, MemberEvidence};
