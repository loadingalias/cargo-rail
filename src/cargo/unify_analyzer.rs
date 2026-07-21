//! Clean unification analyzer using the HYBRID approach
//!
//! This is the core of the unify implementation that properly integrates
//! with WorkspaceContext and uses multi-target metadata + manifest analysis.
//! The undeclared features detection lives here.

use crate::cargo::{
  DepKind,
  manifest_analyzer::{ExistingWorkspaceDep, ManifestAnalyzer, parse_existing_workspace_deps},
  multi_target_metadata::MultiTargetMetadata,
  unify::version_utils::{find_major_version_conflicts, is_exact_pin, versions_compatible},
  unify::{CandidateIterator, FeaturePruner, TransitivePlanner, UnusedDepFinder},
  unify_types::{
    DuplicateCleanup, FeatureEnablingPath, IssueSeverity, MemberEdit, PrunedFeature, UndeclaredFeature,
    UnificationPlan, UnifiedDep, UnifyDecision, UnifyDecisionCode, UnifyDecisionReason, UnifyDecisionSubject,
    UnifyIssue, UnifyIssueKind, UnusedDep, ValidationResult, VersionMismatch,
  },
};
use crate::compiler::CompilerCacheIdentity;
use crate::compiler::cfg_eval::{TargetCfgSet, target_constraint_matches_target};
use crate::config::{ConsumerScope, ExactPinHandling, MajorVersionConflict, UnifyConfig};
use crate::error::{RailResult, ResultExt};
use crate::progress;
use crate::workspace::WorkspaceContext;
use rustc_hash::{FxHashMap, FxHashSet};
use semver::Version;
use semver::VersionReq;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Analyzes workspace for dependency unifications
pub struct UnifyAnalyzer {
  /// Multi-target metadata (Arc for cheap sharing - avoids expensive clone)
  metadata: Arc<MultiTargetMetadata>,
  manifests: ManifestAnalyzer,
  /// Configuration for unification behavior (can be overridden at runtime)
  pub config: UnifyConfig,
  /// Existing workspace dependencies (already in [workspace.dependencies])
  existing_workspace_deps: FxHashMap<String, ExistingWorkspaceDep>,
  /// Precomputed cohort diagnostics from config + workspace-member graph policy
  cohort_issues: Vec<UnifyIssue>,
  /// Explainability records for cohort policy decisions keyed by member package name.
  cohort_decisions: FxHashMap<Arc<str>, Vec<CohortDecision>>,
  /// Workspace root path (for path normalization)
  workspace_root: PathBuf,
  /// Canonical workspace root path (computed once)
  canonical_workspace_root: PathBuf,
  /// Exact rustc cfg sets shared across target-aware unify analyses.
  target_cfg_sets: Arc<std::collections::HashMap<String, TargetCfgSet>>,
  compiler_cache_identity: Option<CompilerCacheIdentity>,
}

type FeatureDomain = (String, String, DepKind);
type FeatureSources = FxHashMap<FeatureDomain, FxHashMap<String, Vec<FeatureProvider>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct FeatureProvider {
  member: String,
  alias: Arc<str>,
  kind: DepKind,
  target: Option<String>,
  default_features: bool,
  optional: bool,
}

#[derive(Debug, Clone)]
struct CohortDecision {
  summary: Arc<str>,
  members: Vec<Arc<str>>,
}

impl UnifyAnalyzer {
  /// Create a new analyzer from workspace context
  pub fn new(ctx: &WorkspaceContext) -> RailResult<Self> {
    // Get multi-target metadata (lazily loaded on first access)
    // Uses Arc::clone for cheap reference counting instead of cloning the entire cache
    let metadata = ctx.multi_target_metadata()?;
    let target_cfg_sets = ctx.target_cfg_sets()?;

    // Parse all manifests once
    let workspace_packages = ctx.cargo().workspace_members();
    let manifests = ManifestAnalyzer::parse_workspace(ctx.workspace_root(), &workspace_packages)?;

    // Parse existing workspace.dependencies to avoid duplicates
    let existing_workspace_deps = parse_existing_workspace_deps(ctx.workspace_root())?;

    // Build config from context, then enforce workspace-member cohort semantics.
    let base_config = ctx.config().map(|c| c.unify.clone()).unwrap_or_default();
    let workspace_member_names: FxHashSet<Arc<str>> = workspace_packages
      .iter()
      .map(|pkg| Arc::from(pkg.name.as_str()))
      .collect();
    let (config, cohort_issues, cohort_decisions) =
      Self::apply_workspace_member_cohort_policy(base_config, &manifests, &workspace_member_names);
    let workspace_root = ctx.workspace_root().to_path_buf();
    let canonical_workspace_root = workspace_root.canonicalize().unwrap_or_else(|_| workspace_root.clone());
    let compiler_cache_identity = match CompilerCacheIdentity::capture(ctx.snapshot()?) {
      Ok(identity) => Some(identity),
      Err(error) => {
        crate::warn!(
          "compiler evidence identity capture failed; falling back to graph-only detection: {}",
          error
        );
        None
      }
    };

    Ok(Self {
      metadata,
      manifests,
      config,
      existing_workspace_deps,
      cohort_issues,
      cohort_decisions,
      workspace_root,
      canonical_workspace_root,
      target_cfg_sets,
      compiler_cache_identity,
    })
  }

  fn feature_enabling_paths(usages: &[&crate::cargo::manifest_analyzer::DepUsage]) -> Vec<FeatureEnablingPath> {
    let mut paths = usages
      .iter()
      .map(|usage| {
        let features = usage
          .unconditional_features
          .union(&usage.conditional_features)
          .map(|feature| Arc::from(feature.as_str()))
          .collect();
        FeatureEnablingPath {
          member: Arc::clone(&usage.used_by),
          alias: Arc::clone(&usage.cargo_toml_key),
          dependency_kind: usage.kind,
          target: usage.target.as_deref().map(Arc::from),
          features,
          default_features: usage.default_features,
          optional: usage.optional,
        }
      })
      .collect::<Vec<_>>();
    paths.sort_by(|left, right| {
      left
        .member
        .cmp(&right.member)
        .then_with(|| left.alias.cmp(&right.alias))
        .then_with(|| left.dependency_kind.cmp(&right.dependency_kind))
        .then_with(|| left.target.cmp(&right.target))
        .then_with(|| left.features.cmp(&right.features))
    });
    paths.dedup();
    paths
  }

  /// Build connected cohorts of workspace members that depend on each other.
  fn build_workspace_member_cohorts(
    manifests: &ManifestAnalyzer,
    workspace_member_names: &FxHashSet<Arc<str>>,
  ) -> Vec<Vec<Arc<str>>> {
    let mut adjacency: FxHashMap<Arc<str>, FxHashSet<Arc<str>>> = FxHashMap::default();
    for member in workspace_member_names {
      adjacency.entry(Arc::clone(member)).or_default();
    }

    for member in &manifests.members {
      let member_name: Arc<str> = Arc::from(member.package_name.as_str());
      if !workspace_member_names.contains(&member_name) {
        continue;
      }

      for dep_key in member.dependencies.keys() {
        if !workspace_member_names.contains(&dep_key.name) {
          continue;
        }

        adjacency
          .entry(Arc::clone(&member_name))
          .or_default()
          .insert(Arc::clone(&dep_key.name));
        adjacency
          .entry(Arc::clone(&dep_key.name))
          .or_default()
          .insert(Arc::clone(&member_name));
      }
    }

    let mut visited: FxHashSet<Arc<str>> = FxHashSet::default();
    let mut cohorts = Vec::new();
    let mut nodes: Vec<_> = adjacency.keys().cloned().collect();
    nodes.sort();

    for node in nodes {
      if visited.contains(&node) {
        continue;
      }

      let mut stack = vec![Arc::clone(&node)];
      let mut component = Vec::new();

      while let Some(current) = stack.pop() {
        if !visited.insert(Arc::clone(&current)) {
          continue;
        }
        component.push(Arc::clone(&current));
        if let Some(neighbors) = adjacency.get(&current) {
          for neighbor in neighbors {
            if !visited.contains(neighbor) {
              stack.push(Arc::clone(neighbor));
            }
          }
        }
      }

      component.sort();
      cohorts.push(component);
    }

    cohorts
  }

  /// Enforce atomic unification policy for workspace-member cohorts.
  ///
  /// This prevents local-vs-registry splits when only part of an interconnected
  /// workspace-member dependency set would otherwise be unified.
  fn apply_workspace_member_cohort_policy(
    mut config: UnifyConfig,
    manifests: &ManifestAnalyzer,
    workspace_member_names: &FxHashSet<Arc<str>>,
  ) -> (UnifyConfig, Vec<UnifyIssue>, FxHashMap<Arc<str>, Vec<CohortDecision>>) {
    let mut issues = Vec::new();
    let mut cohort_decisions: FxHashMap<Arc<str>, Vec<CohortDecision>> = FxHashMap::default();
    let cohorts = Self::build_workspace_member_cohorts(manifests, workspace_member_names);

    for cohort in cohorts.into_iter().filter(|c| c.len() > 1) {
      let excluded_members: Vec<&Arc<str>> = cohort.iter().filter(|name| config.should_exclude(name)).collect();

      if !excluded_members.is_empty() {
        if excluded_members.len() != cohort.len() {
          let mut cohort_names: Vec<&str> = cohort.iter().map(|s| &**s).collect();
          let mut excluded_names: Vec<&str> = excluded_members.iter().map(|s| &***s).collect();
          cohort_names.sort_unstable();
          excluded_names.sort_unstable();
          let summary = Arc::from(format!(
            "Enforced atomic workspace-member cohort [{}] after partial exclusion [{}] to avoid local-vs-registry splits.",
            cohort_names.join(", "),
            excluded_names.join(", ")
          ));
          issues.push(UnifyIssue {
            kind: UnifyIssueKind::WorkspaceMemberCohortSplitRisk,
            dep_name: Arc::clone(excluded_members[0]),
            severity: IssueSeverity::Warning,
            message: Arc::from(format!(
              "Workspace-member cohort [{}] was partially excluded [{}]. \
               Applying exclude atomically to the full cohort to prevent local-vs-registry source splits.",
              cohort_names.join(", "),
              excluded_names.join(", ")
            )),
          });
          for member in &cohort {
            cohort_decisions
              .entry(Arc::clone(member))
              .or_default()
              .push(CohortDecision {
                summary: Arc::clone(&summary),
                members: cohort.to_vec(),
              });
          }
        }

        for member in &cohort {
          if !config.should_exclude(member) {
            config.exclude.push(member.to_string());
          }
        }
        continue;
      }

      let mut low_usage_members = Vec::new();
      let mut regular_members = Vec::new();
      for member in &cohort {
        if config.should_include(member) {
          regular_members.push(member.to_string());
          continue;
        }

        if manifests.package_usage_count(member) < 2 {
          low_usage_members.push(member.to_string());
        } else {
          regular_members.push(member.to_string());
        }
      }

      if !low_usage_members.is_empty() && !regular_members.is_empty() {
        low_usage_members.sort();
        regular_members.sort();
        let mut cohort_names: Vec<&str> = cohort.iter().map(|s| &**s).collect();
        cohort_names.sort_unstable();
        let summary = Arc::from(format!(
          "Enforced atomic workspace-member cohort [{}] because single-user filtering would split [{}].",
          cohort_names.join(", "),
          low_usage_members.join(", ")
        ));

        issues.push(UnifyIssue {
          kind: UnifyIssueKind::WorkspaceMemberCohortSplitRisk,
          dep_name: Arc::from(regular_members[0].as_str()),
          severity: IssueSeverity::Warning,
          message: Arc::from(format!(
            "Workspace-member cohort [{}] would split under single-user filtering \
             (single-user members: [{}]). cargo-rail will unify the entire cohort atomically.",
            cohort_names.join(", "),
            low_usage_members.join(", ")
          )),
        });

        for member in &cohort {
          cohort_decisions
            .entry(Arc::clone(member))
            .or_default()
            .push(CohortDecision {
              summary: Arc::clone(&summary),
              members: cohort.to_vec(),
            });
        }
      }

      // Cohort members bypass single-user heuristics: all-or-nothing behavior by default.
      for member in &cohort {
        if !config.should_include(member) {
          config.include.push(member.to_string());
        }
      }
    }

    (config, issues, cohort_decisions)
  }

  fn record_decision_reason(
    decisions: &mut Vec<UnifyDecision>,
    dep_name: Arc<str>,
    subject: UnifyDecisionSubject,
    member: Option<Arc<str>>,
    target: Option<Arc<str>>,
    reason: UnifyDecisionReason,
  ) {
    Self::record_decision_reasons(decisions, dep_name, subject, member, target, vec![reason]);
  }

  fn record_decision_reasons(
    decisions: &mut Vec<UnifyDecision>,
    dep_name: Arc<str>,
    subject: UnifyDecisionSubject,
    member: Option<Arc<str>>,
    target: Option<Arc<str>>,
    mut reasons: Vec<UnifyDecisionReason>,
  ) {
    if reasons.is_empty() {
      return;
    }

    if let Some(existing) = decisions.iter_mut().find(|decision| {
      decision.dep_name == dep_name
        && decision.subject == subject
        && decision.member == member
        && decision.target == target
    }) {
      existing.reasons.append(&mut reasons);
      return;
    }

    decisions.push(UnifyDecision {
      dep_name,
      subject,
      member,
      target,
      reasons,
    });
  }

  /// Normalize a dependency path relative to the workspace root.
  ///
  /// Takes a path that's relative to a member's Cargo.toml and converts it
  /// to be relative to the workspace root.
  fn normalize_dep_path(&self, member_manifest_path: &Path, dep_path: &str) -> PathBuf {
    // Get the member's directory (parent of Cargo.toml)
    let member_dir = member_manifest_path.parent().unwrap_or(member_manifest_path);
    // Resolve the dep path relative to the member's directory
    let absolute_dep_path = member_dir.join(dep_path);

    // Canonicalize to resolve .. and symlinks (if path exists)
    let canonical_dep = absolute_dep_path.canonicalize().unwrap_or(absolute_dep_path.clone());

    // Make relative to workspace root
    if let Ok(relative) = canonical_dep.strip_prefix(&self.canonical_workspace_root) {
      return relative.to_path_buf();
    }

    // Fallback: if canonicalization moved outside workspace, try non-canonicalized path
    if let Ok(relative) = absolute_dep_path.strip_prefix(&self.workspace_root) {
      return relative.to_path_buf();
    }

    // Last resort: use the path as-is
    PathBuf::from(dep_path)
  }

  /// Normalize a workspace member directory path relative to the workspace root.
  fn normalize_workspace_member_path(&self, member_manifest_path: &Path) -> PathBuf {
    let member_dir = member_manifest_path.parent().unwrap_or(member_manifest_path);

    let canonical_member = member_dir.canonicalize().unwrap_or_else(|_| member_dir.to_path_buf());

    if let Ok(relative) = canonical_member.strip_prefix(&self.canonical_workspace_root) {
      return relative.to_path_buf();
    }

    if let Ok(relative) = member_dir.strip_prefix(&self.workspace_root) {
      return relative.to_path_buf();
    }

    member_dir.to_path_buf()
  }

  /// Analyze workspace and generate unification plan
  pub fn analyze(&self) -> RailResult<UnificationPlan> {
    let dep_count = self.manifests.all_dependencies().len();
    let mut workspace_deps = Vec::with_capacity(dep_count);
    let mut member_edits: FxHashMap<Arc<str>, Vec<MemberEdit>> = FxHashMap::default();
    let mut member_paths: FxHashMap<Arc<str>, PathBuf> = FxHashMap::default();
    let mut issues = self.cohort_issues.clone();
    issues.reserve(16); // Issues are rare, small pre-alloc
    let mut duplicates_cleaned = Vec::with_capacity(8);
    let mut version_mismatches = Vec::with_capacity(8);
    let mut dependency_decisions = Vec::with_capacity(dep_count);

    // Pre-compute workspace member metadata for reuse.
    let mut workspace_member_names: FxHashSet<Arc<str>> = FxHashSet::default();
    let mut workspace_member_paths: FxHashMap<Arc<str>, PathBuf> = FxHashMap::default();
    let mut workspace_member_versions: FxHashMap<Arc<str>, Version> = FxHashMap::default();

    // Build member_paths mapping from metadata (manifest file paths)
    for pkg in self.metadata.workspace_packages() {
      let member_name = Arc::from(pkg.name.as_str());
      let manifest_path = pkg.manifest_path.clone().into_std_path_buf();
      workspace_member_names.insert(Arc::clone(&member_name));
      workspace_member_paths.insert(
        Arc::clone(&member_name),
        self.normalize_workspace_member_path(&manifest_path),
      );
      workspace_member_versions.insert(Arc::clone(&member_name), pkg.version.clone());
      member_paths.insert(member_name, manifest_path);
    }

    progress!("Analyzing {} dependencies...", self.manifests.all_dependencies().len());

    // Process each dependency using the CandidateIterator
    // This encapsulates include_renamed branching and filtering logic
    for candidate in CandidateIterator::new(&self.manifests, &self.config) {
      let dep_key = candidate.dep_key;
      let usage_sites = candidate.usages;

      // Check for major version conflicts
      // Different major versions cannot be safely merged - behavior depends on config.
      let major_versions = find_major_version_conflicts(&usage_sites);
      if major_versions.len() > 1 {
        let versions_str: Vec<_> = major_versions.iter().map(|v| v.to_string()).collect();
        match self.config.major_version_conflict {
          MajorVersionConflict::Warn => {
            // Skip unification, emit warning - both versions stay in the graph
            issues.push(UnifyIssue {
              kind: UnifyIssueKind::General,
              dep_name: Arc::clone(&dep_key.name),
              severity: IssueSeverity::Warning,
              message: Arc::from(format!(
                "Multiple major versions detected (majors: {}) - skipping unification. \
                 Consider consolidating to a single major version to reduce duplicate compilation, \
                 or set major_version_conflict = \"bump\" to force unification to highest version.",
                versions_str.join(", ")
              )),
            });
            continue; // Skip this dependency, continue with others
          }
          MajorVersionConflict::Bump => {
            // Emit warning but continue with unification to highest version
            issues.push(UnifyIssue {
              kind: UnifyIssueKind::General,
              dep_name: Arc::clone(&dep_key.name),
              severity: IssueSeverity::Warning,
              message: Arc::from(format!(
                "Multiple major versions detected (majors: {}) - bumping to highest resolved version. \
                 This may break crates depending on older major versions.",
                versions_str.join(", ")
              )),
            });
            // Don't continue - fall through to use highest resolved version
          }
        }
      }

      // Check for exact version pins
      let has_exact_pin = usage_sites
        .iter()
        .any(|u| u.declared_version.as_ref().map(|v| is_exact_pin(v)).unwrap_or(false));

      if has_exact_pin {
        match self.config.exact_pin_handling {
          ExactPinHandling::Skip => {
            // Skip this dep entirely - don't unify exact pins
            Self::record_decision_reason(
              &mut dependency_decisions,
              Arc::clone(&dep_key.name),
              UnifyDecisionSubject::SkippedDependency,
              None,
              None,
              UnifyDecisionReason {
                code: UnifyDecisionCode::ExactPinSkipped,
                summary: Arc::from(
                  "Skipped because exact_pin_handling = \"skip\" and at least one usage is pinned exactly.",
                ),
                features: Vec::new(),
                members: usage_sites.iter().map(|usage| Arc::clone(&usage.used_by)).collect(),
                borrowed_from: Vec::new(),
                feature_paths: Vec::new(),
              },
            );
            continue;
          }
          ExactPinHandling::Warn => {
            issues.push(UnifyIssue {
              kind: UnifyIssueKind::General,
              dep_name: Arc::clone(&dep_key.name),
              severity: IssueSeverity::Warning,
              message: Arc::from(
                "Has exact version pin (=x.y.z) - converting to caret (^). \
                        Consider adding to unify.exclude or setting exact_pin_handling = \"skip\"",
              ),
            });
          }
          ExactPinHandling::Preserve => {
            // Will be handled below when constructing version_req
          }
        }
      }

      // Get version from metadata - but ONLY from direct dependencies of workspace members
      // This uses direct_dep_versions() which filters out transitive dependencies
      let versions = self.metadata.direct_dep_versions(&dep_key.name);

      // Workspace-member deps must be unifyable even when Cargo metadata excludes
      // non-default members from resolved direct-dependency traversal.
      let version = if versions.is_empty() {
        if workspace_member_names.contains(&dep_key.name) {
          match workspace_member_versions.get(&dep_key.name) {
            Some(v) => v,
            None => continue,
          }
        } else {
          continue; // Not a direct dependency of any workspace member
        }
      } else {
        // Get unique versions across all targets
        let unique_versions: FxHashSet<_> = versions.values().collect();

        // Always use highest version (cargo's resolver already picks highest compatible)
        // When targets resolve to different versions, we unify to highest
        match unique_versions.iter().max() {
          Some(max) if unique_versions.len() > 1 => {
            // Multiple versions found across targets - use highest
            // This is the "silent win" - we unify duplicates automatically
            let mut versions_found: Vec<Arc<str>> = unique_versions.iter().map(|v| Arc::from(v.to_string())).collect();
            versions_found.sort();
            duplicates_cleaned.push(DuplicateCleanup {
              dep_name: Arc::clone(&dep_key.name),
              versions_found,
              selected_version: Arc::from(max.to_string()),
            });
            *max
          }
          Some(version) => *version,
          None => continue, // Empty set - shouldn't happen given versions check above
        }
      };

      // Construct version requirement - preserve exact pins if configured
      let version_req = if has_exact_pin && self.config.exact_pin_handling == ExactPinHandling::Preserve {
        // Find the exact pin version from declared_version
        let exact_version = usage_sites
          .iter()
          .find_map(|u| u.declared_version.as_ref().filter(|v| is_exact_pin(v)))
          .cloned()
          .unwrap_or_else(|| format!("={}", version));
        // Try exact version first, fall back to caret format, then ultimate fallback
        VersionReq::parse(&exact_version)
          .or_else(|_| VersionReq::parse(&format!("^{}", version)))
          .unwrap_or_else(|_| VersionReq::default())
      } else {
        // Try caret format first, fall back to plain version, then ultimate fallback
        VersionReq::parse(&format!("^{}", version))
          .or_else(|_| VersionReq::parse(&version.to_string()))
          .unwrap_or_else(|_| VersionReq::default())
      };
      let mut decision_reasons = Vec::new();
      if has_exact_pin {
        match self.config.exact_pin_handling {
          ExactPinHandling::Preserve => {
            decision_reasons.push(UnifyDecisionReason {
              code: UnifyDecisionCode::ExactPinPreserved,
              summary: Arc::from(format!(
                "Preserved exact pin in workspace.dependencies because exact_pin_handling = \"preserve\" ({}).",
                version_req
              )),
              features: Vec::new(),
              members: usage_sites.iter().map(|usage| Arc::clone(&usage.used_by)).collect(),
              borrowed_from: Vec::new(),
              feature_paths: Vec::new(),
            });
          }
          ExactPinHandling::Warn => {
            decision_reasons.push(UnifyDecisionReason {
              code: UnifyDecisionCode::ExactPinWarnCaret,
              summary: Arc::from(format!(
                "Converted exact pin to {} because exact_pin_handling = \"warn\".",
                version_req
              )),
              features: Vec::new(),
              members: usage_sites.iter().map(|usage| Arc::clone(&usage.used_by)).collect(),
              borrowed_from: Vec::new(),
              feature_paths: Vec::new(),
            });
          }
          ExactPinHandling::Skip => {}
        }
      }

      // Check for version mismatches with existing workspace.dependencies
      if let Some(existing_ws_dep) = self.existing_workspace_deps.get(&*dep_key.name)
        && let Some(ref ws_version) = existing_ws_dep.version
      {
        for usage in &usage_sites {
          if let Some(ref declared) = usage.declared_version
            && !versions_compatible(declared, ws_version)
          {
            version_mismatches.push(VersionMismatch {
              member: Arc::clone(&usage.used_by),
              dep_name: Arc::clone(&dep_key.name),
              member_version: Arc::from(declared.as_str()),
              workspace_version: Arc::from(ws_version.as_str()),
            });

            // Add as issue based on strictness config
            let severity = if self.config.strict_version_compat {
              IssueSeverity::Error
            } else {
              IssueSeverity::Warning
            };
            issues.push(UnifyIssue {
              kind: UnifyIssueKind::General,
              dep_name: Arc::clone(&dep_key.name),
              severity,
              message: Arc::from(format!(
                "{} declares \"{}\" but workspace.dependencies has \"{}\". \
                 Converting to workspace = true may break this crate.",
                usage.used_by, declared, ws_version
              )),
            });
          }
        }
      }

      let is_workspace_member_dep = workspace_member_names.contains(&dep_key.name);

      // For workspace-member dependencies, we only unify source/version/path atomically.
      // Feature policy remains local per member to avoid enabling member features globally
      // (for example Tokio's "full" leaking into WASM jobs via workspace.dependencies).
      //
      // Non-member deps keep existing feature/default-features unification behavior.
      let feature_paths = Self::feature_enabling_paths(&usage_sites);
      let (mut features, default_features, feature_reason) = if is_workspace_member_dep {
        (
          std::collections::BTreeSet::new(),
          self.manifests.default_features_policy(dep_key).unwrap_or(true),
          Some(UnifyDecisionReason {
            code: UnifyDecisionCode::WorkspaceMemberFeaturesLocal,
            summary: Arc::from(
              "Kept workspace-member features on each exact declaration so target and dependency domains stay isolated.",
            ),
            features: Vec::new(),
            members: usage_sites.iter().map(|usage| Arc::clone(&usage.used_by)).collect(),
            borrowed_from: Vec::new(),
            feature_paths: feature_paths.clone(),
          }),
        )
      } else {
        // Default-feature policy spans every declaration because inherited
        // declarations cannot disable a workspace-level default.
        // When include_renamed = true, check across all package variants
        let has_mixed_defaults = if self.config.include_renamed {
          self.manifests.package_has_mixed_defaults(&dep_key.name)
        } else {
          self.manifests.has_mixed_defaults(dep_key)
        };

        // Compute features - when include_renamed = true, aggregate across all variants
        // Note: intersection doesn't make sense across renamed deps (they're separate usages)
        // so we use union to ensure all needed features are included
        if self.config.include_renamed {
          // For package-level aggregation, always use union strategy
          // (renamed deps typically have distinct feature needs)
          let features = self.manifests.compute_package_union(&dep_key.name);
          // When mixed defaults detected, use default-features = true to preserve
          // features for crates that rely on them (same logic as non-include_renamed path)
          let df = self
            .manifests
            .package_default_features_policy(&dep_key.name)
            .unwrap_or(true);
          let mut feature_list: Vec<Arc<str>> = features.iter().map(|feature| Arc::from(feature.as_str())).collect();
          feature_list.sort();
          (
            features,
            df,
            Some(UnifyDecisionReason {
              code: UnifyDecisionCode::FeatureUnion,
              summary: Arc::from(
                "Used union because include_renamed aggregates distinct Cargo.toml keys at the package level.",
              ),
              features: feature_list,
              members: usage_sites.iter().map(|usage| Arc::clone(&usage.used_by)).collect(),
              borrowed_from: Vec::new(),
              feature_paths: feature_paths.clone(),
            }),
          )
        } else {
          // Standard aliases use the smallest globally safe baseline. Every
          // declaration keeps its additional features locally.
          let intersection = self.manifests.compute_intersection(dep_key);
          let mut feature_list: Vec<Arc<str>> =
            intersection.iter().map(|feature| Arc::from(feature.as_str())).collect();
          feature_list.sort();
          let summary = if intersection.is_empty() {
            "Kept the workspace feature baseline empty because no explicit feature is shared by every declaration."
          } else if has_mixed_defaults {
            "Used the shared feature intersection and disabled workspace defaults because declarations disagree on defaults."
          } else {
            "Used the intersection to keep only explicit features shared by every declaration."
          };
          (
            intersection,
            self.manifests.default_features_policy(dep_key).unwrap_or(true),
            Some(UnifyDecisionReason {
              code: UnifyDecisionCode::FeatureIntersection,
              summary: Arc::from(summary),
              features: feature_list,
              members: usage_sites.iter().map(|usage| Arc::clone(&usage.used_by)).collect(),
              borrowed_from: Vec::new(),
              feature_paths: feature_paths.clone(),
            }),
          )
        }
      };
      if !default_features {
        features.remove("default");
      }
      if let Some(reason) = feature_reason {
        decision_reasons.push(reason);
      }

      // Note: Target constraints stay in member manifests, not workspace.dependencies.
      // Members use `[target.'cfg(...)'.dependencies] dep = { workspace = true }`.
      // We don't set target on UnifiedDep because [workspace.dependencies] cannot
      // have [target] sections in virtual workspaces, and even in non-virtual ones
      // it's cleaner to keep target constraints where they're used.
      let target = None;

      // Get users - use Arc<str> to share allocations
      let users: FxHashSet<Arc<str>> = usage_sites.iter().map(|u| Arc::clone(&u.used_by)).collect();

      // Workspace members must always carry `path` so member-to-member deps stay local.
      // This prevents dual-resolution in fresh lockfiles (local member + crates.io package).
      let dep_path: Option<PathBuf> = if is_workspace_member_dep {
        workspace_member_paths.get(&dep_key.name).cloned()
      } else if self.config.include_paths {
        // Include explicit path deps for non-member packages only when requested.
        usage_sites.iter().find_map(|u| {
          u.path.as_ref().and_then(|p| {
            u.manifest_path
              .as_ref()
              .map(|manifest_path| self.normalize_dep_path(manifest_path, p))
          })
        })
      } else {
        None // include_paths = false, skip all path deps
      };

      // Compute the unified features (this is what workspace.dependencies should have)
      let computed_features: Vec<Arc<str>> = features.into_iter().map(Arc::from).collect();
      if let Some(cohort_entries) = self.cohort_decisions.get(&dep_key.name) {
        for entry in cohort_entries {
          decision_reasons.push(UnifyDecisionReason {
            code: UnifyDecisionCode::CohortEnforced,
            summary: Arc::clone(&entry.summary),
            features: Vec::new(),
            members: entry.members.clone(),
            borrowed_from: Vec::new(),
            feature_paths: Vec::new(),
          });
        }
      }

      // Check if this dep already exists in workspace.dependencies
      let existing_dep = self.existing_workspace_deps.get(&*dep_key.name);

      // Determine if we need to update workspace.dependencies
      // Update if: dep doesn't exist, OR features differ, OR default-features differs, OR has path
      let needs_workspace_update = match existing_dep {
        None => true, // Doesn't exist, need to add
        Some(existing) => {
          // Check if features are different (compare as &str)
          let mut existing_features: Vec<&str> = existing.features.iter().map(String::as_str).collect();
          let mut computed: Vec<&str> = computed_features.iter().map(|s| &**s).collect();
          existing_features.sort();
          computed.sort();

          // Also check if existing path is missing or differs from resolved path.
          let path_differs = match (&dep_path, &existing.path) {
            (Some(new_path), Some(existing_path)) => Path::new(existing_path) != new_path,
            (Some(_), None) => true,
            _ => false,
          };

          existing_features != computed || existing.default_features != default_features || path_differs
        }
      };

      // The features to use for local feature calculation
      // Use computed features (which will be what goes in workspace.dependencies)
      let workspace_features = computed_features.clone();

      // Create unified dep if workspace needs update
      if needs_workspace_update {
        let unified = UnifiedDep {
          name: Arc::clone(&dep_key.name),
          version_req,
          features: workspace_features.clone(),
          default_features,
          used_by: users.into_iter().collect(),
          target,
          path: dep_path, // Include path for workspace member deps
        };
        workspace_deps.push(unified);
      }
      Self::record_decision_reasons(
        &mut dependency_decisions,
        Arc::clone(&dep_key.name),
        UnifyDecisionSubject::WorkspaceDependency,
        None,
        None,
        decision_reasons,
      );

      // Compute member edits for ALL dep kinds (normal, dev, build)
      // Per design doc: all dep kinds should be unified, not just Normal
      // This happens whether or not the dep already exists in workspace.dependencies
      for usage in usage_sites {
        // Compute local features (member features - workspace features)
        // Convert to Arc<str> and filter out features already in workspace
        let mut local_features: Vec<Arc<str>> = usage
          .unconditional_features
          .iter()
          .filter(|f| !workspace_features.iter().any(|wf| &**wf == *f))
          .map(|f| Arc::from(f.as_str()))
          .collect();
        if !default_features && usage.default_features && !local_features.iter().any(|feature| &**feature == "default")
        {
          local_features.push(Arc::from("default"));
        }
        local_features.sort();
        local_features.dedup();

        // Use the cargo_toml_key from the usage - this is the actual key in Cargo.toml
        // For renamed deps like `old_serde = { package = "serde" }`, this is "old_serde"
        // This is critical for include_renamed mode where usage_sites are aggregated
        // across multiple dep_keys for the same package
        let edit = MemberEdit::UseWorkspace {
          dep_name: Arc::clone(&usage.cargo_toml_key),
          dep_kind: usage.kind,
          target: usage.target.as_ref().map(|t| Arc::from(t.as_str())), // Preserve target for correct section
          local_features,
          is_optional: usage.optional,
        };

        member_edits.entry(Arc::clone(&usage.used_by)).or_default().push(edit);
      }
    }

    // Handle transitive fragmentation
    let transitive_pins = if self.config.transitive_pinning.is_some() {
      TransitivePlanner::new(&self.metadata).find_pins()
    } else {
      Vec::new()
    };
    for pin in &transitive_pins {
      Self::record_decision_reason(
        &mut dependency_decisions,
        Arc::clone(&pin.name),
        UnifyDecisionSubject::TransitivePin,
        None,
        None,
        UnifyDecisionReason {
          code: UnifyDecisionCode::TransitivePin,
          summary: Arc::from(
            "Pinned transitively because target-specific feature resolution would otherwise fragment the graph.",
          ),
          features: pin.features.clone(),
          members: Vec::new(),
          borrowed_from: Vec::new(),
          feature_paths: Vec::new(),
        },
      );
    }

    // Compute MSRV if enabled
    let computed_msrv = if let Some(source) = self.config.msrv_policy.source() {
      progress!("Computing MSRV from dependency graph...");
      self.metadata.compute_msrv_with_config(&self.workspace_root, source)
    } else {
      None
    };

    // Optionally ensure every workspace member inherits MSRV from the workspace.
    //
    // This is only safe when we have (or will write) `[workspace.package].rust-version`.
    // When msrv is disabled or cannot be computed, `rust-version = { workspace = true }`
    // would cause Cargo to error if the workspace value is absent.
    if self.config.msrv_policy.inherits() {
      if computed_msrv.is_none() {
        issues.push(UnifyIssue {
          kind: UnifyIssueKind::General,
          dep_name: Arc::from("rust-version"),
          severity: IssueSeverity::Warning,
          message: Arc::from(
            "MSRV inheritance is enabled but no workspace MSRV could be determined; skipping inheritance enforcement",
          ),
        });
      } else {
        progress!("Enforcing MSRV inheritance on workspace members...");
        for (member_name, manifest_path) in &member_paths {
          if member_manifest_inherits_msrv(manifest_path)? {
            continue;
          }
          member_edits
            .entry(Arc::clone(member_name))
            .or_default()
            .push(MemberEdit::EnforceMsrvInheritance);
        }
      }
    }

    // Diagnostics are unconditional. Destructive feature edits still require
    // the explicit closed-consumer proof carried by `consumer_scope`.
    let feature_pruner = FeaturePruner::new(&self.metadata, &self.manifests, &self.config, &self.target_cfg_sets);
    progress!("Scanning for dead features in resolved graph...");
    let (mut pruned_features, optional_features, reachable_features) = feature_pruner.scan();

    // Detect unused dependencies
    let unused_finder = UnusedDepFinder::new(
      &self.workspace_root,
      &self.metadata,
      &self.manifests,
      &self.target_cfg_sets,
      &pruned_features,
      self.config.consumer_scope == ConsumerScope::Workspace,
      self.compiler_cache_identity.as_ref(),
    );
    progress!("Detecting unused dependencies...");
    let mut unused_deps = unused_finder.find();
    issues.extend(unused_finder.uncertainty_issues());

    self.retain_features_with_live_optional_activations(&mut pruned_features, &unused_deps, &mut issues);

    // Generate removal edits for dead features (empty no-ops)
    for (crate_name, edits) in feature_pruner.generate_prune_edits(&pruned_features) {
      member_edits.entry(crate_name).or_default().extend(edits);
    }

    // Detect undeclared features
    // Find cases where a crate uses features that it didn't declare in Cargo.toml
    // This happens when Cargo's feature unification "borrows" features from other members
    progress!("Checking for undeclared feature dependencies...");
    let (undeclared_features, causality_issues) = self.detect_undeclared_features()?;
    issues.extend(causality_issues);

    // A causal feature repair and a dependency removal cannot target the same
    // declaration. Feature borrowing proves the declaration participates in
    // the resolved graph even when source linting sees no direct symbol use.
    unused_deps.retain(|unused| {
      !undeclared_features.iter().any(|feature| {
        feature.member == unused.member && feature.dep_name == unused.dep_name && feature.dep_kind == unused.kind
      })
    });
    if !unused_deps.is_empty() {
      progress!(
        "Generating removal edits for {} unused dependencies...",
        unused_deps.len()
      );
      for (member, edits) in unused_finder.generate_removal_edits(&unused_deps) {
        member_edits.entry(Arc::from(member)).or_default().extend(edits);
      }
    }

    if !undeclared_features.is_empty() {
      progress!(
        "Generating fixes for {} undeclared feature issues...",
        undeclared_features.len()
      );
      for uf in &undeclared_features {
        let edit = MemberEdit::AddFeatures {
          dep_name: Arc::clone(&uf.dep_name),
          dep_kind: uf.dep_kind,
          target: uf.target.clone(),
          features_to_add: uf.undeclared_features.clone(),
        };
        member_edits.entry(Arc::clone(&uf.member)).or_default().push(edit);
        Self::record_decision_reason(
          &mut dependency_decisions,
          Arc::clone(&uf.dep_name),
          UnifyDecisionSubject::UndeclaredFeatureFix,
          Some(Arc::clone(&uf.member)),
          uf.target.clone(),
          UnifyDecisionReason {
            code: UnifyDecisionCode::UndeclaredFeatureFix,
            summary: Arc::from(format!(
              "Added missing features to {} so it stops borrowing resolver state from other workspace members.",
              uf.member
            )),
            features: uf.undeclared_features.clone(),
            members: vec![Arc::clone(&uf.member)],
            borrowed_from: uf.borrowed_from.clone(),
            feature_paths: uf.enabling_paths.clone(),
          },
        );
      }
    }

    // Run validation
    let validation_results = self.validate_targets()?;

    // Defensive cleanup: avoid representing "no-op members" as pending changes.
    member_edits.retain(|_, edits| !edits.is_empty());
    for decision in &mut dependency_decisions {
      decision.reasons.sort_by(|left, right| {
        left
          .code
          .as_str()
          .cmp(right.code.as_str())
          .then_with(|| left.summary.cmp(&right.summary))
      });
    }
    dependency_decisions.sort_by(|left, right| {
      left
        .dep_name
        .cmp(&right.dep_name)
        .then_with(|| left.subject.as_str().cmp(right.subject.as_str()))
        .then_with(|| left.member.cmp(&right.member))
        .then_with(|| left.target.cmp(&right.target))
    });

    Ok(UnificationPlan {
      workspace_deps,
      member_edits,
      member_paths,
      transitive_pins,
      validation_results,
      issues,
      computed_msrv,
      duplicates_cleaned,
      pruned_features,
      reachable_features,
      optional_features,
      version_mismatches,
      unused_deps,
      undeclared_features,
      dependency_decisions,
    })
  }

  /// Validate that the unification works for all targets
  fn validate_targets(&self) -> RailResult<Vec<ValidationResult>> {
    let targets = self.metadata.targets();
    let mut results = Vec::with_capacity(targets.len());

    for target in targets {
      // Simple check: does metadata load successfully for this target?
      // In production, you might want to run `cargo check --target=X`
      let success = self.metadata.get(target).is_some();

      results.push(ValidationResult {
        target: Arc::from(target),
        success,
        error: if !success {
          Some(Arc::from("Failed to load metadata"))
        } else {
          None
        },
      });
    }

    Ok(results)
  }

  // ============================================================================
  // Helper methods for detect_undeclared_features
  // ============================================================================

  /// Get workspace member package names
  fn workspace_member_names(&self) -> FxHashSet<String> {
    self.metadata.workspace_packages().map(|p| p.name.to_string()).collect()
  }

  /// Build workspace baseline features from [workspace.dependencies]
  ///
  /// These are workspace policy - members don't need to re-declare them.
  fn workspace_baseline_features(&self) -> FxHashMap<String, FxHashSet<String>> {
    self
      .existing_workspace_deps
      .iter()
      .map(|(name, dep)| {
        let mut feats: FxHashSet<String> = dep.features.iter().cloned().collect();
        if dep.default_features {
          feats.insert("default".to_string());
        }
        (name.clone(), feats)
      })
      .collect()
  }

  fn retain_features_with_live_optional_activations(
    &self,
    pruned_features: &mut Vec<PrunedFeature>,
    unused_dependencies: &[UnusedDep],
    issues: &mut Vec<UnifyIssue>,
  ) {
    pruned_features.retain(|feature| {
      let Some(member) = self
        .manifests
        .members
        .iter()
        .find(|member| member.package_name == feature.crate_name.as_ref())
      else {
        return false;
      };
      let live_activation = member.dependencies.iter().any(|(_, usages)| {
        usages.iter().any(|usage| {
          usage.optional
            && feature.declared_edges.iter().any(|edge| {
              crate::cargo::feature_scanner::feature_edge_references_dependency(edge, &usage.cargo_toml_key)
            })
            && !unused_dependencies.iter().any(|unused| {
              unused.member == feature.crate_name
                && unused.dep_name == usage.cargo_toml_key
                && unused.kind == usage.kind
            })
        })
      });
      if !live_activation {
        return true;
      }
      issues.push(UnifyIssue {
        kind: UnifyIssueKind::General,
        dep_name: Arc::clone(&feature.feature_name),
        severity: IssueSeverity::Warning,
        message: Arc::from(format!(
          "preserved feature '{}': an activated optional dependency has positive or incomplete compiler evidence",
          feature.feature_name
        )),
      });
      false
    });
  }

  /// Track which members contribute which features (beyond workspace baseline)
  ///
  /// Retains the exact dependency kind and target section for every provider.
  fn track_feature_sources(&self, workspace_baseline: &FxHashMap<String, FxHashSet<String>>) -> FeatureSources {
    let mut feature_sources: FeatureSources = FxHashMap::default();

    for member in &self.manifests.members {
      for (dep_key, usages) in &member.dependencies {
        let baseline = workspace_baseline.get(&*dep_key.name);
        // Compute dep name string once per (member, dep) pair instead of per-feature
        let dep_name_str = dep_key.name.to_string();

        for usage in usages {
          let mut feats: FxHashSet<&String> = usage.unconditional_features.iter().collect();
          feats.extend(usage.conditional_features.iter());

          for feat in feats {
            // Skip features in workspace baseline (workspace policy)
            if baseline.is_some_and(|b| b.contains(feat)) {
              continue;
            }

            let domain = (dep_name_str.clone(), usage.cargo_toml_key.to_string(), usage.kind);
            feature_sources
              .entry(domain)
              .or_default()
              .entry(feat.clone())
              .or_default()
              .push(FeatureProvider {
                member: member.package_name.clone(),
                alias: Arc::clone(&usage.cargo_toml_key),
                kind: usage.kind,
                target: usage.target.clone(),
                default_features: usage.default_features,
                optional: usage.optional,
              });
          }
        }
      }
    }

    for features in feature_sources.values_mut() {
      for providers in features.values_mut() {
        providers.sort_by(|left, right| {
          left
            .member
            .cmp(&right.member)
            .then_with(|| left.alias.cmp(&right.alias))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.target.cmp(&right.target))
        });
        providers.dedup();
      }
    }
    feature_sources
  }

  /// Detect dependencies where resolved features exceed declared features
  ///
  /// This catches cases where a crate relies on Cargo's feature unification to
  /// "borrow" features from other workspace members. After workspace dependency
  /// unification, standalone builds (cargo test -p <crate>) will fail because
  /// the borrowed features are no longer available.
  ///
  /// Key distinctions:
  /// - **Workspace policy**: Features in `[workspace.dependencies]` are intentional
  ///   workspace-level declarations. Members using these don't need to re-declare.
  /// - **Member borrowing**: Features that only come from another member are
  ///   implicit coupling and should be flagged.
  /// - **Conditional features**: Features declared via `[features]` table (e.g.,
  ///   `my-feature = ["dep/feat"]`) count as declared - the author made an
  ///   intentional choice.
  fn detect_undeclared_features(&self) -> RailResult<(Vec<UndeclaredFeature>, Vec<UnifyIssue>)> {
    let workspace_member_names = self.workspace_member_names();
    let workspace_baseline = self.workspace_baseline_features();
    let feature_sources = self.track_feature_sources(&workspace_baseline);

    let mut undeclared = Vec::with_capacity(16);
    let mut skipped_platform_features: Vec<(String, String, String)> = Vec::with_capacity(8);
    let mut skipped_workspace_member_deps: FxHashSet<String> = FxHashSet::default();

    // Find borrowed features for each member
    for member in &self.manifests.members {
      for (dep_key, usages) in &member.dependencies {
        // Skip workspace member deps - their optional features are intentional opt-ins
        if workspace_member_names.contains(&*dep_key.name) {
          skipped_workspace_member_deps.insert(dep_key.name.to_string());
          continue;
        }

        let Some(resolved_dependency) = self.metadata.resolve_dependency(
          &member.package_id,
          &dep_key.name,
          dep_key.is_renamed().then(|| dep_key.alias()),
        ) else {
          continue;
        };
        let resolved_features = self.metadata.resolved_features_for(&resolved_dependency.package_ids);
        let default_features = self
          .metadata
          .default_feature_closure_for(&resolved_dependency.package_ids);

        for usage in usages {
          let domain = (dep_key.name.to_string(), dep_key.alias().to_string(), usage.kind);
          let Some(feat_sources) = feature_sources.get(&domain) else {
            continue;
          };
          if let Some(borrowed) = self.find_borrowed_features_for_usage(
            member,
            dep_key,
            usage,
            feat_sources,
            &resolved_features,
            &default_features,
            &mut skipped_platform_features,
          ) {
            undeclared.push(borrowed);
          }
        }
      }
    }

    // Log skipped items
    if !skipped_workspace_member_deps.is_empty() {
      progress!(
        "  Skipped undeclared feature detection for {} workspace member deps (features are opt-in)",
        skipped_workspace_member_deps.len()
      );
    }
    if !skipped_platform_features.is_empty() {
      progress!(
        "  Skipped {} platform-specific undeclared features (target-constrained declarations)",
        skipped_platform_features.len()
      );
    }

    skipped_platform_features.sort();
    skipped_platform_features.dedup();
    let causality_issues = skipped_platform_features
      .into_iter()
      .map(|(member, dependency, feature)| UnifyIssue {
        kind: UnifyIssueKind::General,
        dep_name: Arc::from(dependency.as_str()),
        severity: IssueSeverity::Warning,
        message: Arc::from(format!(
          "preserved borrowed feature candidate '{feature}' for member '{member}': provider target coverage does not authorize widening the dependency declaration"
        )),
      })
      .collect();

    let mut by_member: std::collections::BTreeMap<String, std::collections::BTreeMap<String, Vec<String>>> =
      std::collections::BTreeMap::new();
    for feature in &undeclared {
      by_member
        .entry(feature.member.to_string())
        .or_default()
        .entry(feature.dep_name.to_string())
        .or_default()
        .extend(feature.undeclared_features.iter().map(ToString::to_string));
    }
    let mut required = std::collections::BTreeMap::new();
    for (member, dependencies) in by_member {
      let candidates: Vec<_> = dependencies.into_iter().collect();
      let missing = crate::compiler::standalone_missing_features(&self.workspace_root, &member, &candidates)?;
      required.extend(
        missing
          .into_iter()
          .map(|((dependency, feature), paths)| ((member.clone(), dependency, feature), paths)),
      );
    }
    for feature in &mut undeclared {
      feature.undeclared_features.retain(|candidate| {
        required.contains_key(&(
          feature.member.to_string(),
          feature.dep_name.to_string(),
          candidate.to_string(),
        ))
      });
      feature.required_by = feature
        .undeclared_features
        .iter()
        .flat_map(|candidate| {
          required
            .get(&(
              feature.member.to_string(),
              feature.dep_name.to_string(),
              candidate.to_string(),
            ))
            .into_iter()
            .flatten()
        })
        .map(|path| Arc::from(path.as_str()))
        .collect();
      feature.required_by.sort();
      feature.required_by.dedup();
    }
    undeclared.retain(|feature| !feature.undeclared_features.is_empty());

    Ok((undeclared, causality_issues))
  }

  /// Find borrowed features for a single dependency usage
  #[allow(clippy::too_many_arguments)]
  fn find_borrowed_features_for_usage(
    &self,
    member: &crate::cargo::manifest_analyzer::ParsedManifest,
    dep_key: &crate::cargo::manifest_analyzer::DepKey,
    usage: &crate::cargo::manifest_analyzer::DepUsage,
    feat_sources: &FxHashMap<String, Vec<FeatureProvider>>,
    resolved_features: &std::collections::BTreeSet<String>,
    default_features: &std::collections::BTreeSet<String>,
    skipped_platform_features: &mut Vec<(String, String, String)>,
  ) -> Option<UndeclaredFeature> {
    // Get this usage's declared features
    let mut declared: FxHashSet<String> = usage.unconditional_features.iter().cloned().collect();
    declared.extend(usage.conditional_features.iter().cloned());
    if usage.default_features {
      declared.insert("default".to_string());
    }

    let mut borrowed_features: Vec<Arc<str>> = Vec::with_capacity(feat_sources.len());
    let mut borrowed_from_members: FxHashSet<Arc<str>> = FxHashSet::default();
    let mut enabling_paths = Vec::new();

    for (feat, providers) in feat_sources {
      if !resolved_features.contains(feat) {
        continue;
      }
      // Skip if this member declares the feature itself
      if declared.contains(feat) {
        continue;
      }

      // Cargo already guarantees this feature through the dependency's own
      // default closure; writing it explicitly would be redundant.
      if usage.default_features && default_features.contains(feat) {
        continue;
      }

      // Skip if configured to skip this feature pattern
      if self.config.should_skip_undeclared_feature(feat) {
        continue;
      }

      let compatible_providers: Vec<_> = providers
        .iter()
        .filter(|provider| self.provider_covers_usage_target(provider, usage.target.as_deref()))
        .collect();
      if compatible_providers.is_empty() {
        skipped_platform_features.push((member.package_name.clone(), dep_key.name.to_string(), feat.clone()));
        continue;
      }

      // This feature is borrowed from other members
      borrowed_features.push(Arc::from(feat.as_str()));
      for provider in compatible_providers {
        enabling_paths.push(FeatureEnablingPath {
          member: Arc::from(provider.member.as_str()),
          alias: Arc::clone(&provider.alias),
          dependency_kind: provider.kind,
          target: provider.target.as_deref().map(Arc::from),
          features: vec![Arc::from(feat.as_str())],
          default_features: provider.default_features,
          optional: provider.optional,
        });
        if provider.member != member.package_name {
          borrowed_from_members.insert(Arc::from(provider.member.as_str()));
        }
      }
    }

    if borrowed_features.is_empty() {
      return None;
    }

    borrowed_features.sort();
    let mut borrowed_from: Vec<Arc<str>> = borrowed_from_members.into_iter().collect();
    borrowed_from.sort();
    enabling_paths.sort();
    enabling_paths.dedup();

    Some(UndeclaredFeature {
      member: Arc::from(member.package_name.as_str()),
      dep_name: Arc::clone(&dep_key.name),
      undeclared_features: borrowed_features,
      declared_features: declared.into_iter().map(|s| Arc::from(s.as_str())).collect(),
      resolved_features: resolved_features.iter().map(|s| Arc::from(s.as_str())).collect(),
      dep_kind: usage.kind,
      target: usage.target.as_ref().map(|t| Arc::from(t.as_str())),
      borrowed_from,
      enabling_paths,
      required_by: Vec::new(),
    })
  }

  fn provider_covers_usage_target(&self, provider: &FeatureProvider, consumer: Option<&str>) -> bool {
    if consumer.is_none() {
      return provider.target.is_none();
    }
    if provider.target.is_none() {
      return true;
    }
    let targets = self.metadata.targets();
    let consumer_targets: std::collections::BTreeSet<_> = targets
      .iter()
      .copied()
      .filter(|target| {
        consumer.is_none_or(|constraint| {
          target_constraint_matches_target(constraint, target, self.target_cfg_sets.get(*target))
        })
      })
      .collect();
    if consumer_targets.is_empty() {
      return false;
    }
    let provider_targets: std::collections::BTreeSet<_> = targets
      .iter()
      .copied()
      .filter(|target| {
        provider.target.as_deref().is_none_or(|constraint| {
          target_constraint_matches_target(constraint, target, self.target_cfg_sets.get(*target))
        })
      })
      .collect();
    consumer_targets.is_subset(&provider_targets)
  }
}

fn member_manifest_inherits_msrv(manifest_path: &Path) -> RailResult<bool> {
  let content =
    std::fs::read_to_string(manifest_path).context(format!("Failed to read {}", manifest_path.display()))?;
  let doc: toml_edit::DocumentMut = content
    .parse()
    .context(format!("Failed to parse {}", manifest_path.display()))?;

  let Some(pkg) = doc.get("package").and_then(|p| p.as_table()) else {
    // No [package] section (likely a virtual workspace member). Nothing to enforce.
    return Ok(true);
  };

  let Some(rv) = pkg.get("rust-version") else {
    return Ok(false);
  };

  let Some(rv_tbl) = rv.as_table_like() else {
    return Ok(false);
  };

  Ok(rv_tbl.get("workspace").and_then(|v| v.as_bool()) == Some(true))
}
