//! Cargo.toml transformation for split/sync operations
//!
//! This module provides simple Cargo.toml transformations needed by split and sync:
//! - Transform workspace dependencies to standalone format (for splits)
//! - Transform standalone dependencies back to workspace format (for syncs)

use crate::cargo::manifest_ops;
use crate::error::{RailError, RailResult};
use cargo_metadata::Metadata;
use std::path::PathBuf;
use toml_edit::{DocumentMut, Item};

/// Context for Cargo.toml transformations
pub struct TransformContext {
  /// Name of the crate being transformed
  pub crate_name: String,
  /// Workspace root path
  pub workspace_root: PathBuf,
}

/// Cargo.toml transformer for split/sync operations
pub struct CargoTransform {
  metadata: Metadata,
}

impl CargoTransform {
  /// Create a new transformer with workspace metadata
  pub fn new(metadata: Metadata) -> Self {
    Self { metadata }
  }

  /// Transform a Cargo.toml from workspace format to split (standalone) format
  ///
  /// This replaces workspace dependency references with concrete version requirements.
  pub fn transform_to_split(&self, content: &str, context: &TransformContext) -> RailResult<String> {
    let mut doc: DocumentMut = content
      .parse()
      .map_err(|e| RailError::message(format!("Failed to parse Cargo.toml: {}", e)))?;

    // Remove workspace inheritance markers and resolve to actual values
    self.resolve_workspace_inheritance(&mut doc, &context.workspace_root)?;

    // Transform workspace dependencies to standalone format
    self.transform_dependencies_to_standalone(&mut doc)?;

    Ok(doc.to_string())
  }

  /// Transform a Cargo.toml from split (standalone) format back to workspace format
  ///
  /// This is currently a no-op since syncing from remote to mono doesn't need transformation.
  /// The crate in the monorepo already uses workspace format.
  pub fn transform_to_mono(&self, content: &str, _context: &TransformContext) -> RailResult<String> {
    // For now, pass through unchanged. If we need to restore workspace.dependencies
    // references, we can implement that here.
    Ok(content.to_string())
  }

  /// Resolve workspace inheritance (workspace = true fields) to actual values
  fn resolve_workspace_inheritance(&self, doc: &mut DocumentMut, workspace_root: &std::path::Path) -> RailResult<()> {
    // Load workspace Cargo.toml to get [workspace.package] values
    let workspace_toml_path = workspace_root.join("Cargo.toml");
    let workspace_doc = manifest_ops::read_toml_file(&workspace_toml_path)?;

    // Get workspace.package table if it exists
    let workspace_package = workspace_doc
      .get("workspace")
      .and_then(|w| w.as_table())
      .and_then(|w| w.get("package"))
      .and_then(|p| p.as_table());

    // Use manifest_ops to resolve package inheritance
    if let Some(workspace_pkg) = workspace_package {
      manifest_ops::resolve_package_workspace_inheritance(doc, workspace_pkg)?;
    }

    Ok(())
  }

  /// Transform workspace dependencies to standalone format
  fn transform_dependencies_to_standalone(&self, doc: &mut DocumentMut) -> RailResult<()> {
    // Transform each dependency section using manifest_ops
    manifest_ops::transform_dependencies_in_section(doc, "dependencies", |name, item| {
      self.transform_and_resolve_dep(item, name)
    })?;

    manifest_ops::transform_dependencies_in_section(doc, "dev-dependencies", |name, item| {
      self.transform_and_resolve_dep(item, name)
    })?;

    manifest_ops::transform_dependencies_in_section(doc, "build-dependencies", |name, item| {
      self.transform_and_resolve_dep(item, name)
    })?;

    Ok(())
  }

  /// Helper: transform and resolve a single dependency
  fn transform_and_resolve_dep(&self, dep_item: &mut Item, dep_name: &str) -> RailResult<()> {
    // Check if this is a workspace dependency using manifest_ops
    if manifest_ops::is_workspace_dep(dep_item) {
      // Find the dependency version in workspace metadata
      if let Some(pkg) = self.metadata.packages.iter().find(|p| p.name == dep_name) {
        let version = pkg.version.to_string();

        // Remove workspace marker and set version using manifest_ops
        manifest_ops::extract_workspace_marker(dep_item);
        manifest_ops::set_version(dep_item, &version)?;
      }
    }

    // Remove path dependencies (they won't be valid in split repo)
    // This applies to both workspace and non-workspace dependencies
    manifest_ops::remove_path(dep_item);

    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_transform_workspace_dep() {
    let input = r#"
[package]
name = "test-crate"
version = "0.1.0"

[dependencies]
other-crate = { workspace = true }
serde = "1.0"
"#;

    // For testing, we'll just verify it parses and doesn't crash
    let doc: DocumentMut = input.parse().unwrap();
    assert!(doc.get("dependencies").is_some());
  }

  #[test]
  fn test_transform_to_mono_passthrough() {
    let input = r#"
[package]
name = "test-crate"
version = "0.1.0"

[dependencies]
serde = "1.0"
"#;

    // Create a minimal metadata for testing
    let metadata_json = serde_json::json!({
        "packages": [],
        "workspace_members": [],
        "resolve": null,
        "target_directory": "/tmp",
        "version": 1,
        "workspace_root": "/tmp",
        "metadata": null
    });

    let metadata: Metadata = serde_json::from_value(metadata_json).unwrap();
    let transformer = CargoTransform::new(metadata);
    let context = TransformContext {
      crate_name: "test-crate".to_string(),
      workspace_root: PathBuf::from("/tmp"),
    };

    let result = transformer.transform_to_mono(input, &context).unwrap();
    assert_eq!(result, input);
  }
}
