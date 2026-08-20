//! Feature reachability planning.
//!
//! Removes only private features outside the deterministic root closure and
//! reports inactive forwarding features retained as published API.

use crate::cargo::feature_scanner::FeatureScanner;
use crate::cargo::manifest_analyzer::ManifestAnalyzer;
use crate::cargo::multi_target_metadata::MultiTargetMetadata;
use crate::cargo::unify_types::{MemberEdit, OptionalFeature, PrunedFeature, ReachableFeature};
use crate::compiler::cfg_eval::TargetCfgSet;
use crate::config::{ConsumerScope, UnifyConfig};
use crate::progress;
use rustc_hash::FxHashMap;
use std::collections::HashMap;
use std::sync::Arc;

/// Scans for dead and optional features in workspace crates
pub struct FeaturePruner<'a> {
    metadata: &'a MultiTargetMetadata,
    manifests: &'a ManifestAnalyzer,
    config: &'a UnifyConfig,
    target_cfg_sets: &'a HashMap<String, TargetCfgSet>,
}

impl<'a> FeaturePruner<'a> {
    /// Create a new feature pruner
    pub fn new(
        metadata: &'a MultiTargetMetadata,
        manifests: &'a ManifestAnalyzer,
        config: &'a UnifyConfig,
        target_cfg_sets: &'a HashMap<String, TargetCfgSet>,
    ) -> Self {
        Self {
            metadata,
            manifests,
            config,
            target_cfg_sets,
        }
    }

    /// Detect unreachable private and inactive published features.
    ///
    /// Returns two lists:
    /// - `pruned`: private features outside the root closure
    /// - `optional`: inactive published forwarding features retained as API
    ///
    /// Features matching `preserve_features` patterns in config are excluded from pruning.
    pub fn scan(&self) -> (Vec<PrunedFeature>, Vec<OptionalFeature>, Vec<ReachableFeature>) {
        let mut pruned = Vec::new();
        let mut optional = Vec::new();
        let mut reachable = Vec::new();

        // Analyze workspace using resolved metadata
        let results = FeatureScanner::analyze_workspace(
            self.metadata,
            self.manifests,
            |feature| self.config.should_preserve_feature(feature),
            self.config.consumer_scope == ConsumerScope::Workspace,
            self.target_cfg_sets,
        );

        // Collect dead and optional features
        for result in &results {
            let explicit_features = self
                .manifests
                .members
                .iter()
                .find(|member| member.package_name == result.crate_name)
                .map(|member| &member.declared_features);
            reachable.extend(
                result
                    .reachability_paths
                    .iter()
                    .map(|(feature_name, proof)| ReachableFeature {
                        crate_name: Arc::from(result.crate_name.as_str()),
                        feature_name: Arc::from(feature_name.as_str()),
                        root_kind: proof.root_kind,
                        path: proof.path.iter().map(|feature| Arc::from(feature.as_str())).collect(),
                    }),
            );
            // Truly dead features (empty no-ops), excluding preserved features
            for feature_name in &result.dead_features {
                // Cargo synthesizes a feature for each optional dependency that is not
                // referenced through `dep:`. There is no manifest feature key for the
                // writer to remove; unused-dependency analysis owns that declaration.
                if !explicit_features.is_some_and(|features| features.contains(feature_name)) {
                    continue;
                }
                // Skip features that match preserve_features patterns
                if self.config.should_preserve_feature(feature_name) {
                    continue;
                }
                pruned.push(PrunedFeature {
                    crate_name: Arc::from(result.crate_name.as_str()),
                    feature_name: Arc::from(feature_name.as_str()),
                    declared_edges: self
                        .metadata
                        .workspace_packages()
                        .find(|package| package.name == result.crate_name)
                        .and_then(|package| package.features.get(feature_name))
                        .into_iter()
                        .flatten()
                        .map(|edge| Arc::from(edge.as_str()))
                        .collect(),
                });
            }

            // Optional features (enable something, user-facing API)
            for feature_name in &result.optional_features {
                // Get what this feature enables for reporting
                let enables: Vec<Arc<str>> = self
                    .metadata
                    .workspace_packages()
                    .find(|p| p.name == result.crate_name)
                    .and_then(|p| p.features.get(feature_name))
                    .map(|v| v.iter().map(|s| Arc::from(s.as_str())).collect())
                    .unwrap_or_default();

                optional.push(OptionalFeature {
                    crate_name: Arc::from(result.crate_name.as_str()),
                    feature_name: Arc::from(feature_name.as_str()),
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

        pruned.sort_by(|left, right| {
            left.crate_name
                .cmp(&right.crate_name)
                .then_with(|| left.feature_name.cmp(&right.feature_name))
        });
        optional.sort_by(|left, right| {
            left.crate_name
                .cmp(&right.crate_name)
                .then_with(|| left.feature_name.cmp(&right.feature_name))
        });
        reachable.sort_by(|left, right| {
            left.crate_name
                .cmp(&right.crate_name)
                .then_with(|| left.feature_name.cmp(&right.feature_name))
        });

        (pruned, optional, reachable)
    }

    /// Generate removal edits for pruned features
    pub fn generate_prune_edits(&self, pruned: &[PrunedFeature]) -> FxHashMap<Arc<str>, Vec<MemberEdit>> {
        // Pre-allocate with estimated number of unique crates
        let mut edits: FxHashMap<Arc<str>, Vec<MemberEdit>> = FxHashMap::default();
        edits.reserve(pruned.len().min(32));

        for pf in pruned {
            // Use entry API with clone only when inserting new key
            let entry = edits.entry(Arc::clone(&pf.crate_name));
            entry.or_default().push(MemberEdit::RemoveFeature {
                feature_name: Arc::clone(&pf.feature_name),
            });
        }

        edits
    }
}
