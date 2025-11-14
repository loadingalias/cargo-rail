#![allow(dead_code)]

use crate::core::error::RailResult;
use cargo_metadata::{MetadataCommand, Package};
use std::path::Path;

/// Workspace introspection using cargo_metadata
#[derive(Clone)]
pub struct WorkspaceMetadata {
  metadata: cargo_metadata::Metadata,
}

impl WorkspaceMetadata {
  pub fn load(workspace_root: &Path) -> RailResult<Self> {
    let metadata = MetadataCommand::new()
      .manifest_path(workspace_root.join("Cargo.toml"))
      .exec()?;
    Ok(Self { metadata })
  }

  pub fn list_crates(&self) -> Vec<&Package> {
    self.metadata.workspace_packages()
  }

  pub fn get_package(&self, name: &str) -> Option<&Package> {
    self
      .metadata
      .workspace_packages()
      .into_iter()
      .find(|pkg| pkg.name == name)
  }

  pub fn workspace_root(&self) -> &std::path::Path {
    self.metadata.workspace_root.as_std_path()
  }

  /// Get raw JSON string for external tools
  pub fn to_json_string(&self) -> RailResult<String> {
    serde_json::to_string(&self.metadata)
      .map_err(|e| crate::core::error::RailError::message(format!("Failed to serialize metadata: {}", e)))
  }

  /// Access raw cargo_metadata::Metadata for graph construction
  pub fn metadata_json(&self) -> &cargo_metadata::Metadata {
    &self.metadata
  }
}
