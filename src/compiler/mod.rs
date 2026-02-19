//! Compiler diagnostics subsystem used by target-aware dependency analysis.

pub mod cfg_eval;
pub mod collector;
pub mod diagnostics_store;
pub mod model;

pub use collector::{CompilerDiagnosticsCollector, MemberDiagnostics};
