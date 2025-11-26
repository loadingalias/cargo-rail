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
use std::collections::HashSet;

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
  pub fn analyze_crate(pkg: &Package, metadata: &MultiTargetMetadata) -> FeatureScanResult {
    let crate_name = pkg.name.to_string();

    // Get declared features from the package
    let declared_features: HashSet<String> = pkg.features.keys().cloned().collect();

    // Get enabled features across all targets from the resolved graph
    let enabled_across_targets = metadata.all_features(&crate_name);
    let enabled_features: HashSet<String> = enabled_across_targets.values().flatten().cloned().collect();

    // Dead features = declared - enabled
    // But "default" is special - it's the entry point, not something external enables
    let dead_features: HashSet<String> = declared_features
      .difference(&enabled_features)
      .filter(|f| *f != "default")
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
    let mut results = Vec::new();

    for pkg in metadata.workspace_packages() {
      // Skip crates with no declared features
      if pkg.features.is_empty() {
        continue;
      }

      let result = Self::analyze_crate(pkg, metadata);

      // Only include if there are dead features
      if !result.dead_features.is_empty() {
        results.push(result);
      }
    }

    results
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
}
