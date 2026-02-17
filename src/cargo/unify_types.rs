//! Core types for dependency unification
//!
//! This module contains all the data structures used by the unification system:
//! - `UnificationPlan` - the complete plan for unifying dependencies
//! - `UnifiedDep` - a dependency to add to workspace.dependencies
//! - `MemberEdit` - edits to apply to member Cargo.toml files
//! - Various issue and tracking types

use crate::cargo::manifest_analyzer::DepKind;
use crate::cargo::multi_target_metadata::ComputedMsrv;
use rustc_hash::FxHashMap;
use semver::VersionReq;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

// Core Types

/// A dep that will be unified in [workspace.dependencies]
///
/// Uses `Arc<str>` for identifier fields to avoid redundant allocations.
/// The same package/feature names are shared across multiple structures.
#[derive(Debug, Clone)]
pub struct UnifiedDep {
  /// Dependency package name
  pub name: Arc<str>,
  /// Version requirement (e.g., "^1.0.0")
  pub version_req: VersionReq,
  /// Features to enable (minimal intersection across all uses)
  pub features: Vec<Arc<str>>,
  /// Whether to enable default features
  pub default_features: bool,
  /// List of workspace members that use this dependency
  pub used_by: Vec<Arc<str>>,
  /// Target platform constraint (e.g., "cfg(unix)")
  pub target: Option<Arc<str>>,
  /// Local path for path dependencies
  pub path: Option<PathBuf>,
}

/// An edit to apply to a member's `Cargo.toml`
///
/// Uses `Arc<str>` for identifier fields to share allocations with source data.
#[derive(Debug, Clone)]
pub enum MemberEdit {
  /// Replace dep with workspace inheritance
  UseWorkspace {
    /// Name of the dependency to replace
    dep_name: Arc<str>,
    /// Type of dependency (normal, dev, build)
    dep_kind: DepKind,
    /// Target platform constraint (e.g., "cfg(unix)") - preserves which section to edit
    target: Option<Arc<str>>,
    /// Additional features to enable locally (beyond workspace features)
    local_features: Vec<Arc<str>>,
    /// Whether the dependency is optional
    is_optional: bool,
  },
  /// Remove an unused dep
  RemoveDep {
    /// Name of the dependency to remove
    dep_name: Arc<str>,
    /// Type of dependency (normal, dev, build)
    dep_kind: DepKind,
    /// Target platform constraint (if in target-specific section)
    target: Option<Arc<str>>,
  },
  /// Remove a dead feature (empty no-op)
  RemoveFeature {
    /// Name of the feature to remove
    feature_name: Arc<str>,
  },
  /// Add missing features to a dependency (fix undeclared feature borrowing)
  AddFeatures {
    /// Name of the dependency to update
    dep_name: Arc<str>,
    /// Type of dependency (normal, dev, build)
    dep_kind: DepKind,
    /// Target platform constraint (if in target-specific section)
    target: Option<Arc<str>>,
    /// Features to add to the dependency
    features_to_add: Vec<Arc<str>>,
  },
  /// Ensure this crate inherits MSRV from the workspace
  ///
  /// Sets `[package].rust-version = { workspace = true }`.
  EnforceMsrvInheritance,
}

/// Issue that prevents or warns about unification
#[derive(Debug, Clone)]
pub struct UnifyIssue {
  /// Classification of the issue for machine-readable diagnostics
  pub kind: UnifyIssueKind,
  /// Name of the dependency with the issue
  pub dep_name: Arc<str>,
  /// Whether this blocks unification or is just a warning
  pub severity: IssueSeverity,
  /// Description of the issue
  pub message: Arc<str>,
}

/// Category of unification issue
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnifyIssueKind {
  /// Generic issue with no special category
  General,
  /// A workspace-member cohort would be split across local and registry sources
  WorkspaceMemberCohortSplitRisk,
}

/// Severity level of a unification issue
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueSeverity {
  /// Blocks unification - must be resolved
  Error,
  /// Proceeds but with caution
  Warning,
}

/// Result of validation across targets
#[derive(Debug)]
pub struct ValidationResult {
  /// Target platform being validated
  pub target: Arc<str>,
  /// Whether validation passed
  pub success: bool,
  /// Error message if validation failed
  pub error: Option<Arc<str>>,
}

/// Record of a duplicate version that was cleaned up
#[derive(Debug, Clone)]
pub struct DuplicateCleanup {
  /// Dependency name
  pub dep_name: Arc<str>,
  /// Versions that were found across different targets
  pub versions_found: Vec<Arc<str>>,
  /// Version that was selected (always highest)
  pub selected_version: Arc<str>,
}

/// Record of a truly dead feature (empty no-op) that can be safely pruned
#[derive(Debug, Clone)]
pub struct PrunedFeature {
  /// Crate that declared the feature
  pub crate_name: Arc<str>,
  /// Feature name that was pruned
  pub feature_name: Arc<str>,
}

/// Record of an optional feature that's not currently enabled but enables something
/// These are user-facing API and should NOT be automatically removed
#[derive(Debug, Clone)]
pub struct OptionalFeature {
  /// Crate that declared the feature
  pub crate_name: Arc<str>,
  /// Feature name
  pub feature_name: Arc<str>,
  /// What this feature enables (for reporting)
  pub enables: Vec<Arc<str>>,
}

/// Record of a version mismatch between member manifest and workspace.dependencies
#[derive(Debug, Clone)]
pub struct VersionMismatch {
  /// Member that has the mismatched version
  pub member: Arc<str>,
  /// Dependency name
  pub dep_name: Arc<str>,
  /// Version declared in the member's Cargo.toml
  pub member_version: Arc<str>,
  /// Version in workspace.dependencies
  pub workspace_version: Arc<str>,
}

/// Record of an unused dependency (declared but not in resolved graph)
#[derive(Debug, Clone)]
pub struct UnusedDep {
  /// Member that has the unused dependency
  pub member: Arc<str>,
  /// Dependency name
  pub dep_name: Arc<str>,
  /// Kind of dependency (normal, dev, build)
  pub kind: DepKind,
  /// Why this dependency was flagged as unused
  pub reason: UnusedReason,
}

/// Reason a dependency was flagged as unused
#[derive(Debug, Clone)]
pub enum UnusedReason {
  /// Not found in resolved dependency graph for any configured target
  NotInResolvedGraph,
  /// Target-specific dep where the target IS configured but dep still not resolved
  /// (e.g., `[target.'cfg(windows)'.dependencies]` when windows target is configured)
  TargetConfiguredButNotResolved {
    /// The target cfg expression from the manifest
    target_cfg: Arc<str>,
  },
}

/// A transitive dependency to pin for workspace-hack replacement
#[derive(Debug, Clone)]
pub struct TransitivePin {
  /// Dependency name
  pub name: Arc<str>,
  /// Resolved version
  pub version: semver::Version,
  /// Features to enable
  pub features: Vec<Arc<str>>,
}

/// Record of a feature that is used at runtime but not declared in Cargo.toml
///
/// This happens when a crate relies on Cargo's feature unification to "borrow"
/// features from another crate in the workspace. After unification, standalone
/// builds of that crate will fail because the borrowed feature is no longer available.
#[derive(Debug, Clone)]
pub struct UndeclaredFeature {
  /// Member crate that uses the undeclared feature
  pub member: Arc<str>,
  /// Dependency name
  pub dep_name: Arc<str>,
  /// Features that are resolved (enabled at runtime) but not declared
  pub undeclared_features: Vec<Arc<str>>,
  /// Features that were declared in Cargo.toml
  pub declared_features: Vec<Arc<str>>,
  /// Features from the resolved dependency graph
  pub resolved_features: Vec<Arc<str>>,
  /// Kind of dependency (normal, dev, build)
  pub dep_kind: DepKind,
  /// Target platform constraint (if in target-specific section)
  pub target: Option<Arc<str>>,
  /// Which workspace members provide the borrowed features
  /// Shows where the feature is coming from (for transparency)
  pub borrowed_from: Vec<Arc<str>>,
}

// UnificationPlan

/// Complete unification plan
#[derive(Debug)]
pub struct UnificationPlan {
  /// Dependencies to add to [workspace.dependencies]
  pub workspace_deps: Vec<UnifiedDep>,
  /// Edits to apply to each member's Cargo.toml
  pub member_edits: FxHashMap<Arc<str>, Vec<MemberEdit>>,
  /// Mapping from package name to manifest path (relative to workspace root)
  pub member_paths: FxHashMap<Arc<str>, PathBuf>,
  /// Transitive dependencies to pin (with version info)
  pub transitive_pins: Vec<TransitivePin>,
  /// Results from validating across target platforms
  pub validation_results: Vec<ValidationResult>,
  /// Issues detected during analysis
  pub issues: Vec<UnifyIssue>,
  /// Computed MSRV from dependency graph (if msrv = true in config)
  pub computed_msrv: Option<ComputedMsrv>,
  /// Duplicate versions that were silently cleaned up
  pub duplicates_cleaned: Vec<DuplicateCleanup>,
  /// Truly dead features (empty no-ops) that can be safely pruned
  pub pruned_features: Vec<PrunedFeature>,
  /// Optional features that are not enabled but provide user-facing functionality
  /// These should NOT be removed, only reported for awareness
  pub optional_features: Vec<OptionalFeature>,
  /// Version mismatches between member manifests and existing workspace.dependencies
  pub version_mismatches: Vec<VersionMismatch>,
  /// Unused dependencies detected in workspace members
  pub unused_deps: Vec<UnusedDep>,
  /// Undeclared features detected (resolved > declared)
  /// These indicate crates relying on feature unification from other workspace members
  pub undeclared_features: Vec<UndeclaredFeature>,
}

impl UnificationPlan {
  /// Returns true if there are any errors that block unification
  pub fn has_blocking_issues(&self) -> bool {
    self.issues.iter().any(|i| i.severity == IssueSeverity::Error)
  }

  /// Total number of concrete member edits planned across the workspace.
  ///
  /// Note: `member_edits` may contain empty vectors in edge cases; this returns the
  /// actual edit count and is the right basis for "would this mutate files?" checks.
  pub fn member_edit_count(&self) -> usize {
    self.member_edits.values().map(|v| v.len()).sum()
  }

  /// Returns true if the plan would mutate manifests or workspace MSRV.
  ///
  /// Intentionally ignores informational-only fields like `optional_features`.
  pub fn has_planned_changes(&self, msrv_write_needed: bool) -> bool {
    !self.workspace_deps.is_empty()
      || self.member_edit_count() > 0
      || !self.transitive_pins.is_empty()
      || msrv_write_needed
  }

  /// Generates a human-readable summary of the plan
  pub fn summary(&self) -> String {
    let mut s = String::new();
    s.push_str("=== Unification Plan ===\n\n");

    // Show dependencies to unify (new workspace deps)
    if !self.workspace_deps.is_empty() {
      s.push_str(&format!("Dependencies to unify: {}\n", self.workspace_deps.len()));
      for dep in &self.workspace_deps {
        s.push_str(&format!("  - {} = \"{}\"", dep.name, dep.version_req));

        if !dep.features.is_empty() {
          let feats: Vec<&str> = dep.features.iter().map(|f| &**f).collect();
          s.push_str(&format!(", features = [{}]", feats.join(", ")));
        }

        if !dep.default_features {
          s.push_str(", default-features = false");
        }

        s.push_str(&format!(" (used by {} crates)\n", dep.used_by.len()));
      }
      s.push('\n');
    }

    // Show member edits (deps being converted to workspace = true)
    if !self.member_edits.is_empty() {
      // Collect unique dep names being converted, removed deps, removed features, and added features
      let mut converted_deps: HashSet<Arc<str>> = HashSet::new();
      let mut removed_deps: HashSet<Arc<str>> = HashSet::new();
      let mut removed_features: HashSet<Arc<str>> = HashSet::new();
      let mut features_added_count = 0usize;
      let mut features_added_crates: HashSet<Arc<str>> = HashSet::new();
      let mut msrv_inheritance_crates: HashSet<Arc<str>> = HashSet::new();
      for (member_name, edits) in &self.member_edits {
        for edit in edits {
          match edit {
            MemberEdit::UseWorkspace { dep_name, .. } => {
              converted_deps.insert(Arc::clone(dep_name));
            }
            MemberEdit::RemoveDep { dep_name, .. } => {
              removed_deps.insert(Arc::clone(dep_name));
            }
            MemberEdit::RemoveFeature { feature_name } => {
              removed_features.insert(Arc::clone(feature_name));
            }
            MemberEdit::AddFeatures { features_to_add, .. } => {
              features_added_count += features_to_add.len();
              features_added_crates.insert(Arc::clone(member_name));
            }
            MemberEdit::EnforceMsrvInheritance => {
              msrv_inheritance_crates.insert(Arc::clone(member_name));
            }
          }
        }
      }

      // Only show if there are deps being converted that aren't already in workspace_deps
      let workspace_dep_names: HashSet<Arc<str>> = self.workspace_deps.iter().map(|d| Arc::clone(&d.name)).collect();
      let conversion_only: Vec<_> = converted_deps.difference(&workspace_dep_names).collect();

      if !conversion_only.is_empty() {
        s.push_str(&format!(
          "Dependencies to convert to workspace inheritance: {}\n",
          conversion_only.len()
        ));
        for dep_name in conversion_only {
          s.push_str(&format!("  - {} (already in workspace.dependencies)\n", dep_name));
        }
        s.push('\n');
      }

      // Show deps being removed
      if !removed_deps.is_empty() {
        s.push_str(&format!("Dependencies to remove (unused): {}\n", removed_deps.len()));
        for dep_name in &removed_deps {
          s.push_str(&format!("  - {}\n", dep_name));
        }
        s.push('\n');
      }

      // Show features being added (undeclared feature fixes)
      if features_added_count > 0 {
        s.push_str(&format!(
          "Undeclared features to fix: {} features across {} crates\n",
          features_added_count,
          features_added_crates.len()
        ));
        s.push('\n');
      }

      if !msrv_inheritance_crates.is_empty() {
        s.push_str(&format!(
          "MSRV inheritance to enforce: {} crates\n",
          msrv_inheritance_crates.len()
        ));
        s.push('\n');
      }
    }

    s.push_str(&format!("Member edits: {}\n", self.member_edits.len()));
    s.push_str(&format!("Transitive pins: {}\n", self.transitive_pins.len()));

    if !self.issues.is_empty() {
      s.push_str(&format!("\nIssues requiring attention: {}\n", self.issues.len()));
      for issue in &self.issues {
        let kind_suffix = if issue.kind == UnifyIssueKind::General {
          ""
        } else {
          " [WorkspaceMemberCohortSplitRisk]"
        };
        s.push_str(&format!(
          "  - [{}]{} {}: {}\n",
          if issue.severity == IssueSeverity::Error {
            "ERROR"
          } else {
            "WARN"
          },
          kind_suffix,
          issue.dep_name,
          issue.message
        ));
      }
    }

    if !self.validation_results.is_empty() {
      let failed = self.validation_results.iter().filter(|v| !v.success).count();
      if failed > 0 {
        s.push_str(&format!("\n  {} target validations failed\n", failed));
      }
    }

    // Show computed MSRV if available
    if let Some(ref msrv) = self.computed_msrv {
      s.push_str(&format!(
        "\nComputed MSRV: {} (from {} deps with rust-version)\n",
        msrv.version, msrv.deps_with_msrv
      ));
      if let Some(ref warning) = msrv.warning {
        s.push_str(&format!("  Warning: {}\n", warning));
      }
      if !msrv.contributors.is_empty() {
        let contributors_str = if msrv.contributors.len() > 3 {
          format!(
            "{}, ... ({} total)",
            msrv.contributors[..3].join(", "),
            msrv.contributors.len()
          )
        } else {
          msrv.contributors.join(", ")
        };
        s.push_str(&format!("  Contributors: {}\n", contributors_str));
      }
    }

    // Show duplicates that were cleaned up
    if !self.duplicates_cleaned.is_empty() {
      s.push_str(&format!(
        "\nDuplicate versions unified: {}\n",
        self.duplicates_cleaned.len()
      ));
      for dup in &self.duplicates_cleaned {
        s.push_str(&format!(
          "  - {} -> {} (was: {})\n",
          dup.dep_name,
          dup.selected_version,
          dup.versions_found.join(", ")
        ));
      }
    }

    // Show pruned dead features (empty no-ops)
    if !self.pruned_features.is_empty() {
      s.push_str(&format!(
        "\nDead features (empty no-ops, safe to prune): {}\n",
        self.pruned_features.len()
      ));
      // Group by crate (BTreeMap for deterministic output order)
      let mut by_crate: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
      for pf in &self.pruned_features {
        by_crate.entry(&pf.crate_name).or_default().push(&pf.feature_name);
      }
      for (crate_name, mut features) in by_crate {
        features.sort_unstable();
        s.push_str(&format!("  - {}: {}\n", crate_name, features.join(", ")));
      }
    }

    // Show optional features (user-facing API, NOT removed)
    if !self.optional_features.is_empty() {
      s.push_str(&format!(
        "\nOptional features (user-facing API, NOT removed): {}\n",
        self.optional_features.len()
      ));
      // Group by crate (BTreeMap for deterministic output order)
      let mut by_crate: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
      for of in &self.optional_features {
        by_crate.entry(&of.crate_name).or_default().push(&of.feature_name);
      }
      for (crate_name, mut features) in by_crate {
        features.sort_unstable();
        s.push_str(&format!("  - {}: {}\n", crate_name, features.join(", ")));
      }
    }

    // Show version mismatches
    if !self.version_mismatches.is_empty() {
      s.push_str(&format!(
        "\n  Version mismatches with workspace.dependencies: {}\n",
        self.version_mismatches.len()
      ));
      for mismatch in &self.version_mismatches {
        s.push_str(&format!(
          "  - {} in {}: declares \"{}\" but workspace has \"{}\"\n",
          mismatch.dep_name, mismatch.member, mismatch.member_version, mismatch.workspace_version
        ));
      }
    }

    // Show unused dependencies
    if !self.unused_deps.is_empty() {
      s.push_str(&format!(
        "\n  Unused dependencies detected: {}\n",
        self.unused_deps.len()
      ));
      // Group by member (BTreeMap for deterministic output order)
      let mut by_member: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
      for ud in &self.unused_deps {
        by_member.entry(&ud.member).or_default().push(&ud.dep_name);
      }
      for (member, mut deps) in by_member {
        deps.sort_unstable();
        s.push_str(&format!("  - {}: {}\n", member, deps.join(", ")));
      }
    }

    // Show undeclared features - only as warnings if NOT being auto-fixed
    // Check if we have AddFeatures edits (which means fix_undeclared_features is enabled)
    let has_feature_fixes = self
      .member_edits
      .values()
      .any(|edits| edits.iter().any(|e| matches!(e, MemberEdit::AddFeatures { .. })));

    if !self.undeclared_features.is_empty() && !has_feature_fixes {
      // Warn mode: show all undeclared features as warnings
      s.push_str(&format!(
        "\n⚠️  Undeclared features detected (will break standalone builds): {}\n",
        self.undeclared_features.len()
      ));
      for uf in &self.undeclared_features {
        let borrowed_from_str = if uf.borrowed_from.is_empty() {
          String::new()
        } else {
          format!(" (borrowed from {})", uf.borrowed_from.join(", "))
        };
        s.push_str(&format!(
          "  - {}/{}: [{}]{}\n",
          uf.member,
          uf.dep_name,
          uf.undeclared_features.join(", "),
          borrowed_from_str
        ));
      }
      s.push_str("  These crates rely on feature unification from other workspace members.\n");
      s.push_str("  After unification, standalone builds (cargo test -p <crate>) will fail.\n");
      s.push_str("  Fix: Set fix_undeclared_features = true in rail.toml to auto-fix.\n");
    }

    s
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Helper to create Arc<str> from string literal
  fn arc(s: &str) -> Arc<str> {
    Arc::from(s)
  }

  #[test]
  fn test_member_edit_add_features_struct() {
    let edit = MemberEdit::AddFeatures {
      dep_name: arc("serde"),
      dep_kind: DepKind::Normal,
      target: None,
      features_to_add: vec![arc("derive"), arc("std")],
    };

    match edit {
      MemberEdit::AddFeatures {
        dep_name,
        dep_kind,
        target,
        features_to_add,
      } => {
        assert_eq!(&*dep_name, "serde");
        assert_eq!(dep_kind, DepKind::Normal);
        assert!(target.is_none());
        assert_eq!(features_to_add.len(), 2);
        assert!(features_to_add.iter().any(|f| &**f == "derive"));
      }
      _ => panic!("Expected AddFeatures variant"),
    }
  }

  #[test]
  fn test_member_edit_add_features_with_target() {
    let edit = MemberEdit::AddFeatures {
      dep_name: arc("libc"),
      dep_kind: DepKind::Normal,
      target: Some(arc("cfg(unix)")),
      features_to_add: vec![arc("extra_traits")],
    };

    match edit {
      MemberEdit::AddFeatures { target, .. } => {
        assert_eq!(target.as_deref(), Some("cfg(unix)"));
      }
      _ => panic!("Expected AddFeatures variant"),
    }
  }

  #[test]
  fn test_member_edit_add_features_dev_dependency() {
    let edit = MemberEdit::AddFeatures {
      dep_name: arc("tokio"),
      dep_kind: DepKind::Dev,
      target: None,
      features_to_add: vec![arc("rt-multi-thread"), arc("macros")],
    };

    match edit {
      MemberEdit::AddFeatures { dep_kind, .. } => {
        assert_eq!(dep_kind, DepKind::Dev);
      }
      _ => panic!("Expected AddFeatures variant"),
    }
  }

  #[test]
  fn test_member_edit_add_features_build_dependency() {
    let edit = MemberEdit::AddFeatures {
      dep_name: arc("cc"),
      dep_kind: DepKind::Build,
      target: None,
      features_to_add: vec![arc("parallel")],
    };

    match edit {
      MemberEdit::AddFeatures { dep_kind, .. } => {
        assert_eq!(dep_kind, DepKind::Build);
      }
      _ => panic!("Expected AddFeatures variant"),
    }
  }

  // UndeclaredFeature Tests

  #[test]
  fn test_undeclared_feature_struct() {
    let uf = UndeclaredFeature {
      member: arc("my-crate"),
      dep_name: arc("backoff"),
      undeclared_features: vec![arc("futures"), arc("tokio")],
      declared_features: vec![arc("default")],
      resolved_features: vec![arc("default"), arc("futures"), arc("tokio")],
      dep_kind: DepKind::Normal,
      target: None,
      borrowed_from: vec![arc("other-crate")],
    };

    assert_eq!(&*uf.member, "my-crate");
    assert_eq!(&*uf.dep_name, "backoff");
    assert_eq!(uf.undeclared_features.len(), 2);
    assert_eq!(uf.declared_features.len(), 1);
    assert_eq!(uf.resolved_features.len(), 3);
    assert_eq!(uf.dep_kind, DepKind::Normal);
    assert!(uf.target.is_none());
    assert_eq!(uf.borrowed_from.len(), 1);
  }

  #[test]
  fn test_undeclared_feature_with_target() {
    let uf = UndeclaredFeature {
      member: arc("platform-crate"),
      dep_name: arc("libc"),
      undeclared_features: vec![arc("extra_traits")],
      declared_features: vec![],
      resolved_features: vec![arc("extra_traits")],
      dep_kind: DepKind::Normal,
      target: Some(arc("cfg(unix)")),
      borrowed_from: vec![arc("unix-crate")],
    };

    assert_eq!(uf.target.as_deref(), Some("cfg(unix)"));
  }

  #[test]
  fn test_undeclared_feature_dev_dependency() {
    let uf = UndeclaredFeature {
      member: arc("test-crate"),
      dep_name: arc("tokio"),
      undeclared_features: vec![arc("macros")],
      declared_features: vec![arc("rt")],
      resolved_features: vec![arc("rt"), arc("macros")],
      dep_kind: DepKind::Dev,
      target: None,
      borrowed_from: vec![arc("main-crate")],
    };

    assert_eq!(uf.dep_kind, DepKind::Dev);
  }

  // UnificationPlan Summary Tests

  #[test]
  fn test_summary_with_add_features_edit() {
    let mut plan = UnificationPlan {
      workspace_deps: vec![],
      member_edits: FxHashMap::default(),
      member_paths: FxHashMap::default(),
      transitive_pins: vec![],
      validation_results: vec![],
      issues: vec![],
      computed_msrv: None,
      duplicates_cleaned: vec![],
      pruned_features: vec![],
      optional_features: vec![],
      version_mismatches: vec![],
      unused_deps: vec![],
      undeclared_features: vec![],
    };

    // Add a MemberEdit::AddFeatures
    plan.member_edits.insert(
      arc("test-crate"),
      vec![MemberEdit::AddFeatures {
        dep_name: arc("serde"),
        dep_kind: DepKind::Normal,
        target: None,
        features_to_add: vec![arc("derive")],
      }],
    );

    let summary = plan.summary();
    assert!(summary.contains("Undeclared features to fix: 1 features across 1 crates"));
  }

  #[test]
  fn test_summary_with_multiple_add_features_edits() {
    let mut plan = UnificationPlan {
      workspace_deps: vec![],
      member_edits: FxHashMap::default(),
      member_paths: FxHashMap::default(),
      transitive_pins: vec![],
      validation_results: vec![],
      issues: vec![],
      computed_msrv: None,
      duplicates_cleaned: vec![],
      pruned_features: vec![],
      optional_features: vec![],
      version_mismatches: vec![],
      unused_deps: vec![],
      undeclared_features: vec![],
    };

    // Add multiple AddFeatures edits
    plan.member_edits.insert(
      arc("crate-a"),
      vec![
        MemberEdit::AddFeatures {
          dep_name: arc("serde"),
          dep_kind: DepKind::Normal,
          target: None,
          features_to_add: vec![arc("derive"), arc("std")],
        },
        MemberEdit::AddFeatures {
          dep_name: arc("tokio"),
          dep_kind: DepKind::Normal,
          target: None,
          features_to_add: vec![arc("rt")],
        },
      ],
    );
    plan.member_edits.insert(
      arc("crate-b"),
      vec![MemberEdit::AddFeatures {
        dep_name: arc("backoff"),
        dep_kind: DepKind::Normal,
        target: None,
        features_to_add: vec![arc("futures")],
      }],
    );

    let summary = plan.summary();
    // 2 + 1 + 1 = 4 features across 2 crates
    assert!(summary.contains("Undeclared features to fix: 4 features across 2 crates"));
  }

  #[test]
  fn test_summary_undeclared_warning_when_no_fixes() {
    let plan = UnificationPlan {
      workspace_deps: vec![],
      member_edits: FxHashMap::default(),
      member_paths: FxHashMap::default(),
      transitive_pins: vec![],
      validation_results: vec![],
      issues: vec![],
      computed_msrv: None,
      duplicates_cleaned: vec![],
      pruned_features: vec![],
      optional_features: vec![],
      version_mismatches: vec![],
      unused_deps: vec![],
      undeclared_features: vec![UndeclaredFeature {
        member: arc("my-crate"),
        dep_name: arc("backoff"),
        undeclared_features: vec![arc("futures")],
        declared_features: vec![],
        resolved_features: vec![arc("futures")],
        dep_kind: DepKind::Normal,
        target: None,
        borrowed_from: vec![arc("other-crate")],
      }],
    };

    // No AddFeatures edits = warn mode
    let summary = plan.summary();
    assert!(summary.contains("⚠️  Undeclared features detected"));
    assert!(summary.contains("fix_undeclared_features = true"));
  }

  #[test]
  fn test_summary_no_undeclared_warning_when_fixes_present() {
    let mut plan = UnificationPlan {
      workspace_deps: vec![],
      member_edits: FxHashMap::default(),
      member_paths: FxHashMap::default(),
      transitive_pins: vec![],
      validation_results: vec![],
      issues: vec![],
      computed_msrv: None,
      duplicates_cleaned: vec![],
      pruned_features: vec![],
      optional_features: vec![],
      version_mismatches: vec![],
      unused_deps: vec![],
      undeclared_features: vec![UndeclaredFeature {
        member: arc("my-crate"),
        dep_name: arc("backoff"),
        undeclared_features: vec![arc("futures")],
        declared_features: vec![],
        resolved_features: vec![arc("futures")],
        dep_kind: DepKind::Normal,
        target: None,
        borrowed_from: vec![arc("other-crate")],
      }],
    };

    // Add a fix
    plan.member_edits.insert(
      arc("my-crate"),
      vec![MemberEdit::AddFeatures {
        dep_name: arc("backoff"),
        dep_kind: DepKind::Normal,
        target: None,
        features_to_add: vec![arc("futures")],
      }],
    );

    let summary = plan.summary();
    // Should NOT show the warning since we have fixes
    assert!(!summary.contains("⚠️  Undeclared features detected"));
    // Should show the fix count
    assert!(summary.contains("Undeclared features to fix: 1 features across 1 crates"));
  }

  #[test]
  fn test_member_edit_clone() {
    let edit = MemberEdit::AddFeatures {
      dep_name: arc("test"),
      dep_kind: DepKind::Normal,
      target: Some(arc("cfg(test)")),
      features_to_add: vec![arc("a"), arc("b")],
    };

    let cloned = edit.clone();
    match (edit, cloned) {
      (
        MemberEdit::AddFeatures {
          dep_name: a,
          features_to_add: fa,
          ..
        },
        MemberEdit::AddFeatures {
          dep_name: b,
          features_to_add: fb,
          ..
        },
      ) => {
        assert_eq!(a, b);
        assert_eq!(fa, fb);
      }
      _ => panic!("Expected AddFeatures"),
    }
  }

  #[test]
  fn test_undeclared_feature_clone() {
    let uf = UndeclaredFeature {
      member: arc("test"),
      dep_name: arc("dep"),
      undeclared_features: vec![arc("f1")],
      declared_features: vec![arc("f2")],
      resolved_features: vec![arc("f1"), arc("f2")],
      dep_kind: DepKind::Dev,
      target: Some(arc("cfg(windows)")),
      borrowed_from: vec![arc("source-crate")],
    };

    let cloned = uf.clone();
    assert_eq!(uf.member, cloned.member);
    assert_eq!(uf.dep_name, cloned.dep_name);
    assert_eq!(uf.undeclared_features, cloned.undeclared_features);
    assert_eq!(uf.target, cloned.target);
    assert_eq!(uf.borrowed_from, cloned.borrowed_from);
  }

  #[test]
  fn test_summary_shows_borrowed_from_in_undeclared_warning() {
    let plan = UnificationPlan {
      workspace_deps: vec![],
      member_edits: FxHashMap::default(),
      member_paths: FxHashMap::default(),
      transitive_pins: vec![],
      validation_results: vec![],
      issues: vec![],
      computed_msrv: None,
      duplicates_cleaned: vec![],
      pruned_features: vec![],
      optional_features: vec![],
      version_mismatches: vec![],
      unused_deps: vec![],
      undeclared_features: vec![UndeclaredFeature {
        member: arc("my-crate"),
        dep_name: arc("tokio"),
        undeclared_features: vec![arc("macros"), arc("rt")],
        declared_features: vec![],
        resolved_features: vec![arc("macros"), arc("rt")],
        dep_kind: DepKind::Normal,
        target: None,
        borrowed_from: vec![arc("other-crate"), arc("third-crate")],
      }],
    };

    // No AddFeatures edits = warn mode with borrowed_from
    let summary = plan.summary();
    assert!(summary.contains("⚠️  Undeclared features detected"));
    assert!(summary.contains("my-crate/tokio"));
    assert!(summary.contains("macros"));
    assert!(summary.contains("rt"));
    assert!(summary.contains("(borrowed from other-crate, third-crate)"));
  }

  #[test]
  fn test_summary_borrowed_from_empty_not_shown() {
    let plan = UnificationPlan {
      workspace_deps: vec![],
      member_edits: FxHashMap::default(),
      member_paths: FxHashMap::default(),
      transitive_pins: vec![],
      validation_results: vec![],
      issues: vec![],
      computed_msrv: None,
      duplicates_cleaned: vec![],
      pruned_features: vec![],
      optional_features: vec![],
      version_mismatches: vec![],
      unused_deps: vec![],
      undeclared_features: vec![UndeclaredFeature {
        member: arc("my-crate"),
        dep_name: arc("backoff"),
        undeclared_features: vec![arc("futures")],
        declared_features: vec![],
        resolved_features: vec![arc("futures")],
        dep_kind: DepKind::Normal,
        target: None,
        borrowed_from: vec![], // Empty - shouldn't show "borrowed from"
      }],
    };

    let summary = plan.summary();
    assert!(summary.contains("my-crate/backoff"));
    assert!(summary.contains("futures"));
    // Should NOT contain "borrowed from" when empty
    assert!(!summary.contains("borrowed from"));
  }

  #[test]
  fn test_member_edit_count_ignores_empty_entries() {
    let mut member_edits: FxHashMap<Arc<str>, Vec<MemberEdit>> = FxHashMap::default();
    member_edits.insert(arc("crate-a"), vec![]);

    let plan = UnificationPlan {
      workspace_deps: vec![],
      member_edits,
      member_paths: FxHashMap::default(),
      transitive_pins: vec![],
      validation_results: vec![],
      issues: vec![],
      computed_msrv: None,
      duplicates_cleaned: vec![],
      pruned_features: vec![],
      optional_features: vec![OptionalFeature {
        crate_name: arc("crate-a"),
        feature_name: arc("serde"),
        enables: vec![arc("serde/derive")],
      }],
      version_mismatches: vec![],
      unused_deps: vec![],
      undeclared_features: vec![],
    };

    assert_eq!(plan.member_edit_count(), 0);
    assert!(!plan.has_planned_changes(false));
  }
}
