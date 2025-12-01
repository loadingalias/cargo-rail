//! Core types for dependency unification
//!
//! This module contains all the data structures used by the unification system:
//! - `UnificationPlan` - the complete plan for unifying dependencies
//! - `UnifiedDep` - a dependency to add to workspace.dependencies
//! - `MemberEdit` - edits to apply to member Cargo.toml files
//! - Various issue and tracking types

use crate::cargo::manifest_analyzer::DepKind;
use crate::cargo::multi_target_metadata::ComputedMsrv;
use semver::VersionReq;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

// ============================================================================
// Core Types
// ============================================================================

/// A dependency that will be unified in [workspace.dependencies]
#[derive(Debug, Clone)]
pub struct UnifiedDep {
  /// Dependency package name
  pub name: String,
  /// Version requirement (e.g., "^1.0.0")
  pub version_req: VersionReq,
  /// Features to enable (minimal intersection across all uses)
  pub features: Vec<String>,
  /// Whether to enable default features
  pub default_features: bool,
  /// List of workspace members that use this dependency
  pub used_by: Vec<String>,
  /// Target platform constraint (e.g., "cfg(unix)")
  pub target: Option<String>,
  /// Local path for path dependencies
  pub path: Option<PathBuf>,
}

/// An edit to apply to a member's Cargo.toml
#[derive(Debug, Clone)]
pub enum MemberEdit {
  /// Replace dependency with workspace inheritance
  UseWorkspace {
    /// Name of the dependency to replace
    dep_name: String,
    /// Type of dependency (normal, dev, build)
    dep_kind: DepKind,
    /// Target platform constraint (e.g., "cfg(unix)") - preserves which section to edit
    target: Option<String>,
    /// Additional features to enable locally (beyond workspace features)
    local_features: Vec<String>,
    /// Whether the dependency is optional
    is_optional: bool,
  },
  /// Remove an unused dependency
  RemoveDep {
    /// Name of the dependency to remove
    dep_name: String,
    /// Type of dependency (normal, dev, build)
    dep_kind: DepKind,
    /// Target platform constraint (if in target-specific section)
    target: Option<String>,
  },
  /// Remove a dead feature (empty no-op)
  RemoveFeature {
    /// Name of the feature to remove
    feature_name: String,
  },
}

/// Issue that prevents or warns about unification
#[derive(Debug, Clone)]
pub struct UnifyIssue {
  /// Name of the dependency with the issue
  pub dep_name: String,
  /// Whether this blocks unification or is just a warning
  pub severity: IssueSeverity,
  /// Description of the issue
  pub message: String,
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
  pub target: String,
  /// Whether validation passed
  pub success: bool,
  /// Error message if validation failed
  pub error: Option<String>,
}

/// Record of a duplicate version that was cleaned up
#[derive(Debug, Clone)]
pub struct DuplicateCleanup {
  /// Dependency name
  pub dep_name: String,
  /// Versions that were found across different targets
  pub versions_found: Vec<String>,
  /// Version that was selected (always highest)
  pub selected_version: String,
}

/// Record of a truly dead feature (empty no-op) that can be safely pruned
#[derive(Debug, Clone)]
pub struct PrunedFeature {
  /// Crate that declared the feature
  pub crate_name: String,
  /// Feature name that was pruned
  pub feature_name: String,
}

/// Record of an optional feature that's not currently enabled but enables something
/// These are user-facing API and should NOT be automatically removed
#[derive(Debug, Clone)]
pub struct OptionalFeature {
  /// Crate that declared the feature
  pub crate_name: String,
  /// Feature name
  pub feature_name: String,
  /// What this feature enables (for reporting)
  pub enables: Vec<String>,
}

/// Record of a version mismatch between member manifest and workspace.dependencies
#[derive(Debug, Clone)]
pub struct VersionMismatch {
  /// Member that has the mismatched version
  pub member: String,
  /// Dependency name
  pub dep_name: String,
  /// Version declared in the member's Cargo.toml
  pub member_version: String,
  /// Version in workspace.dependencies
  pub workspace_version: String,
}

/// Record of an unused dependency (declared but not in resolved graph)
#[derive(Debug, Clone)]
pub struct UnusedDep {
  /// Member that has the unused dependency
  pub member: String,
  /// Dependency name
  pub dep_name: String,
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
    target_cfg: String,
  },
}

/// A transitive dependency to pin for workspace-hack replacement
#[derive(Debug, Clone)]
pub struct TransitivePin {
  /// Dependency name
  pub name: String,
  /// Resolved version
  pub version: semver::Version,
  /// Features to enable
  pub features: Vec<String>,
}

// ============================================================================
// UnificationPlan
// ============================================================================

/// Complete unification plan
#[derive(Debug)]
pub struct UnificationPlan {
  /// Dependencies to add to [workspace.dependencies]
  pub workspace_deps: Vec<UnifiedDep>,
  /// Edits to apply to each member's Cargo.toml
  pub member_edits: HashMap<String, Vec<MemberEdit>>,
  /// Mapping from package name to manifest path (relative to workspace root)
  pub member_paths: HashMap<String, PathBuf>,
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
}

impl UnificationPlan {
  /// Returns true if there are any errors that block unification
  pub fn has_blocking_issues(&self) -> bool {
    self.issues.iter().any(|i| i.severity == IssueSeverity::Error)
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
          s.push_str(&format!(", features = [{}]", dep.features.join(", ")));
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
      // Collect unique dep names being converted, removed deps, and removed features
      let mut converted_deps: HashSet<String> = HashSet::new();
      let mut removed_deps: HashSet<String> = HashSet::new();
      let mut removed_features: HashSet<String> = HashSet::new();
      for edits in self.member_edits.values() {
        for edit in edits {
          match edit {
            MemberEdit::UseWorkspace { dep_name, .. } => {
              converted_deps.insert(dep_name.clone());
            }
            MemberEdit::RemoveDep { dep_name, .. } => {
              removed_deps.insert(dep_name.clone());
            }
            MemberEdit::RemoveFeature { feature_name } => {
              removed_features.insert(feature_name.clone());
            }
          }
        }
      }

      // Only show if there are deps being converted that aren't already in workspace_deps
      let workspace_dep_names: HashSet<_> = self.workspace_deps.iter().map(|d| d.name.clone()).collect();
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
    }

    s.push_str(&format!("Member edits: {}\n", self.member_edits.len()));
    s.push_str(&format!("Transitive pins: {}\n", self.transitive_pins.len()));

    if !self.issues.is_empty() {
      s.push_str(&format!("\nIssues requiring attention: {}\n", self.issues.len()));
      for issue in &self.issues {
        s.push_str(&format!(
          "  - [{}] {}: {}\n",
          if issue.severity == IssueSeverity::Error {
            "ERROR"
          } else {
            "WARN"
          },
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
      // Group by crate
      let mut by_crate: HashMap<&str, Vec<&str>> = HashMap::new();
      for pf in &self.pruned_features {
        by_crate.entry(&pf.crate_name).or_default().push(&pf.feature_name);
      }
      for (crate_name, features) in by_crate {
        s.push_str(&format!("  - {}: {}\n", crate_name, features.join(", ")));
      }
    }

    // Show optional features (user-facing API, NOT removed)
    if !self.optional_features.is_empty() {
      s.push_str(&format!(
        "\nOptional features (user-facing API, NOT removed): {}\n",
        self.optional_features.len()
      ));
      // Group by crate
      let mut by_crate: HashMap<&str, Vec<&str>> = HashMap::new();
      for of in &self.optional_features {
        by_crate.entry(&of.crate_name).or_default().push(&of.feature_name);
      }
      for (crate_name, features) in by_crate {
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
      // Group by member
      let mut by_member: HashMap<&str, Vec<&str>> = HashMap::new();
      for ud in &self.unused_deps {
        by_member.entry(&ud.member).or_default().push(&ud.dep_name);
      }
      for (member, deps) in by_member {
        s.push_str(&format!("  - {}: {}\n", member, deps.join(", ")));
      }
    }

    s
  }
}
