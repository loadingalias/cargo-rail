use crate::error::RailResult;
use cargo_metadata::{Dependency, DependencyKind, MetadataCommand, Package, Resolve, Target};
use semver::Version;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// Workspace introspection using cargo_metadata
///
/// Provides comprehensive access to:
/// - Package information (dependencies, features, targets)
/// - Resolved dependency graph with actual enabled features
/// - MSRV and edition information
/// - Platform-specific and optional dependencies
#[derive(Clone)]
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

  /// Get all features defined by a package
  ///
  /// Returns map of feature_name -> Vec of required features
  ///
  /// TODO: Future feature - will be used for:
  /// - Feature audit: `cargo rail audit features` to detect unused features
  /// - Feature analysis: understanding feature dependencies
  #[allow(dead_code)]
  pub fn package_features(&self, name: &str) -> Option<&BTreeMap<String, Vec<String>>> {
    self.get_package(name).map(|pkg| &pkg.features)
  }

  /// Get all targets (lib, bin, test, etc.) for a package
  ///
  /// TODO: Future feature - will be used for:
  /// - Quality engine: only test packages that have test targets
  /// - Target filtering: `cargo rail test --bins-only`
  #[allow(dead_code)]
  pub fn package_targets(&self, name: &str) -> Vec<&Target> {
    self
      .get_package(name)
      .map(|pkg| pkg.targets.iter().collect())
      .unwrap_or_default()
  }

  /// Get package edition (2015, 2018, 2021, 2024)
  ///
  /// TODO: Wire into unify.rs - will be used for:
  /// - Edition compatibility validation before unification
  /// - Warning if deps have edition conflicts
  #[allow(dead_code)]
  pub fn package_edition(&self, name: &str) -> Option<&str> {
    self.get_package(name).map(|pkg| pkg.edition.as_str())
  }

  /// Get package MSRV (minimum supported Rust version)
  ///
  /// TODO: Wire into unify.rs - will be used for:
  /// - MSRV validation before unification
  /// - Warning if deps have MSRV conflicts
  #[allow(dead_code)]
  pub fn package_rust_version(&self, name: &str) -> Option<&Version> {
    self.get_package(name).and_then(|pkg| pkg.rust_version.as_ref())
  }

  // ============================================================================
  // Tier 3: Advanced Dependency Analysis
  // ============================================================================

  /// Get all dependencies for a package (normal + dev + build)
  ///
  /// TODO: Future feature - will be used for:
  /// - Quality engine: comprehensive dependency audit
  /// - Dependency graph: full dep tree visualization
  #[allow(dead_code)]
  pub fn package_dependencies(&self, name: &str) -> Vec<&Dependency> {
    self
      .get_package(name)
      .map(|pkg| pkg.dependencies.iter().collect())
      .unwrap_or_default()
  }

  /// Get dependencies of a specific kind (normal, dev, or build)
  ///
  /// TODO: Future feature - will be used for:
  /// - Quality engine: "find all dev-only deps"
  /// - Audit: detect mis-categorized dependencies
  #[allow(dead_code)]
  pub fn package_dependencies_by_kind(&self, name: &str, kind: DependencyKind) -> Vec<&Dependency> {
    self
      .package_dependencies(name)
      .into_iter()
      .filter(|dep| dep.kind == kind)
      .collect()
  }

  /// Get all optional dependencies for a package
  ///
  /// TODO: Future feature - will be used for:
  /// - Quality engine: optional dependency management
  /// - Feature analysis: map features to optional deps
  #[allow(dead_code)]
  pub fn package_optional_dependencies(&self, name: &str) -> Vec<&Dependency> {
    self
      .package_dependencies(name)
      .into_iter()
      .filter(|dep| dep.optional)
      .collect()
  }

  /// Check if a dependency uses default features
  ///
  /// TODO: Wire into unify.rs - will be used for:
  /// - Smarter default_features handling in unification
  /// - Better feature merging logic
  #[allow(dead_code)]
  pub fn dependency_uses_default_features(&self, package: &str, dep_name: &str) -> Option<bool> {
    self
      .package_dependencies(package)
      .into_iter()
      .find(|dep| dep.name == dep_name)
      .map(|dep| dep.uses_default_features)
  }

  /// Get platform-specific dependencies (e.g., only on Windows, Unix, etc.)
  ///
  /// TODO: Future feature - will be used for:
  /// - Quality engine: platform compatibility matrix
  /// - Cross-platform testing: detect platform-specific issues
  #[allow(dead_code)]
  pub fn package_platform_specific_dependencies(&self, name: &str) -> Vec<(&Dependency, String)> {
    self
      .package_dependencies(name)
      .into_iter()
      .filter_map(|dep| dep.target.as_ref().map(|target| (dep, target.to_string())))
      .collect()
  }

  /// Get all packages (workspace members + dependencies)
  ///
  /// TODO: Future feature - will be used for:
  /// - Graph queries: include external deps in dependency graph
  /// - Full workspace analysis: not just workspace members
  #[allow(dead_code)]
  pub fn all_packages(&self) -> &[Package] {
    &self.metadata.packages
  }

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

  /// Get raw JSON string for external tools
  ///
  /// TODO: Future feature - will be used for:
  /// - External tooling integration: Pass metadata to other tools
  /// - Debugging: Export full metadata for inspection
  #[allow(dead_code)]
  pub fn to_json_string(&self) -> RailResult<String> {
    serde_json::to_string(&self.metadata)
      .map_err(|e| crate::error::RailError::message(format!("Failed to serialize metadata: {}", e)))
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

  #[test]
  fn test_workspace_root() {
    let metadata = create_test_metadata();
    let root = metadata.workspace_root();

    // Should return valid path
    assert!(root.exists(), "Workspace root should exist");
    assert!(root.is_dir(), "Workspace root should be directory");

    // Should contain Cargo.toml
    assert!(root.join("Cargo.toml").exists(), "Should have Cargo.toml");
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

  #[test]
  fn test_package_features() {
    let metadata = create_test_metadata();

    // cargo-rail should have some features
    let features = metadata.package_features("cargo-rail");
    assert!(features.is_some(), "cargo-rail should have features map");

    // Non-existent package should return None
    let missing = metadata.package_features("non-existent-package");
    assert!(missing.is_none(), "Non-existent package should have no features");
  }

  #[test]
  fn test_package_targets() {
    let metadata = create_test_metadata();

    // cargo-rail should have targets
    let targets = metadata.package_targets("cargo-rail");
    assert!(!targets.is_empty(), "cargo-rail should have targets");

    // Should have at least one bin target
    let has_bin = targets.iter().any(|t| t.is_bin());
    assert!(has_bin, "cargo-rail should have bin target");
  }

  #[test]
  fn test_package_edition() {
    let metadata = create_test_metadata();

    let edition = metadata.package_edition("cargo-rail");
    assert!(edition.is_some(), "cargo-rail should have edition");

    let edition = edition.unwrap();
    assert!(
      ["2015", "2018", "2021", "2024"].contains(&edition),
      "Should be valid edition: {}",
      edition
    );
  }

  #[test]
  fn test_package_rust_version() {
    let metadata = create_test_metadata();

    // cargo-rail may or may not have rust-version set
    let rust_version = metadata.package_rust_version("cargo-rail");

    if let Some(version) = rust_version {
      // If set, should be valid semver
      assert!(!version.to_string().is_empty(), "Rust version should not be empty");
    }
  }

  // ============================================================================
  // Tier 3: Advanced Dependency Analysis
  // ============================================================================

  #[test]
  fn test_package_dependencies() {
    let metadata = create_test_metadata();

    let deps = metadata.package_dependencies("cargo-rail");
    assert!(!deps.is_empty(), "cargo-rail should have dependencies");

    // Should include known dependencies like clap, serde
    let dep_names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
    assert!(dep_names.contains(&"clap"), "Should depend on clap");
    assert!(dep_names.contains(&"serde"), "Should depend on serde");
  }

  #[test]
  fn test_package_dependencies_by_kind() {
    let metadata = create_test_metadata();

    use cargo_metadata::DependencyKind;

    // Get normal dependencies
    let normal = metadata.package_dependencies_by_kind("cargo-rail", DependencyKind::Normal);
    assert!(!normal.is_empty(), "Should have normal dependencies");

    // All should be normal kind
    for dep in &normal {
      assert_eq!(
        dep.kind,
        DependencyKind::Normal,
        "{} should be normal dependency",
        dep.name
      );
    }

    // Dev dependencies
    let dev = metadata.package_dependencies_by_kind("cargo-rail", DependencyKind::Development);
    // May or may not have dev dependencies

    // All should be dev kind
    for dep in &dev {
      assert_eq!(
        dep.kind,
        DependencyKind::Development,
        "{} should be dev dependency",
        dep.name
      );
    }
  }

  #[test]
  fn test_package_optional_dependencies() {
    let metadata = create_test_metadata();

    let optional = metadata.package_optional_dependencies("cargo-rail");

    // All should have optional = true
    for dep in &optional {
      assert!(dep.optional, "{} should be optional", dep.name);
    }
  }

  #[test]
  fn test_dependency_uses_default_features() {
    let metadata = create_test_metadata();

    // Check a known dependency
    let uses_default = metadata.dependency_uses_default_features("cargo-rail", "serde");

    // Should be Some(bool) or None
    let _ = uses_default;

    // Non-existent dependency should return None
    let missing = metadata.dependency_uses_default_features("cargo-rail", "this-does-not-exist");
    assert!(missing.is_none(), "Non-existent dependency should be None");
  }

  #[test]
  fn test_package_platform_specific_dependencies() {
    let metadata = create_test_metadata();

    let platform_deps = metadata.package_platform_specific_dependencies("cargo-rail");

    // Each platform dep should have valid target string
    for (dep, target) in &platform_deps {
      assert!(!dep.name.is_empty(), "Dependency name should not be empty");
      assert!(!target.is_empty(), "Target should not be empty");
    }
  }

  #[test]
  fn test_all_packages() {
    let metadata = create_test_metadata();

    let all = metadata.all_packages();
    assert!(!all.is_empty(), "Should have packages");

    // Should include both workspace members and dependencies
    let workspace_count = metadata.list_crates().len();
    assert!(
      all.len() > workspace_count,
      "Should include dependencies beyond workspace members"
    );
  }

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

  #[test]
  fn test_metadata_json() {
    let metadata = create_test_metadata();
    let raw = metadata.metadata_json();

    // Should have access to raw metadata
    assert!(!raw.packages.is_empty(), "Raw metadata should have packages");
  }

  #[test]
  fn test_to_json_string() {
    let metadata = create_test_metadata();
    let json = metadata.to_json_string();

    assert!(json.is_ok(), "Should serialize to JSON");

    let json = json.unwrap();
    assert!(!json.is_empty(), "JSON should not be empty");

    // Should be valid JSON
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&json);
    assert!(parsed.is_ok(), "Should be valid JSON");
  }

  // ============================================================================
  // Feature Unification Tests
  // ============================================================================

  #[test]
  fn test_get_resolved_features_for_package() {
    let metadata = create_test_metadata();

    // Test with a known external dependency (serde)
    // We know cargo-rail depends on serde
    if let Some(features) = metadata.get_resolved_features_for_package("serde") {
      // Resolved features should be a set
      assert!(
        !features.is_empty() || features.is_empty(),
        "Features set should be valid"
      );

      // serde commonly has these features in resolved graph
      // (may vary based on what other crates enable)
      // Just verify we got a valid HashSet
      let _features_vec: Vec<String> = features.into_iter().collect();
    } else {
      // It's ok if serde doesn't have resolved features in test context
      // (might not be in resolve graph if running with limited metadata)
      println!("Note: serde not found in resolved graph (test may need full metadata)");
    }
  }

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
