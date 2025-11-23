//! Cargo workspace integration
//!
//! This module provides:
//! - Workspace metadata loading via cargo_metadata
//! - Cargo.toml manifest transformation (workspace flattening, path→version deps)
//! - Workspace dependency unification (eliminates workspace-hack crates)
//! - Optional per-target validation with parallel execution
//!
//! # Architecture
//!
//! - `metadata` - Comprehensive cargo_metadata wrapper with resolved feature analysis
//! - `manifest` - Lossless Cargo.toml transformation
//! - `unify` - Workspace dependency unification engine (replaces cargo-hakari)
//! - `validate` - Optional per-target validation with Rayon parallelism
/// Workspace dependency unification engine
pub mod manifest;
/// Comprehensive cargo_metadata wrapper
pub mod metadata;
pub mod unify;
/// Per-target validation with parallel execution
pub mod validate;

pub use manifest::{CargoTransform, TransformContext};
pub use metadata::WorkspaceMetadata;
pub use unify::{IssueSeverity, UnifiedDep, UnifyConfig, UnifyReport, WorkspaceUnifier};
pub use validate::validate_targets;
