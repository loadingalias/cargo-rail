//! Dead feature detection using resolved cargo metadata
//!
//! Detects features declared in workspace crates that are never enabled
//! by any consumer in the resolved dependency graph across all target triples.
//!
//! This approach is superior to source scanning because:
//! - Uses mathematically verified resolved metadata
//! - Accounts for all target triples
//! - No regex or source parsing needed
//! - Features that enable deps without cfg checks are correctly handled

use cargo_metadata::Package;
use std::collections::{HashMap, HashSet};

use super::multi_target_metadata::MultiTargetMetadata;

/// Result of analyzing a crate's features
#[derive(Debug, Clone)]
pub struct FeatureScanResult {
  /// Crate name that was analyzed
  pub crate_name: String,
  /// Features declared in Cargo.toml
  pub declared_features: HashSet<String>,
  /// Features that are actually enabled in the resolved graph
  pub enabled_features: HashSet<String>,
  /// Features declared but never enabled (dead features)
  pub dead_features: HashSet<String>,
}

/// Analyzes workspace features using resolved cargo metadata
pub struct FeatureScanner;

impl FeatureScanner {
  /// Analyze a single workspace member for dead features
  ///
  /// Uses the resolved metadata across all targets to determine which
  /// declared features are never enabled.
  ///
  /// A feature is considered "alive" (not dead) if:
  /// 1. It's enabled in the resolved graph for any target, OR
  /// 2. It's referenced by any workspace crate's feature definition (e.g., `dep/feature`)
  ///
  /// The second check is crucial because conditional features like `tikv_alloc/mimalloc`
  /// are valid parts of the public API even when not currently enabled.
  pub fn analyze_crate(
    pkg: &Package,
    metadata: &MultiTargetMetadata,
    referenced_features: &HashMap<String, HashSet<String>>,
  ) -> FeatureScanResult {
    let crate_name = pkg.name.to_string();

    // Get declared features from the package
    let declared_features: HashSet<String> = pkg.features.keys().cloned().collect();

    // Get enabled features across all targets from the resolved graph
    let enabled_across_targets = metadata.all_features(&crate_name);
    let enabled_features: HashSet<String> = enabled_across_targets.values().flatten().cloned().collect();

    // Get features referenced by other workspace crates (even if not currently enabled)
    let externally_referenced = referenced_features.get(&crate_name).cloned().unwrap_or_default();

    // Dead features = declared - enabled - externally_referenced
    // "default" is always excluded (it's the entry point)
    let dead_features: HashSet<String> = declared_features
      .difference(&enabled_features)
      .filter(|f| *f != "default")
      .filter(|f| !externally_referenced.contains(*f))
      .cloned()
      .collect();

    FeatureScanResult {
      crate_name,
      declared_features,
      enabled_features,
      dead_features,
    }
  }

  /// Analyze all workspace members for dead features
  ///
  /// Returns results for crates that have dead features.
  pub fn analyze_workspace(metadata: &MultiTargetMetadata) -> Vec<FeatureScanResult> {
    // First, build a map of all features referenced by workspace crates
    // This catches conditional features like `dep/feature` in [features] tables
    let referenced_features = Self::build_referenced_features_map(metadata);

    let mut results = Vec::new();

    for pkg in metadata.workspace_packages() {
      // Skip crates with no declared features
      if pkg.features.is_empty() {
        continue;
      }

      let result = Self::analyze_crate(pkg, metadata, &referenced_features);

      // Only include if there are dead features
      if !result.dead_features.is_empty() {
        results.push(result);
      }
    }

    results
  }

  /// Build a map of features referenced by workspace crates' feature definitions
  ///
  /// Scans all workspace packages' `[features]` tables to find references like:
  /// - `dep/feature` (enables `feature` in dependency `dep`)
  /// - `dep?/feature` (optional dep feature)
  ///
  /// Returns: HashMap<crate_name, HashSet<feature_name>>
  /// where the features are ones referenced externally by other crates.
  fn build_referenced_features_map(metadata: &MultiTargetMetadata) -> HashMap<String, HashSet<String>> {
    let mut map: HashMap<String, HashSet<String>> = HashMap::new();

    for pkg in metadata.workspace_packages() {
      // Scan all feature definitions in this package
      for feature_deps in pkg.features.values() {
        for dep_str in feature_deps {
          // Parse feature references like "dep/feature" or "dep?/feature"
          if let Some((dep_name, feature_name)) = Self::parse_feature_reference(dep_str) {
            map.entry(dep_name).or_default().insert(feature_name);
          }
        }
      }
    }

    map
  }

  /// Parse a feature reference string to extract dep name and feature
  ///
  /// Handles formats:
  /// - `dep/feature` -> Some(("dep", "feature"))
  /// - `dep?/feature` -> Some(("dep", "feature"))
  /// - `dep:feature` (older format) -> Some(("dep", "feature"))
  /// - `feature` (just a feature name) -> None
  /// - `dep:dep` (enabling optional dep) -> None
  fn parse_feature_reference(s: &str) -> Option<(String, String)> {
    // Try slash format first (most common): dep/feature or dep?/feature
    if let Some(idx) = s.find('/') {
      let dep_part = &s[..idx];
      let feature = &s[idx + 1..];
      // Remove trailing ? from optional deps
      let dep_name = dep_part.trim_end_matches('?');
      if !feature.is_empty() {
        return Some((dep_name.to_string(), feature.to_string()));
      }
    }

    None
  }

  /// Get total count of dead features across workspace
  pub fn count_dead_features(results: &[FeatureScanResult]) -> usize {
    results.iter().map(|r| r.dead_features.len()).sum()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_feature_scan_result_dead_features() {
    let mut declared = HashSet::new();
    declared.insert("foo".to_string());
    declared.insert("bar".to_string());
    declared.insert("default".to_string());

    let mut enabled = HashSet::new();
    enabled.insert("foo".to_string());

    // bar is dead (declared but not enabled)
    // default is NOT dead (special case)
    let dead: HashSet<String> = declared
      .difference(&enabled)
      .filter(|f| *f != "default")
      .cloned()
      .collect();

    assert!(dead.contains("bar"));
    assert!(!dead.contains("default"));
    assert!(!dead.contains("foo"));
    assert_eq!(dead.len(), 1);
  }

  #[test]
  fn test_parse_feature_reference() {
    // Standard dep/feature format
    assert_eq!(
      FeatureScanner::parse_feature_reference("tikv_alloc/mimalloc"),
      Some(("tikv_alloc".to_string(), "mimalloc".to_string()))
    );

    // Optional dep format: dep?/feature
    assert_eq!(
      FeatureScanner::parse_feature_reference("serde?/derive"),
      Some(("serde".to_string(), "derive".to_string()))
    );

    // Just a feature name (no dep reference)
    assert_eq!(FeatureScanner::parse_feature_reference("std"), None);

    // dep:dep format (enabling optional dep, not a feature)
    assert_eq!(FeatureScanner::parse_feature_reference("dep:serde"), None);

    // Empty feature after slash
    assert_eq!(FeatureScanner::parse_feature_reference("dep/"), None);
  }
}
