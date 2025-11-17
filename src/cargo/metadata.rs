use crate::error::RailResult;
use cargo_metadata::{Dependency, DependencyKind, MetadataCommand, Package, Resolve, Target};
use semver::Version;
use std::collections::{BTreeMap, HashMap, HashSet};
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
  /// Useful for simulating different feature combinations to detect fragmentation
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
  pub fn package_features(&self, name: &str) -> Option<&BTreeMap<String, Vec<String>>> {
    self.get_package(name).map(|pkg| &pkg.features)
  }

  /// Get all targets (lib, bin, test, etc.) for a package
  pub fn package_targets(&self, name: &str) -> Vec<&Target> {
    self
      .get_package(name)
      .map(|pkg| pkg.targets.iter().collect())
      .unwrap_or_default()
  }

  /// Get package edition (2015, 2018, 2021, 2024)
  pub fn package_edition(&self, name: &str) -> Option<&str> {
    self.get_package(name).map(|pkg| pkg.edition.as_str())
  }

  /// Get package MSRV (minimum supported Rust version)
  pub fn package_rust_version(&self, name: &str) -> Option<&Version> {
    self.get_package(name).and_then(|pkg| pkg.rust_version.as_ref())
  }

  // ============================================================================
  // Tier 3: Advanced Dependency Analysis
  // ============================================================================

  /// Get all dependencies for a package (normal + dev + build)
  pub fn package_dependencies(&self, name: &str) -> Vec<&Dependency> {
    self
      .get_package(name)
      .map(|pkg| pkg.dependencies.iter().collect())
      .unwrap_or_default()
  }

  /// Get dependencies of a specific kind (normal, dev, or build)
  pub fn package_dependencies_by_kind(&self, name: &str, kind: DependencyKind) -> Vec<&Dependency> {
    self
      .package_dependencies(name)
      .into_iter()
      .filter(|dep| dep.kind == kind)
      .collect()
  }

  /// Get all optional dependencies for a package
  pub fn package_optional_dependencies(&self, name: &str) -> Vec<&Dependency> {
    self
      .package_dependencies(name)
      .into_iter()
      .filter(|dep| dep.optional)
      .collect()
  }

  /// Check if a dependency uses default features
  pub fn dependency_uses_default_features(&self, package: &str, dep_name: &str) -> Option<bool> {
    self
      .package_dependencies(package)
      .into_iter()
      .find(|dep| dep.name == dep_name)
      .map(|dep| dep.uses_default_features)
  }

  /// Get platform-specific dependencies (e.g., only on Windows, Unix, etc.)
  pub fn package_platform_specific_dependencies(&self, name: &str) -> Vec<(&Dependency, String)> {
    self
      .package_dependencies(name)
      .into_iter()
      .filter_map(|dep| dep.target.as_ref().map(|target| (dep, target.to_string())))
      .collect()
  }

  /// Get all packages (workspace members + dependencies)
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

  /// Analyze which features are actually enabled in the resolved graph
  ///
  /// Returns map of package_id -> enabled_features
  pub fn resolved_features(&self) -> HashMap<String, HashSet<String>> {
    let mut result = HashMap::new();

    if let Some(resolve) = self.resolve() {
      for node in &resolve.nodes {
        let pkg_id = node.id.repr.clone();
        let features: HashSet<String> = node.features.iter().map(|f| f.to_string()).collect();
        result.insert(pkg_id, features);
      }
    }

    result
  }

  /// Find all packages that depend on a specific package (reverse dependencies)
  ///
  /// Uses the resolved graph, not just declared dependencies
  pub fn reverse_dependencies(&self, target_package: &str) -> Vec<String> {
    let mut result = Vec::new();

    if let Some(resolve) = self.resolve() {
      // Find target package ID
      let target_id = self
        .metadata
        .packages
        .iter()
        .find(|pkg| pkg.name == target_package)
        .map(|pkg| &pkg.id);

      if let Some(target_id) = target_id {
        // Find all nodes that depend on target
        for node in &resolve.nodes {
          if node.deps.iter().any(|dep| &dep.pkg == target_id)
            && let Some(pkg) = self.find_package_by_id(&node.id)
          {
            result.push(pkg.name.to_string());
          }
        }
      }
    }

    result.sort();
    result.dedup();
    result
  }

  // ============================================================================
  // Raw Access
  // ============================================================================

  /// Access raw cargo_metadata::Metadata for advanced use cases
  pub fn metadata_json(&self) -> &cargo_metadata::Metadata {
    &self.metadata
  }

  /// Get raw JSON string for external tools
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
  fn test_resolved_features() {
    let metadata = create_test_metadata();
    let resolved = metadata.resolved_features();

    assert!(!resolved.is_empty(), "Should have resolved features");

    // Each package should have feature set (may be empty)
    for pkg_id in resolved.keys() {
      assert!(!pkg_id.is_empty(), "Package ID should not be empty");
      // Features can be empty (no features enabled) - just verify structure exists
    }
  }

  #[test]
  fn test_reverse_dependencies() {
    let metadata = create_test_metadata();

    // Find reverse dependencies of serde (many crates use it)
    let reverse_deps = metadata.reverse_dependencies("serde");

    // Should be sorted and deduplicated
    let mut sorted = reverse_deps.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
      reverse_deps, sorted,
      "Reverse dependencies should be sorted and deduplicated"
    );
  }

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
}
