//! Workspace dependency unification
//!
//! Eliminates workspace-hack crates by unifying dependencies in [workspace.dependencies]
//! and converting members to use workspace inheritance.
//!
//! # Superior Resolution-Based Algorithm
//!
//! Unlike other tools that only look at version requirement syntax, cargo-rail uses
//! Cargo's actual dependency resolution to determine compatibility:
//!
//! 1. **Resolution-first checking**: Check what Cargo actually resolved
//!    - If all members resolve to the same version → compatible!
//!    - Example: `"1.0"` and `"^1.5"` both resolve to `1.5.3` → unified
//!
//! 2. **Syntactic merging fallback**: If no resolution available, merge compatible requirements
//!    - `^1.2` and `^1.3` → `^1.3` (picks most restrictive)
//!    - Only fails when truly incompatible
//!
//! 3. **Resolved features**: Uses actual enabled features, not just declared
//!    - Captures transitive feature activation
//!    - Produces minimal, accurate feature unions
//!
//! # Strategy
//!
//! 1. Collect all dependencies from workspace members (using resolved features)
//! 2. Group by dependency name
//! 3. Check actual Cargo resolution for version compatibility
//! 4. Fallback to syntactic version requirement merging
//! 5. Detect remaining conflicts and edge cases
//! 6. Compute unified feature sets (union of resolved features)
//! 7. Generate unification plan
//! 8. Apply plan to Cargo.toml files (workspace + members)
//!
use super::WorkspaceMetadata;
use crate::error::RailResult;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

mod collector;
mod config;
mod feature_cache;
mod issue_detection;
mod minimize;
mod path_handling;
mod plan;
mod report;
mod resolution;
mod strategy;
mod transitive;
mod types;
mod unifier;
mod version_merge;

// Re-export public types
pub use config::UnifyConfig;
pub use minimize::{MinimizationReport, minimize_workspace_features};
pub use report::UnifyReport;
pub use types::{IssueSeverity, MemberEdit, UnificationPlan, UnificationStats, UnifiedDep};

/// Workspace dependency unifier
///
/// Orchestrates the unification process using resolved features from the dependency graph.
pub struct WorkspaceUnifier<'a> {
  metadata: &'a WorkspaceMetadata,
  config: UnifyConfig,
}

/// Result of analyzing a single dependency
struct DependencyAnalysisResult {
  workspace_dep: Option<UnifiedDep>,
  member_edits: Vec<(String, MemberEdit)>,
  issues: Vec<types::UnificationIssue>,
  compilations_saved: usize,
}

impl<'a> WorkspaceUnifier<'a> {
  /// Create with custom configuration
  pub fn with_config(metadata: &'a WorkspaceMetadata, config: UnifyConfig) -> Self {
    Self { metadata, config }
  }

  /// Analyze workspace and generate unification plan
  pub fn analyze(&self) -> RailResult<UnificationPlan> {
    // 1. Collect all dependency instances from workspace members
    let dep_instances = collector::collect_dependencies(self.metadata, self.config.use_all_features);

    // 2. Group by dependency name
    let grouped = collector::group_by_name(dep_instances, self.metadata);

    // 3. Analyze each dependency group IN PARALLEL using rayon
    let total_deps = grouped.len();

    // Convert HashMap to Vec for parallel iteration
    let dep_groups: Vec<_> = grouped.into_iter().collect();

    // Parallel analysis of independent dependencies
    let results: Vec<DependencyAnalysisResult> = dep_groups
      .into_par_iter()
      .filter_map(|(dep_name, instances)| {
        // Analyze this dependency (returns None if excluded/skipped)
        self.analyze_dependency(dep_name, instances).transpose()
      })
      .collect::<Result<Vec<_>, _>>()?;

    // 4. Aggregate results from parallel analysis
    let mut workspace_deps = Vec::new();
    let mut member_edits: HashMap<String, Vec<MemberEdit>> = HashMap::new();
    let mut issues = Vec::new();
    let mut compilations_saved = 0;

    for result in results {
      if let Some(dep) = result.workspace_dep {
        workspace_deps.push(dep);
      }

      for (member, edit) in result.member_edits {
        member_edits.entry(member).or_default().push(edit);
      }

      issues.extend(result.issues);
      compilations_saved += result.compilations_saved;
    }

    let stats = UnificationStats {
      total_deps,
      unified_count: workspace_deps.len(),
      issue_count: issues.len(),
      compilations_saved,
    };

    // 5. Detect multi-version conflicts for REMAINING unresolved dependencies
    // This runs AFTER we've attempted unification, so it only reports true conflicts
    // that couldn't be resolved through resolution-based or syntactic merging.
    //
    // IMPORTANT: Only report these issues if auto_resolve_version_conflicts is disabled.
    // When auto_resolve is enabled, Cargo's resolution is the source of truth - if multiple
    // versions exist in the resolved graph, that's because Cargo legitimately needs them
    // (e.g., for different dependency paths with incompatible requirements). These are
    // transitive dependencies we don't control directly.
    if !self.config.auto_resolve_version_conflicts {
      let multi_version_issues = issue_detection::detect_multi_version_conflicts(self.metadata)?;

      // Only add multi-version issues for deps that weren't successfully unified
      let unified_names: HashSet<_> = workspace_deps.iter().map(|d| &d.name).collect();
      for issue in multi_version_issues {
        // Skip if we successfully unified this dependency
        if !unified_names.contains(&issue.dep_name) {
          issues.push(issue);
        }
      }
    }

    // 6. Detect transitive-only crates with fragmented feature sets
    let transitive_fragmentations = transitive::detect_transitive_fragmentation(self.metadata)?;

    Ok(UnificationPlan {
      workspace_deps,
      member_edits,
      issues,
      transitive_fragmentations,
      stats,
    })
  }

  /// Analyze a single dependency group
  ///
  /// Returns Ok(None) if the dependency should be skipped (excluded or doesn't meet strategy).
  /// Returns Ok(Some(result)) if analysis succeeded.
  /// Returns Err if analysis failed.
  fn analyze_dependency(
    &self,
    dep_name: String,
    mut instances: Vec<types::DependencyInstance>,
  ) -> RailResult<Option<DependencyAnalysisResult>> {
    // Check if should be excluded
    if self.config.exclude.contains(&dep_name) {
      return Ok(None);
    }

    // Check if meets strategy criteria
    if !strategy::should_unify(&dep_name, &instances, &self.config) {
      return Ok(None);
    }

    // RESOLUTION-BASED VERSION COMPATIBILITY
    // First, check if all instances resolve to the same version in Cargo's dependency graph.
    // This is the TRUTH - what Cargo actually resolved. If they all resolve to the same
    // version, they're compatible regardless of their declared requirements.
    //
    // Examples where this is superior:
    // - "1.0" and "^1.5" both resolve to 1.5.3 → compatible!
    // - "^1.2" and "^1.3" both resolve to 1.3.5 → compatible!
    //
    // Only if resolution checking fails do we fallback to syntactic merging.
    let versions: HashSet<_> = instances.iter().map(|i| &i.version_req).collect();
    if versions.len() > 1 {
      // Check resolution first (the superior approach)
      if let Some(resolved_version) = resolution::get_resolved_version(&dep_name, &instances, self.metadata) {
        // All instances resolve to the same version - they're compatible!
        // Normalize to a requirement that matches the resolved version
        let resolved_req = semver::VersionReq::parse(&format!("={}", resolved_version)).unwrap();
        for instance in &mut instances {
          instance.version_req = resolved_req.clone();
        }
      } else if let Some(merged) = self.try_merge_versions(&instances) {
        // Fallback: Syntactic merging (for when resolution isn't available)
        for instance in &mut instances {
          instance.version_req = merged.clone();
        }
      }
      // If both fail, detect_issues() will catch it as a version conflict
    }

    // Detect issues (will naturally pass if versions were successfully merged)
    let mut resolution_comment = None;
    let mut issues = Vec::new();

    if let Some(issue) = issue_detection::detect_issues(&dep_name, &instances, self.config.allow_renamed, self.metadata)
    {
      let mut resolved = false;

      // Attempt auto-resolution if enabled
      if self.config.auto_resolve_version_conflicts {
        match &issue.issue_type {
          types::IssueType::IncompatibleVersionRequirements { .. } => {
            // Auto-resolve: Pick the "highest" version requirement
            // Heuristic: Sort by string representation and pick the last one
            // This is a simple heuristic but effective for standard caret requirements
            let mut versions: Vec<_> = instances.iter().map(|i| i.version_req.clone()).collect();
            versions.sort_by_key(|a| a.to_string());

            if let Some(highest) = versions.last() {
              let highest_clone = highest.clone();
              // Apply to all instances
              for instance in &mut instances {
                instance.version_req = highest_clone.clone();
              }
              resolution_comment = Some(format!("Auto-resolved version conflict to {}", highest));
              resolved = true;
            }
          }
          types::IssueType::InconsistentDefaultFeatures { .. } => {
            // Auto-resolve: Enable default features (already done by unify_instances logic)
            // We just need to suppress the issue
            resolution_comment = Some("Auto-resolved inconsistent default-features (enabled)".to_string());
            resolved = true;
          }
          _ => {}
        }
      }

      if !resolved {
        // If auto-resolution is disabled, version conflicts should block the apply
        let mut issue = issue;
        if !self.config.auto_resolve_version_conflicts
          && matches!(
            issue.issue_type,
            types::IssueType::IncompatibleVersionRequirements { .. }
          )
        {
          issue.severity = types::IssueSeverity::Hard;
        }
        issues.push(issue);
        // Return early - this dependency has unresolved issues
        return Ok(Some(DependencyAnalysisResult {
          workspace_dep: None,
          member_edits: vec![],
          issues,
          compilations_saved: 0,
        }));
      }
    }

    // Compute unified dependency
    let mut unified = unifier::unify_instances(&dep_name, &instances, self.metadata)?;

    // Add resolution comment if any
    if let Some(comment) = resolution_comment {
      unified.comments.push(comment);
    }

    // Track fragmentation savings
    let compilations_saved = unified.fragmentation_count.saturating_sub(1);

    // Generate member edits
    let member_edits: Vec<_> = instances
      .iter()
      .map(|instance| {
        (
          instance.member.clone(),
          MemberEdit::UseWorkspace {
            dep_name: dep_name.clone(),
            kind: instance.kind,
            add_features: if instance.default_features && !unified.default_features {
              Some(vec!["default".to_string()])
            } else {
              None
            },
          },
        )
      })
      .collect();

    Ok(Some(DependencyAnalysisResult {
      workspace_dep: Some(unified),
      member_edits,
      issues,
      compilations_saved,
    }))
  }

  /// Try to merge multiple version requirements into a single compatible one
  fn try_merge_versions(&self, instances: &[types::DependencyInstance]) -> Option<semver::VersionReq> {
    if instances.is_empty() {
      return None;
    }

    // Collect unique version requirements
    let mut versions: Vec<&semver::VersionReq> = instances.iter().map(|i| &i.version_req).collect();
    versions.sort_by_key(|a| a.to_string());
    versions.dedup();

    if versions.is_empty() {
      return None;
    }

    if versions.len() == 1 {
      return Some(versions[0].clone());
    }

    // Try to merge all versions pairwise
    let mut merged = versions[0].clone();
    for v in &versions[1..] {
      match version_merge::try_merge_version_reqs(&merged, v) {
        Some(m) => merged = m,
        None => return None, // Cannot merge - incompatible
      }
    }

    Some(merged)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn create_test_metadata() -> WorkspaceMetadata {
    let current_dir = std::env::current_dir().unwrap();
    WorkspaceMetadata::load(&current_dir).unwrap()
  }

  #[test]
  fn test_analyze_workspace() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::with_config(&metadata, UnifyConfig::default());

    let plan = unifier.analyze();
    assert!(plan.is_ok(), "Analysis should succeed");

    let plan = plan.unwrap();
    assert!(plan.stats.total_deps > 0, "Should have analyzed dependencies");

    for dep in &plan.workspace_deps {
      assert!(!dep.name.is_empty(), "Dep name should not be empty");
      assert!(!dep.used_by.is_empty(), "Should have users");
      assert!(dep.used_by.len() >= 2, "Unified deps should have 2+ users");
    }
  }

  #[test]
  fn test_plan_summary_generation() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::with_config(&metadata, UnifyConfig::default());

    let plan = unifier.analyze().unwrap();
    let summary = plan.summary();

    assert!(summary.contains("Workspace Dependency Unification"));
    assert!(summary.contains("Statistics:"));

    if !plan.workspace_deps.is_empty() {
      assert!(summary.contains("Dependencies to unify:"));
    }
  }

  #[test]
  fn test_exclude_configuration() {
    let metadata = create_test_metadata();
    let mut config = UnifyConfig::default();
    config.exclude.insert("serde".to_string());

    let unifier = WorkspaceUnifier::with_config(&metadata, config);
    let plan = unifier.analyze().unwrap();

    assert!(
      !plan.workspace_deps.iter().any(|d| d.name == "serde"),
      "Excluded dep should not be unified"
    );
  }
}
