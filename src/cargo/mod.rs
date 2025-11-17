//! Cargo workspace integration
//!
//! This module provides:
//! - Workspace metadata loading via cargo_metadata
//! - Feature unification and fragmentation analysis
//! - Cargo.toml manifest transformation (workspace flattening, path→version deps)
//!
//! # Architecture
//!
//! - `metadata` - Comprehensive cargo_metadata wrapper with Tier 2/3 capabilities
//! - `features` - Feature fragmentation detection and unification suggestions
//! - `manifest` - Lossless Cargo.toml transformation

pub mod features;
pub mod manifest;
pub mod metadata;

pub use manifest::{CargoTransform, TransformContext};
pub use metadata::WorkspaceMetadata;
