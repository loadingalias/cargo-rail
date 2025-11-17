//! Auditable plan and execution system.
//!
//! This module provides:
//! - Plan types for dry-run capability
//! - PlanExecutor for deterministic execution
//! - SHA256-based plan IDs for caching

mod executor;
mod plan;

pub use executor::PlanExecutor;
pub use plan::{Plan, PlanId, Operation, OperationType};
