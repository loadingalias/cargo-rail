//! Advanced change detection and classification
//!
//! This module provides intelligent file change classification using:
//! - Path-based pattern matching (fast, no I/O)
//! - Cargo metadata for proc-macro detection
//!
//! # Architecture
//!
//! 1. `classify` - File classification by type and impact
//!
//! Note: Change impact analysis (ChangeImpact) is in `workspace` module
//! as it requires workspace context and dependency graph.

pub mod classify;

// Classification types are public API
