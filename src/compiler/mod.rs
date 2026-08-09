//! Compiler diagnostics subsystem used by target-aware dependency analysis.

pub mod cfg_eval;
pub mod collector;
pub mod diagnostics_store;
/// Exact pre-Clap compiler process classification and execution.
pub mod invocation;
pub mod model;
pub(crate) mod native_cache;
pub(crate) mod observation;
pub(crate) mod session;

pub(crate) use collector::CompilerCacheIdentity;
pub use collector::{
  CompilerCandidate, CompilerDiagnosticsCollector, standalone_missing_features, verify_standalone_member,
};
pub use model::{DependencyEvidenceState, DependencyIdentity, FeatureSelection, MemberEvidence};
