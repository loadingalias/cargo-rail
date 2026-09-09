//! Workspace context and state management
//!
//! This module unifies optional Git, Cargo, graph, configuration, and one
//! explicitly requested immutable snapshot in a single `WorkspaceContext`.
//!
//! # Architecture
//!
//! WorkspaceContext = optional GitState + CargoState + DependencyGraph + Config + optional WorkspaceSnapshot
//!
//! Built once at startup, passed by reference to all commands.

use std::path::{Path, PathBuf};

/// Unified workspace context (includes optional GitState and CargoState)
pub mod context;
/// Immutable authoritative workspace capture.
pub mod snapshot;
/// High-level façade for crate information queries
pub mod view;

// Re-export workspace types from context module
pub use context::{CargoState, GitState, WorkspaceContext};
pub use snapshot::{
    LockedPackageIdentity, LockfileSnapshot, SnapshotFile, SnapshotId, SnapshotPackage, WorkspaceSnapshot,
};

// Re-export view types
pub use view::{CrateInfo, WorkspaceView};

/// Return the workspace-owned root for generated cargo-rail state.
pub(crate) fn cargo_rail_state_root(workspace_root: &Path) -> PathBuf {
    workspace_root.join("target").join("cargo-rail")
}

/// Resolve Cargo-owned filesystem paths once before structural consumers receive metadata.
/// Package IDs and compiler invocation spellings remain owned by Cargo.
pub(crate) fn capture_metadata_paths(
    mut metadata: cargo_metadata::Metadata,
) -> crate::error::RailResult<cargo_metadata::Metadata> {
    let mut paths = std::collections::HashMap::<PathBuf, String>::new();
    let mut resolve = |path: &Path| -> crate::error::RailResult<String> {
        if let Some(resolved) = paths.get(path) {
            return Ok(resolved.clone());
        }
        let resolved = crate::utils::canonicalize_allow_missing(path)?;
        let resolved = resolved.into_os_string().into_string().map_err(|_| {
            crate::error::RailError::message(format!("resolved Cargo path '{}' is not UTF-8", path.display()))
        })?;
        paths.insert(path.to_path_buf(), resolved.clone());
        Ok(resolved)
    };
    metadata.workspace_root = resolve(metadata.workspace_root.as_std_path())?.into();
    metadata.target_directory = resolve(metadata.target_directory.as_std_path())?.into();
    if let Some(path) = &mut metadata.build_directory {
        *path = resolve(path.as_std_path())?.into();
    }
    for package in &mut metadata.packages {
        package.manifest_path = resolve(package.manifest_path.as_std_path())?.into();
        for target in &mut package.targets {
            target.src_path = resolve(target.src_path.as_std_path())?.into();
        }
        for dependency in &mut package.dependencies {
            if let Some(path) = &mut dependency.path {
                *path = resolve(path.as_std_path())?.into();
            }
        }
    }
    Ok(metadata)
}
