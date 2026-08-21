//! Unused dependency detection.
//!
//! Combines resolved-graph checks with target-aware rustc diagnostics.

use crate::cargo::manifest_analyzer::{DepKind, DepUsage, ManifestAnalyzer, ParsedManifest};
use crate::cargo::multi_target_metadata::{MultiTargetMetadata, ResolvedDependency};
use crate::cargo::unify_types::{
    DependencyProof, IssueSeverity, MemberEdit, UnifyIssue, UnifyIssueKind, UnusedDep, UnusedReason,
};
use crate::compiler::cfg_eval::{TargetCfgSet, target_constraint_matches_target};
use crate::compiler::scheduler::planned_feature_selections;
use crate::compiler::{
    CompilerCacheIdentity, CompilerCandidate, CompilerDiagnosticsCollector, DependencyEvidenceState,
    DependencyIdentity, FeatureSelection, MemberEvidence,
};
use crate::error::RailResult;
use crate::progress;
use cargo_metadata::PackageId;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

/// Detects unused dependencies in workspace members.
pub struct UnusedDepFinder<'a> {
    workspace_root: &'a Path,
    metadata: &'a MultiTargetMetadata,
    manifests: &'a ManifestAnalyzer,
    target_cfg_sets: &'a HashMap<String, TargetCfgSet>,
    unreachable_features: HashMap<String, BTreeSet<String>>,
    workspace_is_consumer_scope: bool,
    compiler_cache_identity: &'a CompilerCacheIdentity,
}

struct DependencyDeclaration<'a> {
    member: &'a ParsedManifest,
    usages: &'a [DepUsage],
    resolved: Option<ResolvedDependency>,
}

impl<'a> UnusedDepFinder<'a> {
    /// Create a new unused dependency finder.
    pub fn new(
        workspace_root: &'a Path,
        metadata: &'a MultiTargetMetadata,
        manifests: &'a ManifestAnalyzer,
        target_cfg_sets: &'a HashMap<String, TargetCfgSet>,
        pruned_features: &[crate::cargo::unify_types::PrunedFeature],
        workspace_is_consumer_scope: bool,
        compiler_cache_identity: &'a CompilerCacheIdentity,
    ) -> Self {
        let mut unreachable_features: HashMap<String, BTreeSet<String>> = HashMap::new();
        for feature in pruned_features {
            unreachable_features
                .entry(feature.crate_name.to_string())
                .or_default()
                .insert(feature.feature_name.to_string());
        }
        Self {
            workspace_root,
            metadata,
            manifests,
            target_cfg_sets,
            unreachable_features,
            workspace_is_consumer_scope,
            compiler_cache_identity,
        }
    }

    /// Detect unused dependencies in workspace members.
    pub fn find(&self) -> RailResult<Vec<UnusedDep>> {
        let mut unused = Vec::new();
        let declarations = self.dependency_declarations();
        let configured_targets: Vec<&str> = self.metadata.targets();
        let source_unused = self.detect_source_unused_deps(&declarations, &configured_targets, self.target_cfg_sets)?;

        for declaration in declarations {
            if let Some(resolved) = declaration.resolved {
                let required_features = planned_feature_selections(declaration.member);
                for usage in declaration.usages {
                    if self.should_skip_usage(usage, &configured_targets, self.target_cfg_sets)
                        || (usage.kind == DepKind::Build && self.metadata.package_has_links(&resolved.package_ids))
                    {
                        continue;
                    }

                    let required_targets = usage_required_targets(usage, &configured_targets, self.target_cfg_sets);
                    if required_targets.is_empty() {
                        continue;
                    }
                    if resolved
                        .crate_names
                        .iter()
                        .any(|crate_name| declaration.member.retention_identifiers.contains(crate_name))
                    {
                        continue;
                    }
                    let Some(member_evidence) = source_unused.get(&declaration.member.package_id) else {
                        continue;
                    };
                    let dependency = DependencyIdentity {
                        member: declaration.member.package_id.clone(),
                        declaration_key: usage.cargo_toml_key.to_string(),
                        resolved_packages: resolved.package_ids.clone(),
                        crate_names: resolved.crate_names.clone(),
                        kind: usage.kind,
                        target: usage.target.clone(),
                        optional: usage.optional,
                    };
                    if member_evidence.dependency_state_for_features(&dependency, &required_targets, &required_features)
                        != DependencyEvidenceState::Unused
                    {
                        continue;
                    }

                    unused.push(UnusedDep {
                        member: Arc::from(declaration.member.package_name.as_str()),
                        dep_name: Arc::clone(&usage.cargo_toml_key),
                        kind: usage.kind,
                        target: usage.target.as_deref().map(Arc::from),
                        reason: UnusedReason::NotUsedInSource,
                        proof: compiler_proof(&dependency, member_evidence, &required_targets, &required_features),
                    });
                }
                continue;
            }

            for usage in declaration.usages {
                if usage.kind != DepKind::Normal {
                    continue;
                }
                if usage.optional {
                    if !self.optional_dependency_is_removable(declaration.member, usage) {
                        continue;
                    }
                } else if self.should_skip_usage(usage, &configured_targets, self.target_cfg_sets) {
                    continue;
                }

                let applicable_targets = usage_required_targets(usage, &configured_targets, self.target_cfg_sets);
                let optional_proof = if usage.optional {
                    let Some(member_evidence) = source_unused.get(&declaration.member.package_id) else {
                        continue;
                    };
                    let dependency = DependencyIdentity {
                        member: declaration.member.package_id.clone(),
                        declaration_key: usage.cargo_toml_key.to_string(),
                        resolved_packages: BTreeSet::new(),
                        crate_names: BTreeSet::from([usage.cargo_toml_key.replace('-', "_")]),
                        kind: usage.kind,
                        target: usage.target.clone(),
                        optional: true,
                    };
                    let required_features = [FeatureSelection::AllFeatures];
                    if member_evidence.dependency_state_for_features(
                        &dependency,
                        &applicable_targets,
                        &required_features,
                    ) != DependencyEvidenceState::Unused
                    {
                        continue;
                    }
                    Some(compiler_feature_proof(
                        &dependency,
                        member_evidence,
                        &applicable_targets,
                        &required_features,
                    ))
                } else {
                    None
                };
                if let Some(target_cfg) = &usage.target {
                    unused.push(UnusedDep {
                        member: Arc::from(declaration.member.package_name.as_str()),
                        dep_name: Arc::clone(&usage.cargo_toml_key),
                        kind: usage.kind,
                        target: usage.target.as_deref().map(Arc::from),
                        reason: UnusedReason::TargetConfiguredButNotResolved {
                            target_cfg: Arc::from(target_cfg.as_str()),
                        },
                        proof: optional_proof.unwrap_or_else(|| graph_proof(applicable_targets.len())),
                    });
                    continue;
                }

                unused.push(UnusedDep {
                    member: Arc::from(declaration.member.package_name.as_str()),
                    dep_name: Arc::clone(&usage.cargo_toml_key),
                    kind: usage.kind,
                    target: None,
                    reason: UnusedReason::NotInResolvedGraph,
                    proof: optional_proof.unwrap_or_else(|| graph_proof(applicable_targets.len())),
                });
            }
        }

        if !unused.is_empty() {
            progress!("  Found {} potentially unused dependencies", unused.len());
        }

        Ok(unused)
    }

    /// Explain dependency domains intentionally preserved without complete evidence.
    pub fn uncertainty_issues(&self) -> Vec<UnifyIssue> {
        let mut seen = BTreeSet::new();
        let mut issues = Vec::new();
        for member in &self.manifests.members {
            for (dependency, usages) in &member.dependencies {
                let has_links = self
                    .metadata
                    .resolve_dependency(
                        &member.package_id,
                        &dependency.name,
                        dependency.is_renamed().then(|| dependency.alias()),
                    )
                    .is_some_and(|resolved| self.metadata.package_has_links(&resolved.package_ids));
                for usage in usages {
                    let reason = if usage.referenced_in_features {
                        Some("a manifest feature edge references this declaration")
                    } else if usage.optional && self.optional_dependency_is_removable(member, usage) {
                        None
                    } else if usage.optional {
                        Some("optional activation reachability and compiler evidence are incomplete")
                    } else {
                        match usage.kind {
                            DepKind::Normal => None,
                            DepKind::Dev => None,
                            DepKind::Build if has_links => {
                                Some("the dependency declares a native links contract with side effects")
                            }
                            DepKind::Build => None,
                        }
                    };
                    let Some(reason) = reason else {
                        continue;
                    };
                    let key = (
                        member.package_name.as_str(),
                        usage.cargo_toml_key.as_ref(),
                        usage.kind.as_str(),
                        usage.target.as_deref(),
                    );
                    if !seen.insert(key) {
                        continue;
                    }
                    issues.push(UnifyIssue {
                        kind: UnifyIssueKind::General,
                        dep_name: Arc::clone(&usage.cargo_toml_key),
                        severity: IssueSeverity::Warning,
                        message: Arc::from(format!(
                            "preserved {} dependency `{}` in `{}`: {reason}",
                            usage.kind.as_str(),
                            usage.cargo_toml_key,
                            member.package_name
                        )),
                    });
                }
            }
        }
        issues.sort_by(|left, right| {
            left.dep_name
                .cmp(&right.dep_name)
                .then_with(|| left.message.cmp(&right.message))
        });
        issues
    }

    fn dependency_declarations(&self) -> Vec<DependencyDeclaration<'_>> {
        let workspace_member_names: HashSet<&str> = self
            .metadata
            .workspace_packages()
            .map(|package| package.name.as_ref())
            .collect();
        let capacity = self
            .manifests
            .members
            .iter()
            .map(|member| member.dependencies.len())
            .sum();
        let mut declarations = Vec::with_capacity(capacity);

        for member in &self.manifests.members {
            for (key, usages) in &member.dependencies {
                if workspace_member_names.contains(key.name.as_ref()) {
                    continue;
                }
                declarations.push(DependencyDeclaration {
                    member,
                    usages,
                    resolved: self.metadata.resolve_dependency(
                        &member.package_id,
                        &key.name,
                        key.is_renamed().then(|| key.alias()),
                    ),
                });
            }
        }

        declarations
    }

    /// Conservative skip rules to avoid false positives.
    fn should_skip_usage(
        &self,
        usage: &DepUsage,
        configured_targets: &[&str],
        cfg_sets: &HashMap<String, TargetCfgSet>,
    ) -> bool {
        if usage.optional || usage.referenced_in_features {
            return true;
        }

        if let Some(target_cfg) = &usage.target {
            return !target_constraint_matches_any(target_cfg, configured_targets, cfg_sets);
        }

        false
    }

    fn optional_dependency_is_removable(&self, member: &ParsedManifest, usage: &DepUsage) -> bool {
        let private_package = self
            .metadata
            .workspace_packages()
            .find(|package| package.id == member.package_id)
            .and_then(|package| package.publish.as_ref())
            .is_some_and(Vec::is_empty);
        if !private_package || !self.workspace_is_consumer_scope {
            return false;
        }
        if usage.referenced_in_features && !self.only_referenced_by_unreachable_features(member, usage) {
            return false;
        }
        let crate_name = usage.cargo_toml_key.replace('-', "_");
        !member.source_cfg_features.contains(usage.cargo_toml_key.as_ref())
            && !member.retention_identifiers.contains(&crate_name)
    }

    fn only_referenced_by_unreachable_features(&self, member: &ParsedManifest, usage: &DepUsage) -> bool {
        let Some(unreachable) = self.unreachable_features.get(&member.package_name) else {
            return false;
        };
        let Some(package) = self
            .metadata
            .workspace_packages()
            .find(|package| package.id == member.package_id)
        else {
            return false;
        };
        let dependency = usage.cargo_toml_key.as_ref();
        let providers: Vec<_> = package
            .features
            .iter()
            .filter(|(_, edges)| {
                edges
                    .iter()
                    .any(|edge| crate::cargo::feature_scanner::feature_edge_references_dependency(edge, dependency))
            })
            .map(|(feature, _)| feature)
            .collect();
        !providers.is_empty() && providers.iter().all(|feature| unreachable.contains(*feature))
    }

    /// Detect source-unused crates via rustc's `unused_crate_dependencies` lint.
    fn detect_source_unused_deps(
        &self,
        declarations: &[DependencyDeclaration<'_>],
        configured_targets: &[&str],
        cfg_sets: &HashMap<String, TargetCfgSet>,
    ) -> RailResult<HashMap<PackageId, MemberEvidence>> {
        let candidates = self.compiler_candidates(declarations, configured_targets, cfg_sets);
        if candidates.is_empty() {
            return Ok(HashMap::new());
        }

        let targets = self.metadata.targets();
        let collector = CompilerDiagnosticsCollector::with_identity(
            self.workspace_root,
            self.manifests,
            targets,
            self.compiler_cache_identity,
        );
        collector.collect_for_candidates(&candidates)
    }

    fn compiler_candidates(
        &self,
        declarations: &[DependencyDeclaration<'_>],
        configured_targets: &[&str],
        cfg_sets: &HashMap<String, TargetCfgSet>,
    ) -> Vec<CompilerCandidate> {
        let mut candidates = BTreeSet::new();
        for declaration in declarations {
            for usage in declaration.usages {
                if declaration.resolved.is_none() {
                    if usage.kind != DepKind::Normal
                        || !usage.optional
                        || !self.optional_dependency_is_removable(declaration.member, usage)
                    {
                        continue;
                    }
                    let applicable_targets: BTreeSet<String> =
                        usage_required_targets(usage, configured_targets, cfg_sets)
                            .into_iter()
                            .map(str::to_string)
                            .collect();
                    candidates.insert(CompilerCandidate {
                        member: declaration.member.package_name.clone(),
                        crate_name: usage.cargo_toml_key.replace('-', "_"),
                        kind: usage.kind,
                        applicable_targets,
                        required_features: Some(FeatureSelection::AllFeatures),
                    });
                    continue;
                }
                let Some(resolved) = declaration.resolved.as_ref() else {
                    continue;
                };
                if self.should_skip_usage(usage, configured_targets, cfg_sets)
                    || (usage.kind == DepKind::Build && self.metadata.package_has_links(&resolved.package_ids))
                {
                    continue;
                }
                let applicable_targets: BTreeSet<String> = usage_required_targets(usage, configured_targets, cfg_sets)
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                for crate_name in &resolved.crate_names {
                    candidates.insert(CompilerCandidate {
                        member: declaration.member.package_name.clone(),
                        crate_name: crate_name.clone(),
                        kind: usage.kind,
                        applicable_targets: applicable_targets.clone(),
                        required_features: None,
                    });
                }
            }
        }
        candidates.into_iter().collect()
    }

    /// Select only members whose resolved normal dependencies require rustc
    /// source-usage evidence. Graph-absent dependencies are decided without a
    /// compiler pass, while optional and non-normal dependencies remain under
    /// their existing conservative handling.
    /// Generate removal edits for unused dependencies.
    pub fn generate_removal_edits(&self, unused: &[UnusedDep]) -> HashMap<String, Vec<MemberEdit>> {
        let mut edits: HashMap<String, Vec<MemberEdit>> = HashMap::new();

        for dep in unused {
            edits
                .entry(dep.member.to_string())
                .or_default()
                .push(MemberEdit::RemoveDep {
                    dep_name: Arc::clone(&dep.dep_name),
                    dep_kind: dep.kind,
                    target: dep.target.clone(),
                });
        }

        edits
    }
}

fn usage_required_targets<'a>(
    usage: &DepUsage,
    configured_targets: &'a [&'a str],
    cfg_sets: &HashMap<String, TargetCfgSet>,
) -> Vec<&'a str> {
    match &usage.target {
        Some(target_cfg) => configured_targets
            .iter()
            .copied()
            .filter(|target| target_constraint_matches_target(target_cfg, target, cfg_sets.get(*target)))
            .collect(),
        None => configured_targets.to_vec(),
    }
}

fn compiler_proof(
    dependency: &DependencyIdentity,
    evidence: &MemberEvidence,
    required_targets: &[&str],
    required_features: &[FeatureSelection],
) -> DependencyProof {
    let summary = evidence.dependency_summary_for_features(dependency, required_targets, required_features);
    DependencyProof {
        schema_version: 1,
        package_ids: dependency
            .resolved_packages
            .iter()
            .map(|package| Arc::from(package.to_string()))
            .collect(),
        crate_names: dependency
            .crate_names
            .iter()
            .map(|name| Arc::from(name.as_str()))
            .collect(),
        evidence_source: "compiler",
        applicable_configurations: summary.applicable,
        complete_configurations: summary.complete,
        used_observations: summary.used,
        unused_observations: summary.unused,
        incomplete_observations: summary.incomplete,
        cache_hits: evidence.cache.hits,
        cache_misses: evidence.cache.misses,
        cache_miss_reasons: evidence
            .cache
            .miss_reasons
            .iter()
            .map(|(reason, count)| Arc::from(format!("{reason}={count}")))
            .collect(),
        uncertainties: Vec::new(),
    }
}

fn compiler_feature_proof(
    dependency: &DependencyIdentity,
    evidence: &MemberEvidence,
    required_targets: &[&str],
    required_features: &[FeatureSelection],
) -> DependencyProof {
    let summary = evidence.dependency_summary_for_features(dependency, required_targets, required_features);
    DependencyProof {
        schema_version: 1,
        package_ids: Vec::new(),
        crate_names: dependency
            .crate_names
            .iter()
            .map(|name| Arc::from(name.as_str()))
            .collect(),
        evidence_source: "compiler_optional_activation",
        applicable_configurations: summary.applicable,
        complete_configurations: summary.complete,
        used_observations: summary.used,
        unused_observations: summary.unused,
        incomplete_observations: summary.incomplete,
        cache_hits: evidence.cache.hits,
        cache_misses: evidence.cache.misses,
        cache_miss_reasons: evidence
            .cache
            .miss_reasons
            .iter()
            .map(|(reason, count)| Arc::from(format!("{reason}={count}")))
            .collect(),
        uncertainties: Vec::new(),
    }
}

fn graph_proof(applicable_targets: usize) -> DependencyProof {
    DependencyProof {
        schema_version: 1,
        package_ids: Vec::new(),
        crate_names: Vec::new(),
        evidence_source: "resolved_graph",
        applicable_configurations: applicable_targets,
        complete_configurations: applicable_targets,
        used_observations: 0,
        unused_observations: applicable_targets,
        incomplete_observations: 0,
        cache_hits: 0,
        cache_misses: 0,
        cache_miss_reasons: Vec::new(),
        uncertainties: Vec::new(),
    }
}

/// Check if a target constraint (cfg expression) matches any configured target.
fn target_constraint_matches_any(
    cfg: &str,
    configured_targets: &[&str],
    cfg_sets: &HashMap<String, TargetCfgSet>,
) -> bool {
    configured_targets
        .iter()
        .copied()
        .any(|target| target_constraint_matches_target(cfg, target, cfg_sets.get(target)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_constraint_matches_windows() {
        let targets = ["x86_64-pc-windows-msvc"];
        let mut cfg_sets = HashMap::new();
        cfg_sets.insert(
            "x86_64-pc-windows-msvc".to_string(),
            TargetCfgSet::from_test_lines(&["windows", "target_os=\"windows\""]),
        );
        assert!(target_constraint_matches_any("cfg(windows)", &targets, &cfg_sets));
        assert!(!target_constraint_matches_any("cfg(unix)", &targets, &cfg_sets));
    }

    #[test]
    fn test_target_constraint_matches_unix() {
        let targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"];
        let mut cfg_sets = HashMap::new();
        cfg_sets.insert(
            "x86_64-unknown-linux-gnu".to_string(),
            TargetCfgSet::from_test_lines(&["unix", "target_os=\"linux\""]),
        );
        cfg_sets.insert(
            "aarch64-apple-darwin".to_string(),
            TargetCfgSet::from_test_lines(&["unix", "target_os=\"macos\""]),
        );
        assert!(target_constraint_matches_any("cfg(unix)", &targets, &cfg_sets));
        assert!(!target_constraint_matches_any("cfg(windows)", &targets, &cfg_sets));
    }

    #[test]
    fn test_target_constraint_matches_specific_os() {
        let targets = ["x86_64-unknown-linux-gnu"];
        let mut cfg_sets = HashMap::new();
        cfg_sets.insert(
            "x86_64-unknown-linux-gnu".to_string(),
            TargetCfgSet::from_test_lines(&["unix", "target_os=\"linux\""]),
        );
        assert!(target_constraint_matches_any(
            "cfg(target_os = \"linux\")",
            &targets,
            &cfg_sets
        ));
        assert!(!target_constraint_matches_any(
            "cfg(target_os = \"macos\")",
            &targets,
            &cfg_sets
        ));
    }

    #[test]
    fn test_usage_required_targets_filters_by_cfg() {
        let usage = DepUsage {
            unconditional_features: std::collections::BTreeSet::new(),
            local_features: std::collections::BTreeSet::new(),
            conditional_features: std::collections::BTreeSet::new(),
            default_features: true,
            kind: DepKind::Normal,
            target: Some("cfg(windows)".to_string()),
            used_by: Arc::from("crate"),
            optional: false,
            path: None,
            declared_version: Some("1".to_string()),
            manifest_path: None,
            cargo_toml_key: Arc::from("dep"),
            referenced_in_features: false,
            workspace_inherited: false,
        };

        let configured = ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"];
        let mut cfg_sets = HashMap::new();
        cfg_sets.insert(
            "x86_64-unknown-linux-gnu".to_string(),
            TargetCfgSet::from_test_lines(&["unix", "target_os=\"linux\""]),
        );
        cfg_sets.insert(
            "x86_64-pc-windows-msvc".to_string(),
            TargetCfgSet::from_test_lines(&["windows", "target_os=\"windows\""]),
        );
        let required = usage_required_targets(&usage, &configured, &cfg_sets);
        assert_eq!(required, vec!["x86_64-pc-windows-msvc"]);
    }
}
