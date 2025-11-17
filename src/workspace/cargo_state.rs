//! Cargo workspace state wrapper
//!
//! Thin wrapper around cargo_metadata providing workspace-level cargo operations.
//! Built once at workspace context initialization, passed by reference.

use crate::cargo::WorkspaceMetadata;
use crate::error::RailResult;
use cargo_metadata::Package;
use std::path::{Path, PathBuf};

/// Cargo state for the workspace
///
/// Provides cargo metadata and workspace information.
/// This is built once and shared across all commands via WorkspaceContext.
#[derive(Clone)]
pub struct CargoState {
  /// Underlying cargo metadata
  metadata: WorkspaceMetadata,

  /// Cached workspace root
  workspace_root: PathBuf,
}

impl CargoState {
  /// Load cargo metadata from workspace root
  pub fn load(workspace_root: &Path) -> RailResult<Self> {
    let metadata = WorkspaceMetadata::load(workspace_root)?;
    let workspace_root = metadata.workspace_root().to_path_buf();

    Ok(Self {
      metadata,
      workspace_root,
    })
  }

  /// Get workspace root path
  pub fn workspace_root(&self) -> &Path {
    &self.workspace_root
  }

  /// Access underlying WorkspaceMetadata for advanced operations
  ///
  /// Use this when you need direct access to cargo_metadata types.
  pub fn metadata(&self) -> &WorkspaceMetadata {
    &self.metadata
  }

  /// Get all workspace member packages
  pub fn workspace_packages(&self) -> Vec<&Package> {
    self.metadata.list_crates()
  }

  /// Get package by name
  pub fn get_package(&self, name: &str) -> Option<&Package> {
    self.metadata.get_package(name)
  }
}
