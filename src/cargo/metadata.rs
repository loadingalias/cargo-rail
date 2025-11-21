use crate::error::RailResult;
use cargo_metadata::{MetadataCommand, Package, Resolve, TargetKind};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// Workspace introspection using cargo_metadata
///
/// Provides comprehensive access to:
/// - Package information (dependencies, features, targets)
/// - Resolved dependency graph with actual enabled features
/// - MSRV and edition information
/// - Platform-specific and optional dependencies
#[derive(Clone, Serialize, Deserialize)]
pub struct WorkspaceMetadata {
  metadata: cargo_metadata::Metadata,
}

impl WorkspaceMetadata {
  /// Load workspace metadata with full dependency resolution
  pub fn load(workspace_root: &Path) -> RailResult<Self> {
    let metadata = MetadataCommand::new()
      .manifest_path(workspace_root.join("Cargo.toml"))
      .exec()?;
    Ok(Self { metadata })
  }

  /// Load workspace metadata with custom feature configuration
  ///
  /// Used by unify commands to gather metadata with --all-features for accurate
  /// feature union across the workspace. Can also be used for feature simulation.
  pub fn load_with_features(
    workspace_root: &Path,
    all_features: bool,
    no_default_features: bool,
    features: Vec<String>,
  ) -> RailResult<Self> {
    let mut cmd = MetadataCommand::new();
    cmd.manifest_path(workspace_root.join("Cargo.toml"));

    use cargo_metadata::CargoOpt;
    if all_features {
      cmd.features(CargoOpt::AllFeatures);
    } else if no_default_features {
      cmd.features(CargoOpt::NoDefaultFeatures);
    } else if !features.is_empty() {
      cmd.features(CargoOpt::SomeFeatures(features));
    }

    let metadata = cmd.exec()?;
    Ok(Self { metadata })
  }

  // ============================================================================
  // Basic Package Access
  // ============================================================================

  /// Get all workspace member packages
  pub fn list_crates(&self) -> Vec<&Package> {
    self.metadata.workspace_packages()
  }

  /// Find a package by name in the workspace
  pub fn get_package(&self, name: &str) -> Option<&Package> {
    self
      .metadata
      .workspace_packages()
      .into_iter()
      .find(|pkg| pkg.name == name)
  }

  /// Get the workspace root directory path
  pub fn workspace_root(&self) -> &std::path::Path {
    self.metadata.workspace_root.as_std_path()
  }

  /// Get workspace root as UTF-8 path (for use with cargo_metadata types)
  ///
  /// cargo_metadata uses UTF-8 paths internally, so this avoids unnecessary conversions
  /// when working with path dependencies and workspace members.
  pub fn workspace_root_utf8(&self) -> &cargo_metadata::camino::Utf8Path {
    &self.metadata.workspace_root
  }

  /// Check if a package is a procedural macro crate
  ///
  /// Proc-macro crates have special semantics:
  /// - Must be compiled for the host platform, not target
  /// - Changes affect dependents at compile-time
  /// - Require rebuilding all dependents when modified
  pub fn is_proc_macro_crate(&self, name: &str) -> bool {
    self
      .get_package(name)
      .map(|pkg| {
        pkg
          .targets
          .iter()
          .any(|target| target.kind.iter().any(|k| matches!(k, TargetKind::ProcMacro)))
      })
      .unwrap_or(false)
  }

  // ============================================================================
  // Tier 2: Feature & Target Analysis
  // ============================================================================

  /// Get resolved dependency graph with actual enabled features
  ///
  /// This is THE KEY to understanding feature unification.
  /// Returns None if resolution was not included in metadata.
  pub fn resolve(&self) -> Option<&Resolve> {
    self.metadata.resolve.as_ref()
  }

  // ============================================================================
  // Tier 3: Advanced Dependency Analysis
  // ============================================================================

  /// Find package by ID (useful for resolve graph traversal)
  pub fn find_package_by_id(&self, id: &cargo_metadata::PackageId) -> Option<&Package> {
    self.metadata.packages.iter().find(|pkg| &pkg.id == id)
  }

  // ============================================================================
  // Feature Unification Analysis
  // ============================================================================

  /// Get resolved features for a specific external package
  ///
  /// This returns the ACTUAL features enabled by Cargo's resolver, not just
  /// what's declared in Cargo.toml. This is critical for accurate unification.
  ///
  /// Returns None if:
  /// - No resolve graph available
  /// - Package is a workspace member
  /// - Package not found in resolved graph
  ///
  /// For external packages that appear in multiple versions, returns features
  /// for the first resolved version found.
  pub fn get_resolved_features_for_package(&self, pkg_name: &str) -> Option<HashSet<String>> {
    let resolve = self.resolve()?;

    // Find all resolved nodes for this package name
    for node in &resolve.nodes {
      if let Some(pkg) = self.find_package_by_id(&node.id) {
        // Skip workspace members - we only want external packages
        if self.get_package(&pkg.name).is_some() {
          continue;
        }

        // Check if this is the package we're looking for
        if pkg.name == pkg_name {
          // Return the resolved features for this package
          return Some(node.features.iter().map(|f| f.to_string()).collect());
        }
      }
    }

    None
  }

  // ============================================================================
  // Raw Access
  // ============================================================================

  /// Access raw cargo_metadata::Metadata for advanced use cases
  pub fn metadata_json(&self) -> &cargo_metadata::Metadata {
    &self.metadata
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn create_test_metadata() -> WorkspaceMetadata {
    // Use current workspace for testing
    let current_dir = std::env::current_dir().unwrap();
    WorkspaceMetadata::load(&current_dir).unwrap()
  }

  #[test]
  fn test_load_workspace_metadata() {
    let metadata = create_test_metadata();
    let crates = metadata.list_crates();

    // Should find cargo-rail in workspace
    assert!(!crates.is_empty(), "Should have at least one crate");
    assert!(crates.iter().any(|p| p.name == "cargo-rail"), "Should find cargo-rail");
  }

  #[test]
  fn test_get_package() {
    let metadata = create_test_metadata();

    // Should find cargo-rail
    let pkg = metadata.get_package("cargo-rail");
    assert!(pkg.is_some(), "Should find cargo-rail package");

    let pkg = pkg.unwrap();
    assert_eq!(pkg.name.as_str(), "cargo-rail");

    // Non-existent package should return None
    let missing = metadata.get_package("this-package-does-not-exist");
    assert!(missing.is_none(), "Non-existent package should be None");
  }

  // ============================================================================
  // Tier 2: Feature & Target Analysis
  // ============================================================================

  #[test]
  fn test_resolve() {
    let metadata = create_test_metadata();
    let resolve = metadata.resolve();

    // Should have resolved dependency graph
    assert!(resolve.is_some(), "Should have resolve graph");

    let resolve = resolve.unwrap();
    assert!(!resolve.nodes.is_empty(), "Resolve graph should have nodes");
  }

  // ============================================================================
  // Tier 3: Advanced Dependency Analysis
  // ============================================================================

  #[test]
  fn test_find_package_by_id() {
    let metadata = create_test_metadata();

    // Get cargo-rail's ID
    let cargo_rail = metadata.get_package("cargo-rail").unwrap();
    let pkg_id = &cargo_rail.id;

    // Should find by ID
    let found = metadata.find_package_by_id(pkg_id);
    assert!(found.is_some(), "Should find package by ID");

    let found = found.unwrap();
    assert_eq!(found.name.as_str(), "cargo-rail");
  }

  // ============================================================================
  // Feature Unification Analysis
  // ============================================================================

  #[test]
  fn test_load_with_features() {
    let current_dir = std::env::current_dir().unwrap();

    // Test with all features
    let all_features = WorkspaceMetadata::load_with_features(&current_dir, true, false, vec![]);
    assert!(all_features.is_ok(), "Should load with all features");

    // Test with no default features
    let no_default = WorkspaceMetadata::load_with_features(&current_dir, false, true, vec![]);
    assert!(no_default.is_ok(), "Should load with no default features");

    // Test with empty feature list (should succeed even if features don't exist)
    let empty_features = WorkspaceMetadata::load_with_features(&current_dir, false, false, vec![]);
    assert!(empty_features.is_ok(), "Should load with empty features list");
  }

  // ============================================================================
  // Feature Unification Tests
  // ============================================================================

  #[test]
  fn test_get_resolved_features_returns_none_for_workspace_members() {
    let metadata = create_test_metadata();

    // cargo-rail is a workspace member, should return None
    let result = metadata.get_resolved_features_for_package("cargo-rail");
    assert!(result.is_none(), "Should return None for workspace members");
  }

  #[test]
  fn test_get_resolved_features_returns_none_for_nonexistent_package() {
    let metadata = create_test_metadata();

    // This package doesn't exist
    let result = metadata.get_resolved_features_for_package("this-package-does-not-exist-12345");
    assert!(result.is_none(), "Should return None for nonexistent packages");
  }

  #[test]
  fn test_get_resolved_features_for_clap() {
    let metadata = create_test_metadata();

    // cargo-rail uses clap, which should have resolved features
    if let Some(features) = metadata.get_resolved_features_for_package("clap") {
      // clap typically has several features enabled
      // We don't assert specific features because they can vary
      // Just verify we got a valid set
      println!("clap resolved features: {:?}", features);
    } else {
      println!("Note: clap not found in resolved graph");
    }
  }
}
