//! Cargo workspace integration.
//!
//! This module provides:
//! - Workspace metadata loading via cargo_metadata
//! - WorkspaceContext for single-load pattern
//! - Cargo.toml transformations (workspace flattening, path→version deps)
//! - Auxiliary file handling

pub mod context;
pub mod files;
pub mod metadata;
pub mod transform;

pub use context::WorkspaceContext;
pub use files::{AuxiliaryFiles, ProjectFiles};
pub use metadata::WorkspaceMetadata;
pub use transform::CargoTransform;
