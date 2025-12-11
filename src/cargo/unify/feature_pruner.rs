//! Dead and optional feature scanning
//!
//! Analyzes workspace features using resolved cargo metadata to find:
//! - Dead features (empty no-ops) that can be safely removed
//! - Optional features (user-facing API, not removed)

use crate::cargo::feature_scanner::FeatureScanner;
use crate::cargo::multi_target_metadata::MultiTargetMetadata;
use crate::cargo::unify_types::{MemberEdit, OptionalFeature, PrunedFeature};
use crate::config::UnifyConfig;
use crate::progress;
use std::collections::HashMap;

/// Scans for dead and optional features in workspace crates
pub struct FeaturePruner<'a> {
  metadata: &'a MultiTargetMetadata,
  config: &'a UnifyConfig,
}

impl<'a> FeaturePruner<'a> {
  /// Create a new feature pruner
  pub fn new(metadata: &'a MultiTargetMetadata, config: &'a UnifyConfig) -> Self {
    Self { metadata, config }
  }

  /// Detect dead and optional features using resolved cargo metadata
  ///
  /// Returns two lists:
  /// - `pruned`: Truly dead features (empty no-ops) that can be safely removed
  /// - `optional`: Features not enabled but enable something (user-facing API, don't remove)
  ///
  /// Features matching `preserve_features` patterns in config are excluded from pruning.
  pub fn scan(&self) -> (Vec<PrunedFeature>, Vec<OptionalFeature>) {
    let mut pruned = Vec::new();
    let mut optional = Vec::new();

    // Analyze workspace using resolved metadata
    let results = FeatureScanner::analyze_workspace(self.metadata);

    // Collect dead and optional features
    for result in &results {
      // Truly dead features (empty no-ops), excluding preserved features
      for feature_name in &result.dead_features {
        // Skip features that match preserve_features patterns
        if self.config.should_preserve_feature(feature_name) {
          continue;
        }
        pruned.push(PrunedFeature {
          crate_name: result.crate_name.clone(),
          feature_name: feature_name.clone(),
        });
      }

      // Optional features (enable something, user-facing API)
      for feature_name in &result.optional_features {
        // Get what this feature enables for reporting
        let enables = self
          .metadata
          .workspace_packages()
          .iter()
          .find(|p| p.name == result.crate_name)
          .and_then(|p| p.features.get(feature_name))
          .cloned()
          .unwrap_or_default();

        optional.push(OptionalFeature {
          crate_name: result.crate_name.clone(),
          feature_name: feature_name.clone(),
          enables,
        });
      }
    }

    if !pruned.is_empty() || !optional.is_empty() {
      let crate_count = results.len();
      if !pruned.is_empty() {
        progress!(
          "  Found {} dead features (empty no-ops) across {} crates",
          pruned.len(),
          crate_count
        );
      }
      if !optional.is_empty() {
        progress!(
          "  Found {} optional features (user-facing, not removed)",
          optional.len()
        );
      }
    }

    (pruned, optional)
  }

  /// Generate removal edits for pruned features
  pub fn generate_prune_edits(&self, pruned: &[PrunedFeature]) -> HashMap<String, Vec<MemberEdit>> {
    let mut edits: HashMap<String, Vec<MemberEdit>> = HashMap::new();

    for pf in pruned {
      edits
        .entry(pf.crate_name.clone())
        .or_default()
        .push(MemberEdit::RemoveFeature {
          feature_name: pf.feature_name.clone(),
        });
    }

    edits
  }
}
