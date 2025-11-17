//! Feature unification and fragmentation analysis
//!
//! This module provides tools to detect and analyze feature fragmentation in workspaces.
//! Feature fragmentation occurs when the same dependency is built multiple times with
//! different feature sets across workspace members.
//!
//! # The Problem
//!
//! Without feature unification, a workspace might compile `serde` multiple times:
//! - `my-crate-a` uses `serde` with `["derive"]`
//! - `my-crate-b` uses `serde` with `["derive", "rc"]`
//! - `my-crate-c` uses `serde` with `["derive", "rc", "alloc"]`
//!
//! This causes 3 separate compilations of `serde`, each with a different feature set.
//!
//! # The Solution
//!
//! FeatureAnalyzer detects this fragmentation and suggests unifying features either:
//! 1. In `[workspace.dependencies]` with the union of all features
//! 2. Via a carefully managed unified feature set
//!
//! This eliminates cargo-hakari's need for a fake `workspace-hack` crate.

use super::WorkspaceMetadata;
use crate::error::{RailError, RailResult};
use std::collections::{HashMap, HashSet};

/// Analyzer for feature fragmentation and unification
pub struct FeatureAnalyzer<'a> {
  metadata: &'a WorkspaceMetadata,
}

/// A dependency that is built multiple times with different feature sets
#[derive(Debug, Clone)]
pub struct FragmentedDependency {
  /// Dependency name (e.g., "serde")
  pub name: String,

  /// Different feature sets this dependency is built with
  /// Each HashSet represents one unique compilation
  pub feature_sets: Vec<HashSet<String>>,

  /// Which workspace packages use this dependency
  pub used_by: Vec<String>,

  /// Unified feature set (union of all feature_sets)
  pub unified_features: HashSet<String>,

  /// How many times this dependency is compiled
  pub compilation_count: usize,
}

/// Complete fragmentation report for a workspace
#[derive(Debug)]
pub struct FragmentationReport {
  /// Dependencies that are fragmented (compiled multiple times)
  pub fragmented: Vec<FragmentedDependency>,

  /// Total number of redundant compilations that could be eliminated
  pub redundant_compilations: usize,

  /// Dependencies that are NOT fragmented (compiled exactly once)
  pub unified: Vec<String>,
}

impl<'a> FeatureAnalyzer<'a> {
  /// Create a new feature analyzer
  pub fn new(metadata: &'a WorkspaceMetadata) -> Self {
    Self { metadata }
  }

  /// Analyze feature fragmentation across the workspace
  ///
  /// This examines the resolved dependency graph to find dependencies
  /// that are built multiple times with different feature sets.
  pub fn analyze(&self) -> RailResult<FragmentationReport> {
    let resolve = self
      .metadata
      .resolve()
      .ok_or_else(|| RailError::message("No dependency resolution available in metadata"))?;

    // Group nodes by dependency name
    // Map: dep_name -> [(package_id, feature_set)]
    let mut dependency_features: HashMap<String, Vec<(String, HashSet<String>)>> = HashMap::new();

    for node in &resolve.nodes {
      // Get package info
      if let Some(pkg) = self.metadata.find_package_by_id(&node.id) {
        let pkg_name = pkg.name.to_string();
        let features: HashSet<String> = node.features.iter().map(|f| f.to_string()).collect();

        dependency_features
          .entry(pkg_name)
          .or_default()
          .push((node.id.repr.clone(), features));
      }
    }

    // Find fragmented dependencies (those with multiple different feature sets)
    let mut fragmented = Vec::new();
    let mut unified = Vec::new();
    let mut redundant_compilations = 0;

    for (dep_name, instances) in dependency_features {
      // Skip workspace members - we only care about external dependencies
      if self.metadata.get_package(&dep_name).is_some() {
        continue;
      }

      // Collect unique feature sets
      let mut unique_feature_sets: Vec<HashSet<String>> = Vec::new();
      for (_pkg_id, features) in &instances {
        if !unique_feature_sets.iter().any(|fs| fs == features) {
          unique_feature_sets.push(features.clone());
        }
      }

      if unique_feature_sets.len() > 1 {
        // Fragmented - compiled multiple times
        let compilation_count = unique_feature_sets.len();
        redundant_compilations += compilation_count - 1; // -1 because one compilation is needed

        // Compute unified feature set (union of all)
        let unified_features: HashSet<String> = unique_feature_sets.iter().flat_map(|fs| fs.iter().cloned()).collect();

        // Find which workspace packages use this dependency
        let used_by = self.find_workspace_users(&dep_name);

        fragmented.push(FragmentedDependency {
          name: dep_name,
          feature_sets: unique_feature_sets,
          used_by,
          unified_features,
          compilation_count,
        });
      } else {
        // Unified - compiled exactly once
        unified.push(dep_name);
      }
    }

    // Sort by compilation count (most fragmented first)
    fragmented.sort_by(|a, b| b.compilation_count.cmp(&a.compilation_count));

    Ok(FragmentationReport {
      fragmented,
      redundant_compilations,
      unified,
    })
  }

  /// Find which workspace members use a specific dependency
  fn find_workspace_users(&self, dep_name: &str) -> Vec<String> {
    let mut users = Vec::new();

    for pkg in self.metadata.list_crates() {
      if pkg.dependencies.iter().any(|dep| dep.name == dep_name) {
        users.push(pkg.name.to_string());
      }
    }

    users.sort();
    users
  }

  /// Suggest optimal feature configurations for workspace.dependencies
  ///
  /// Returns suggestions for [workspace.dependencies] entries that would
  /// eliminate fragmentation
  pub fn suggest_unification(&self, report: &FragmentationReport) -> Vec<UnificationSuggestion> {
    let mut suggestions = Vec::new();

    for frag in &report.fragmented {
      let suggestion = UnificationSuggestion {
        dependency: frag.name.clone(),
        current_compilations: frag.compilation_count,
        suggested_features: frag.unified_features.iter().cloned().collect(),
        affected_packages: frag.used_by.clone(),
      };
      suggestions.push(suggestion);
    }

    suggestions
  }
}

/// A suggestion for unifying a fragmented dependency
#[derive(Debug, Clone)]
pub struct UnificationSuggestion {
  /// Dependency name
  pub dependency: String,

  /// Current number of compilations
  pub current_compilations: usize,

  /// Suggested unified feature set
  pub suggested_features: Vec<String>,

  /// Workspace packages that would be affected
  pub affected_packages: Vec<String>,
}

impl UnificationSuggestion {
  /// Format as a Cargo.toml entry for [workspace.dependencies]
  pub fn to_toml_entry(&self) -> String {
    let mut features = self.suggested_features.clone();
    features.sort();

    if features.is_empty() {
      // No features, just version
      format!(
        "{} = \"*\"  # Unifies {} compilations",
        self.dependency, self.current_compilations
      )
    } else {
      // With features
      format!(
        "{} = {{ version = \"*\", features = [{}] }}  # Unifies {} compilations",
        self.dependency,
        features
          .iter()
          .map(|f| format!("\"{}\"", f))
          .collect::<Vec<_>>()
          .join(", "),
        self.current_compilations
      )
    }
  }

  /// Format as a human-readable explanation
  pub fn to_explanation(&self) -> String {
    format!(
      "{} is currently compiled {} times. Add this to [workspace.dependencies]:\n  {}\n  Affects: {}",
      self.dependency,
      self.current_compilations,
      self.to_toml_entry(),
      self.affected_packages.join(", ")
    )
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
  fn test_feature_analyzer_creation() {
    let metadata = create_test_metadata();
    let analyzer = FeatureAnalyzer::new(&metadata);

    // Should be able to create analyzer
    assert!(std::ptr::addr_of!(analyzer) as usize != 0);
  }

  #[test]
  fn test_analyze_returns_report() {
    let metadata = create_test_metadata();
    let analyzer = FeatureAnalyzer::new(&metadata);

    // Should successfully analyze and return a report
    let report = analyzer.analyze();
    assert!(report.is_ok(), "Analysis should succeed");

    let report = report.unwrap();
    // Report should have fragmented and unified lists (may be empty)
    // Just verify we got a valid report structure
    let _ = &report.fragmented;
    let _ = &report.unified;
  }

  #[test]
  fn test_fragmentation_report_structure() {
    let metadata = create_test_metadata();
    let analyzer = FeatureAnalyzer::new(&metadata);
    let report = analyzer.analyze().unwrap();

    // Verify report structure
    for frag in &report.fragmented {
      // Each fragmented dependency should have valid data
      assert!(!frag.name.is_empty(), "Dependency name should not be empty");
      assert!(frag.compilation_count > 1, "Fragmented means compiled > 1 time");
      assert_eq!(
        frag.feature_sets.len(),
        frag.compilation_count,
        "Feature sets should match compilation count"
      );

      // Unified features should be union of all feature sets
      let mut expected_union = HashSet::new();
      for fs in &frag.feature_sets {
        expected_union.extend(fs.iter().cloned());
      }
      assert_eq!(
        frag.unified_features, expected_union,
        "Unified features should be union of all sets"
      );
    }

    // Redundant compilations should be calculated correctly
    let expected_redundant: usize = report.fragmented.iter().map(|f| f.compilation_count - 1).sum();
    assert_eq!(
      report.redundant_compilations, expected_redundant,
      "Redundant compilations miscalculated"
    );
  }

  #[test]
  fn test_suggest_unification() {
    let metadata = create_test_metadata();
    let analyzer = FeatureAnalyzer::new(&metadata);
    let report = analyzer.analyze().unwrap();

    let suggestions = analyzer.suggest_unification(&report);

    // Should have same number of suggestions as fragmented dependencies
    assert_eq!(suggestions.len(), report.fragmented.len());

    // Each suggestion should match its fragmented dependency
    for (suggestion, frag) in suggestions.iter().zip(report.fragmented.iter()) {
      assert_eq!(suggestion.dependency, frag.name);
      assert_eq!(suggestion.current_compilations, frag.compilation_count);
      assert_eq!(suggestion.affected_packages, frag.used_by);

      // Suggested features should match unified features
      let suggested_set: HashSet<_> = suggestion.suggested_features.iter().cloned().collect();
      assert_eq!(suggested_set, frag.unified_features);
    }
  }

  #[test]
  fn test_find_workspace_users() {
    let metadata = create_test_metadata();
    let analyzer = FeatureAnalyzer::new(&metadata);

    // Find users of a common dependency (e.g., serde, clap)
    // cargo-rail should be in the list
    let users = analyzer.find_workspace_users("serde");

    // Should return sorted list
    let mut sorted_users = users.clone();
    sorted_users.sort();
    assert_eq!(users, sorted_users, "Users should be sorted");

    // Should not have duplicates
    let unique: HashSet<_> = users.iter().collect();
    assert_eq!(users.len(), unique.len(), "Should not have duplicate users");
  }

  #[test]
  fn test_resolved_features_map() {
    let metadata = create_test_metadata();
    let resolved = metadata.resolved_features();

    // Should return a map of package_id -> features
    assert!(!resolved.is_empty(), "Should have resolved features");

    // Each entry should have valid package ID and feature set
    for pkg_id in resolved.keys() {
      assert!(!pkg_id.is_empty(), "Package ID should not be empty");
      // Features can be empty (no features enabled) - just verify structure exists
    }
  }

  #[test]
  fn test_fragmented_dependency_excludes_workspace_members() {
    let metadata = create_test_metadata();
    let analyzer = FeatureAnalyzer::new(&metadata);
    let report = analyzer.analyze().unwrap();

    // Workspace members should NOT appear in fragmented list
    let workspace_names: HashSet<_> = metadata.list_crates().iter().map(|p| p.name.as_str()).collect();

    for frag in &report.fragmented {
      assert!(
        !workspace_names.contains(frag.name.as_str()),
        "Workspace member {} should not be in fragmented list",
        frag.name
      );
    }

    for unified_name in &report.unified {
      assert!(
        !workspace_names.contains(unified_name.as_str()),
        "Workspace member {} should not be in unified list",
        unified_name
      );
    }
  }

  #[test]
  fn test_unification_suggestion_formatting() {
    let suggestion = UnificationSuggestion {
      dependency: "serde".to_string(),
      current_compilations: 3,
      suggested_features: vec!["derive".to_string(), "rc".to_string()],
      affected_packages: vec!["crate-a".to_string(), "crate-b".to_string()],
    };

    let toml = suggestion.to_toml_entry();
    assert!(toml.contains("serde"));
    assert!(toml.contains("derive"));
    assert!(toml.contains("rc"));
    assert!(toml.contains("3 compilations"));
  }

  #[test]
  fn test_unification_suggestion_no_features() {
    let suggestion = UnificationSuggestion {
      dependency: "anyhow".to_string(),
      current_compilations: 2,
      suggested_features: vec![],
      affected_packages: vec!["crate-x".to_string()],
    };

    let toml = suggestion.to_toml_entry();
    assert!(toml.contains("anyhow"));
    assert!(!toml.contains("features"));
  }

  #[test]
  fn test_unification_suggestion_explanation() {
    let suggestion = UnificationSuggestion {
      dependency: "tokio".to_string(),
      current_compilations: 4,
      suggested_features: vec!["fs".to_string(), "net".to_string()],
      affected_packages: vec!["app".to_string(), "lib".to_string()],
    };

    let explanation = suggestion.to_explanation();
    assert!(explanation.contains("tokio"));
    assert!(explanation.contains("4 times"));
    assert!(explanation.contains("app"));
    assert!(explanation.contains("lib"));
  }
}
