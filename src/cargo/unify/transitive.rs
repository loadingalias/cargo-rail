//! Transitive dependency fragmentation detection and resolution
//!
//! Handles the ONE case where workspace.dependencies can't automatically unify:
//! transitive-only dependencies that resolve with different feature sets across
//! different builds/targets.
//!
//! # The Problem
//!
//! When a crate `C` is:
//! - Only used transitively (never appears in any Cargo.toml manifest)
//! - Resolves with different feature sets in different contexts
//!
//! Cargo's resolver will use different feature sets for different builds, causing
//! fragmentation that workspace.dependencies can't control.
//!
//! # The Solution
//!
//! 1. **Detect**: Find transitive-only crates with multiple resolved feature sets
//! 2. **Warn**: Explain the issue and tradeoffs
//! 3. **Optional Fix**: Allow users to "pin" these crates by:
//!    - Adding them to [workspace.dependencies] with unified features
//!    - Adding explicit dev-dependencies in selected "host" crates
//!    - This brings them under workspace control at the cost of potentially
//!      enabling more features in some contexts

use crate::cargo::WorkspaceMetadata;
use crate::error::RailResult;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

/// A transitive-only crate that has fragmented feature sets
#[derive(Debug, Clone)]
pub struct TransitiveFragmentation {
  /// Crate name
  pub name: String,

  /// Resolved version
  pub version: String,

  /// All distinct feature sets seen across resolves
  pub feature_sets: Vec<HashSet<String>>,

  /// Union of all features
  pub unified_features: Vec<String>,
}

impl TransitiveFragmentation {
  /// Format a user-friendly explanation
  pub fn format_explanation(&self) -> String {
    let mut msg = String::new();
    msg.push_str(&format!("  {} (v{})\n", self.name, self.version));
    msg.push_str("    • Only used transitively (not in any Cargo.toml)\n");
    msg.push_str(&format!(
      "    • Resolves with {} distinct feature sets\n",
      self.feature_sets.len()
    ));
    msg.push_str("    • Cargo's model can't enforce a single feature set without explicit deps\n");
    msg.push_str("    • Options:\n");
    msg.push_str("      - Leave as-is (accept ~5% fragmentation)\n");
    msg.push_str("      - Pin with --pin-transitives (adds explicit workspace control)\n");
    msg
  }
}

/// Detect transitive-only crates with fragmented feature sets
pub fn detect_transitive_fragmentation(metadata: &WorkspaceMetadata) -> RailResult<Vec<TransitiveFragmentation>> {
  // Step 1: Build set of all crates mentioned in any manifest
  let mut manifest_crates: HashSet<String> = HashSet::new();

  for pkg in metadata.list_crates() {
    for dep in &pkg.dependencies {
      manifest_crates.insert(dep.name.clone());
    }
  }

  // Step 2: Build map of resolved features per crate from the dependency graph
  let mut crate_features: HashMap<String, Vec<HashSet<String>>> = HashMap::new();

  if let Some(resolve) = &metadata.metadata_json().resolve {
    for node in &resolve.nodes {
      let pkg_name = node.id.repr.split_whitespace().next().unwrap_or("");

      // Skip workspace members themselves
      if metadata.get_package(pkg_name).is_some() {
        continue;
      }

      // Collect features for this resolve
      let features: HashSet<String> = node.features.iter().map(|f| f.to_string()).collect();

      crate_features.entry(pkg_name.to_string()).or_default().push(features);
    }
  }

  // Step 3: Find transitive-only crates with multiple distinct feature sets IN PARALLEL
  //
  // Convert to Vec for parallel processing
  let crate_features_vec: Vec<_> = crate_features.into_iter().collect();

  let mut fragmentations: Vec<TransitiveFragmentation> = crate_features_vec
    .into_par_iter()
    .filter_map(|(crate_name, feature_sets_list)| {
      // Skip if this crate appears in any manifest (not transitive-only)
      if manifest_crates.contains(&crate_name) {
        return None;
      }

      // Deduplicate feature sets
      let mut unique_feature_sets: Vec<HashSet<String>> = Vec::new();
      for features in feature_sets_list {
        if !unique_feature_sets.contains(&features) {
          unique_feature_sets.push(features);
        }
      }

      // Only report if we have more than one distinct feature set
      if unique_feature_sets.len() <= 1 {
        return None;
      }

      // Compute union of all features
      let mut unified_features: HashSet<String> = HashSet::new();
      for feature_set in &unique_feature_sets {
        unified_features.extend(feature_set.iter().cloned());
      }

      // Get version from metadata
      let version = metadata
        .metadata_json()
        .packages
        .iter()
        .find(|p| p.name == crate_name)
        .map(|p| p.version.to_string())
        .unwrap_or_else(|| "unknown".to_string());

      Some(TransitiveFragmentation {
        name: crate_name,
        version,
        feature_sets: unique_feature_sets,
        unified_features: unified_features.into_iter().collect(),
      })
    })
    .collect();

  // Sort by name for consistent output
  fragmentations.sort_by(|a, b| a.name.cmp(&b.name));

  Ok(fragmentations)
}

// Note: Pinning configuration is in config::UnifyConfig (pin_transitives, pin_hosts fields)

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_transitive_fragmentation_format() {
    let frag = TransitiveFragmentation {
      name: "windows-sys".to_string(),
      version: "0.52.0".to_string(),
      feature_sets: vec![
        vec!["Win32_Foundation".to_string()].into_iter().collect(),
        vec!["Win32_Foundation".to_string(), "Win32_System_Threading".to_string()]
          .into_iter()
          .collect(),
      ],
      unified_features: vec!["Win32_Foundation".to_string(), "Win32_System_Threading".to_string()],
    };

    let explanation = frag.format_explanation();
    assert!(explanation.contains("windows-sys"));
    assert!(explanation.contains("Only used transitively"));
    assert!(explanation.contains("2 distinct feature sets"));
  }
}
