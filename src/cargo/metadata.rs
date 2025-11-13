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
}
