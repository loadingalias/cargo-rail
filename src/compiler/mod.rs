//! Compiler diagnostics subsystem used by target-aware dependency analysis.

pub(crate) mod acquisition;
pub(crate) mod analysis;
pub(crate) mod capability;
pub mod cfg_eval;
pub mod collector;
pub mod diagnostics_store;
pub(crate) mod distributed;
pub(crate) mod driver;
pub(crate) mod fact_protocol;
pub(crate) mod fact_store;
pub(crate) mod facts;
/// Exact pre-Clap compiler process classification and execution.
pub mod invocation;
pub mod model;
pub(crate) mod native_cache;
pub(crate) mod observation;
pub(crate) mod operation;
pub(crate) mod scheduler;
pub(crate) mod session;

pub(crate) use collector::{
    CompilerAnalysisMetrics, CompilerArtifactBudget, CompilerCacheIdentity, CompilerDiagnosticsCollector,
};
pub use collector::{standalone_missing_features, verify_standalone_member};
pub use model::{CoverageView, DependencyEvidenceState, DependencyIdentity, FeatureSelection, MemberEvidence};
pub use scheduler::CompilerCandidate;
