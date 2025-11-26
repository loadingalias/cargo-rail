//! Clean unification analyzer using the HYBRID approach
//!
//! This is the core of the new unify implementation that properly integrates
//! with WorkspaceContext and uses multi-target metadata + manifest analysis.

use crate::cargo::{
  feature_scanner::FeatureScanner,
  manifest_analyzer::{DepKind, ExistingWorkspaceDep, ManifestAnalyzer, parse_existing_workspace_deps},
  multi_target_metadata::{ComputedMsrv, MultiTargetMetadata},
};
use crate::config::{ExactPinHandling, UnifyConfig};
use crate::error::RailResult;
use crate::workspace::WorkspaceContext;
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

/// Record of a feature that was pruned because it's not used in source code
#[derive(Debug, Clone)]
pub struct PrunedFeature {
  /// Crate that declared the feature
  pub crate_name: String,
  /// Feature name that was pruned
  pub feature_name: String,
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
}

/// Complete unification plan
#[derive(Debug)]
pub struct UnificationPlan {
  /// Dependencies to add to [workspace.dependencies]
  pub workspace_deps: Vec<UnifiedDep>,
  /// Edits to apply to each member's Cargo.toml
  pub member_edits: HashMap<String, Vec<MemberEdit>>,
  /// Mapping from package name to manifest path (relative to workspace root)
  pub member_paths: HashMap<String, PathBuf>,
  /// Transitive dependencies to pin (dep_name, features)
  pub transitive_pins: Vec<(String, Vec<String>)>,
  /// Results from validating across target platforms
  pub validation_results: Vec<ValidationResult>,
  /// Issues detected during analysis
  pub issues: Vec<UnifyIssue>,
  /// Computed MSRV from dependency graph (if msrv = true in config)
  pub computed_msrv: Option<ComputedMsrv>,
  /// Duplicate versions that were silently cleaned up
  pub duplicates_cleaned: Vec<DuplicateCleanup>,
  /// Features that were pruned because they're not used in source code
  pub pruned_features: Vec<PrunedFeature>,
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
      // Collect unique dep names being converted
      let mut converted_deps: HashSet<String> = HashSet::new();
      for edits in self.member_edits.values() {
        for edit in edits {
          match edit {
            MemberEdit::UseWorkspace { dep_name, .. } => {
              converted_deps.insert(dep_name.clone());
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
        s.push_str(&format!("\n⚠️  {} target validations failed\n", failed));
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

    // Show pruned dead features
    if !self.pruned_features.is_empty() {
      s.push_str(&format!("\nDead features pruned: {}\n", self.pruned_features.len()));
      // Group by crate
      let mut by_crate: HashMap<&str, Vec<&str>> = HashMap::new();
      for pf in &self.pruned_features {
        by_crate.entry(&pf.crate_name).or_default().push(&pf.feature_name);
      }
      for (crate_name, features) in by_crate {
        s.push_str(&format!("  - {}: {}\n", crate_name, features.join(", ")));
      }
    }

    // Show version mismatches
    if !self.version_mismatches.is_empty() {
      s.push_str(&format!(
        "\n⚠️  Version mismatches with workspace.dependencies: {}\n",
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
        "\n🗑️  Unused dependencies detected: {}\n",
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

// ============================================================================
// Main Analyzer
// ============================================================================

/// Analyzes workspace for dependency unification opportunities
pub struct UnifyAnalyzer {
  metadata: MultiTargetMetadata,
  manifests: ManifestAnalyzer,
  /// Configuration for unification behavior (can be overridden at runtime)
  pub config: UnifyConfig,
  /// Existing workspace dependencies (already in [workspace.dependencies])
  existing_workspace_deps: HashMap<String, ExistingWorkspaceDep>,
  /// Workspace root path (for path normalization)
  workspace_root: PathBuf,
}

impl UnifyAnalyzer {
  /// Create a new analyzer from workspace context
  pub fn new(ctx: &WorkspaceContext) -> RailResult<Self> {
    // Use pre-loaded multi-target metadata if available, otherwise load it
    let metadata = if let Some(ref cached) = ctx.multi_target_metadata {
      // Already loaded in WorkspaceContext - just clone Arc (cheap)
      (**cached).clone()
    } else {
      // Not pre-loaded (no targets configured) - load default
      let targets = ctx.config.as_ref().map(|c| c.targets.clone()).unwrap_or_default();
      MultiTargetMetadata::load_parallel(ctx.workspace_root(), &targets)?
    };

    // Parse all manifests once
    let workspace_packages = metadata.workspace_packages();
    let manifests = ManifestAnalyzer::parse_workspace(ctx.workspace_root(), &workspace_packages)?;

    // Parse existing workspace.dependencies to avoid duplicates
    let existing_workspace_deps = parse_existing_workspace_deps(ctx.workspace_root())?;

    // Build config from context
    let config = ctx.config.as_ref().map(|c| c.unify.clone()).unwrap_or_default();

    Ok(Self {
      metadata,
      manifests,
      config,
      existing_workspace_deps,
      workspace_root: ctx.workspace_root().to_path_buf(),
    })
  }

  /// Analyze workspace and generate unification plan
  pub fn analyze(&self) -> RailResult<UnificationPlan> {
    let mut workspace_deps = Vec::new();
    let mut member_edits: HashMap<String, Vec<MemberEdit>> = HashMap::new();
    let mut member_paths: HashMap<String, PathBuf> = HashMap::new();
    let mut issues = Vec::new();
    let mut duplicates_cleaned = Vec::new();
    let mut version_mismatches = Vec::new();

    // Pre-compute workspace member names for reuse
    let workspace_member_names: HashSet<String> = self
      .metadata
      .workspace_packages()
      .iter()
      .map(|pkg| pkg.name.to_string())
      .collect();

    // Build member_paths mapping from metadata
    for pkg in self.metadata.workspace_packages() {
      let manifest_path = pkg.manifest_path.clone().into_std_path_buf();
      member_paths.insert(pkg.name.to_string(), manifest_path);
    }

    println!("Analyzing {} dependencies...", self.manifests.all_dependencies().len());

    // Process each dependency
    for dep_key in self.manifests.all_dependencies() {
      // Skip if excluded
      if self.config.exclude.contains(&dep_key.name) {
        continue;
      }

      // Skip if not enough usage
      let usage_count = self.manifests.usage_count(dep_key);
      if usage_count < 2 && !self.config.include.contains(&dep_key.name) {
        continue;
      }

      // Skip renamed dependencies unless configured - BLOCKING ERROR
      if dep_key.renamed_from.is_some() && !self.config.include_renamed {
        issues.push(UnifyIssue {
          dep_name: format!(
            "{} (renamed from {})",
            dep_key.name,
            dep_key.renamed_from.as_ref().unwrap()
          ),
          severity: IssueSeverity::Error,
          message: "Renamed dependency detected - BLOCKING (enable with include_renamed = true)".to_string(),
        });
        continue;
      }

      // Get usage sites early - needed for version checks
      let usage_sites = self.manifests.get_usage_sites(dep_key);

      // === CRITICAL: Check for major version conflicts ===
      // Different major versions should NEVER be merged - this is an anti-pattern
      // that users must fix manually. Merging features across major versions
      // produces invalid configurations (e.g., features that don't exist in the selected version).
      let major_versions = find_major_version_conflicts(&usage_sites);
      if major_versions.len() > 1 {
        let versions_str: Vec<_> = major_versions.iter().map(|v| v.to_string()).collect();
        issues.push(UnifyIssue {
          dep_name: dep_key.name.clone(),
          severity: IssueSeverity::Error,
          message: format!(
            "Multiple major versions detected (majors: {}) - cannot unify safely. \
             This is an anti-pattern in Rust workspaces that causes duplicate compilation \
             and wasted build resources. Please consolidate to a single major version.",
            versions_str.join(", ")
          ),
        });
        continue; // Skip this dependency entirely
      }

      // === Issue B: Check for exact version pins ===
      let has_exact_pin = usage_sites
        .iter()
        .any(|u| u.declared_version.as_ref().map(|v| is_exact_pin(v)).unwrap_or(false));

      if has_exact_pin {
        match self.config.exact_pin_handling {
          ExactPinHandling::Skip => {
            // Skip this dep entirely - don't unify exact pins
            continue;
          }
          ExactPinHandling::Warn => {
            issues.push(UnifyIssue {
              dep_name: dep_key.name.clone(),
              severity: IssueSeverity::Warning,
              message: "Has exact version pin (=x.y.z) - converting to caret (^). \
                        Consider adding to unify.exclude or setting exact_pin_handling = \"skip\""
                .to_string(),
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
      if versions.is_empty() {
        continue; // Not a direct dependency of any workspace member
      }

      // Get unique versions across all targets
      let unique_versions: HashSet<_> = versions.values().collect();

      // Always use highest version (cargo's resolver already picks highest compatible)
      // When targets resolve to different versions, we unify to highest
      let version = if unique_versions.len() > 1 {
        // Multiple versions found across targets - use highest
        // This is the "silent win" - we unify duplicates automatically
        let selected = unique_versions.iter().max().unwrap();

        // Track the cleanup for reporting
        let mut versions_found: Vec<_> = unique_versions.iter().map(|v| v.to_string()).collect();
        versions_found.sort();
        duplicates_cleaned.push(DuplicateCleanup {
          dep_name: dep_key.name.clone(),
          versions_found,
          selected_version: selected.to_string(),
        });

        selected
      } else {
        unique_versions.iter().next().unwrap()
      };

      // Construct version requirement - preserve exact pins if configured
      let version_req = if has_exact_pin && self.config.exact_pin_handling == ExactPinHandling::Preserve {
        // Find the exact pin version from declared_version
        let exact_version = usage_sites
          .iter()
          .find_map(|u| u.declared_version.as_ref().filter(|v| is_exact_pin(v)))
          .cloned()
          .unwrap_or_else(|| format!("={}", version));
        VersionReq::parse(&exact_version).unwrap_or_else(|_| VersionReq::parse(&format!("^{}", version)).unwrap())
      } else {
        VersionReq::parse(&format!("^{}", version)).unwrap_or_else(|_| VersionReq::parse(&version.to_string()).unwrap())
      };

      // === Issue A: Check for version mismatches with existing workspace.dependencies ===
      if let Some(existing_ws_dep) = self.existing_workspace_deps.get(&dep_key.name)
        && let Some(ref ws_version) = existing_ws_dep.version
      {
        for usage in &usage_sites {
          if let Some(ref declared) = usage.declared_version
            && !versions_compatible(declared, ws_version)
          {
            version_mismatches.push(VersionMismatch {
              member: usage.used_by.clone(),
              dep_name: dep_key.name.clone(),
              member_version: declared.clone(),
              workspace_version: ws_version.clone(),
            });

            // Add as issue based on strictness config
            let severity = if self.config.strict_version_compat {
              IssueSeverity::Error
            } else {
              IssueSeverity::Warning
            };
            issues.push(UnifyIssue {
              dep_name: dep_key.name.clone(),
              severity,
              message: format!(
                "{} declares \"{}\" but workspace.dependencies has \"{}\". \
                 Converting to workspace = true may break this crate.",
                usage.used_by, declared, ws_version
              ),
            });
          }
        }
      }

      // Check for mixed defaults - use union strategy if present
      let has_mixed_defaults = self.manifests.has_mixed_defaults(dep_key);

      // First try intersection strategy
      let intersection = self.manifests.compute_intersection(dep_key);

      let (features, default_features) = if has_mixed_defaults || intersection.is_empty() {
        // Use union strategy for:
        // 1. Mixed default-features settings
        // 2. No common features (empty intersection)
        // Set default-features = true (max/union strategy)
        (self.manifests.compute_union(dep_key), true)
      } else {
        // Use intersection (minimal) strategy
        // Use conservative default-features policy
        let df = self.manifests.default_features_policy(dep_key).unwrap_or(true);
        (intersection, df)
      };

      // Check if target-specific
      let targets_with_dep = self.metadata.targets_with_dep(&dep_key.name);
      let all_targets = self.metadata.targets();
      let target = if targets_with_dep.len() < all_targets.len() {
        // This is target-specific, try to infer cfg
        Some(self.infer_target_cfg(&targets_with_dep))
      } else {
        None
      };

      // Get users
      let users: HashSet<_> = usage_sites.iter().map(|u| u.used_by.clone()).collect();

      // === Issue C: Check include_paths config before processing path dependencies ===
      let dep_path: Option<PathBuf> = if self.config.include_paths {
        // Check if any usage has a path (path dependencies)
        // If so, check if the dep is a workspace member to include path in workspace.dependencies
        usage_sites.iter().find_map(|u| {
          u.path.as_ref().and_then(|p| {
            if workspace_member_names.contains(&dep_key.name) {
              // Normalize path relative to workspace root
              // The path in usage (p) is relative to the member's Cargo.toml
              // We need to make it relative to the workspace root
              if let Some(ref manifest_path) = u.manifest_path {
                // Get the member's directory (parent of Cargo.toml)
                let member_dir = manifest_path.parent().unwrap_or(manifest_path);
                // Resolve the dep path relative to the member's directory
                let absolute_dep_path = member_dir.join(p);
                // Canonicalize to resolve .. and symlinks (if path exists)
                let canonical_workspace = self
                  .workspace_root
                  .canonicalize()
                  .unwrap_or_else(|_| self.workspace_root.clone());
                let absolute_dep_path = absolute_dep_path.canonicalize().unwrap_or(absolute_dep_path);
                // Make relative to workspace root
                if let Ok(relative) = absolute_dep_path.strip_prefix(&canonical_workspace) {
                  Some(relative.to_path_buf())
                } else {
                  // Fallback: if canonicalization moved outside workspace, use the original resolved path
                  // Try strip_prefix on non-canonicalized path
                  let resolved = member_dir.join(p);
                  if let Ok(relative) = resolved.strip_prefix(&self.workspace_root) {
                    Some(relative.to_path_buf())
                  } else {
                    // Last resort: just use the path as-is
                    Some(PathBuf::from(p))
                  }
                }
              } else {
                // No manifest_path available - just use as-is (shouldn't happen)
                Some(PathBuf::from(p))
              }
            } else {
              None
            }
          })
        })
      } else {
        None // include_paths = false, skip all path deps
      };

      // Compute the unified features (this is what workspace.dependencies should have)
      let computed_features: Vec<String> = features.into_iter().collect();

      // Check if this dep already exists in workspace.dependencies
      let existing_dep = self.existing_workspace_deps.get(&dep_key.name);

      // Determine if we need to update workspace.dependencies
      // Update if: dep doesn't exist, OR features differ, OR default-features differs, OR has path
      let needs_workspace_update = match existing_dep {
        None => true, // Doesn't exist, need to add
        Some(existing) => {
          // Check if features are different
          let mut existing_features: Vec<String> = existing.features.clone();
          let mut computed: Vec<String> = computed_features.clone();
          existing_features.sort();
          computed.sort();

          // Also check if existing has path but we found one now
          let path_differs = dep_path.is_some() && existing.path.is_none();

          existing_features != computed || existing.default_features != default_features || path_differs
        }
      };

      // The features to use for local feature calculation
      // Use computed features (which will be what goes in workspace.dependencies)
      let workspace_features = computed_features.clone();

      // Create unified dep if workspace needs update
      if needs_workspace_update {
        let unified = UnifiedDep {
          name: dep_key.name.clone(),
          version_req,
          features: workspace_features.clone(),
          default_features,
          used_by: users.into_iter().collect(),
          target,
          path: dep_path, // Include path for workspace member deps
        };
        workspace_deps.push(unified);
      }

      // Compute member edits for ALL dep kinds (normal, dev, build)
      // Per design doc: all dep kinds should be unified, not just Normal
      // This happens whether or not the dep already exists in workspace.dependencies
      for usage in usage_sites {
        // Compute local features (member features - workspace features)
        let local_features: Vec<_> = usage
          .unconditional_features
          .iter()
          .filter(|f| !workspace_features.contains(f))
          .cloned()
          .collect();

        // For renamed dependencies (package = "..."), use the ALIAS name for manifest editing
        // e.g., for `old_getrandom = { package = "getrandom", ... }`, use "old_getrandom"
        let manifest_dep_name = dep_key.renamed_from.clone().unwrap_or_else(|| dep_key.name.clone());

        let edit = MemberEdit::UseWorkspace {
          dep_name: manifest_dep_name,
          dep_kind: usage.kind,
          target: usage.target.clone(), // Preserve target for correct section
          local_features,
          is_optional: usage.optional,
        };

        member_edits.entry(usage.used_by.clone()).or_default().push(edit);
      }
    }

    // Handle transitive fragmentation
    let transitive_pins = if self.config.pin_transitives {
      println!("Analyzing transitive dependencies...");
      let fragmented = self.metadata.find_fragmented_transitives();

      fragmented.into_iter().map(|f| (f.name, f.unified_features)).collect()
    } else {
      Vec::new()
    };

    // Compute MSRV if enabled
    let computed_msrv = if self.config.msrv {
      println!("Computing MSRV from dependency graph...");
      self.metadata.compute_msrv()
    } else {
      None
    };

    // Scan for dead features if enabled
    let pruned_features = if self.config.prune_dead_features {
      println!("Scanning for dead features in resolved graph...");
      self.scan_dead_features()
    } else {
      Vec::new()
    };

    // === Issue D: Detect unused dependencies ===
    let unused_deps = if self.config.detect_unused {
      println!("Detecting unused dependencies...");
      self.find_unused_deps()
    } else {
      Vec::new()
    };

    // Run validation
    let validation_results = self.validate_targets()?;

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
      version_mismatches,
      unused_deps,
    })
  }

  /// Validate that the unification works for all targets
  fn validate_targets(&self) -> RailResult<Vec<ValidationResult>> {
    let mut results = Vec::new();

    for target in self.metadata.targets() {
      // Simple check: does metadata load successfully for this target?
      // In production, you might want to run `cargo check --target=X`
      let success = self.metadata.get(target).is_some();

      results.push(ValidationResult {
        target: target.to_string(),
        success,
        error: if !success {
          Some("Failed to load metadata".to_string())
        } else {
          None
        },
      });
    }

    Ok(results)
  }

  /// Infer a cfg expression from a list of targets
  fn infer_target_cfg(&self, targets: &[String]) -> String {
    // Simple heuristic: if all Unix-like, use cfg(unix)
    if targets.iter().all(|t| t.contains("linux") || t.contains("darwin")) {
      "cfg(unix)".to_string()
    } else if targets.iter().all(|t| t.contains("windows")) {
      "cfg(windows)".to_string()
    } else {
      // Fall back to listing all targets
      format!(
        "cfg(any({}))",
        targets
          .iter()
          .map(|t| format!("target = \"{}\"", t))
          .collect::<Vec<_>>()
          .join(", ")
      )
    }
  }

  /// Detect dead features using resolved cargo metadata
  ///
  /// Returns a list of features that are declared in workspace crates but never
  /// enabled by any consumer in the resolved dependency graph across all targets.
  fn scan_dead_features(&self) -> Vec<PrunedFeature> {
    let mut pruned = Vec::new();

    // Analyze workspace using resolved metadata
    let results = FeatureScanner::analyze_workspace(&self.metadata);

    // Collect dead features
    for result in results {
      for feature_name in result.dead_features {
        pruned.push(PrunedFeature {
          crate_name: result.crate_name.clone(),
          feature_name,
        });
      }
    }

    if !pruned.is_empty() {
      let crate_count = self.metadata.workspace_packages().len();
      println!("  Found {} dead features across {} crates", pruned.len(), crate_count);
    }

    pruned
  }

  /// Detect unused dependencies in workspace members
  ///
  /// Compares declared dependencies (from Cargo.toml) against the resolved
  /// cargo graph to find deps that are declared but never actually used.
  /// This uses the resolved metadata as the source of truth.
  fn find_unused_deps(&self) -> Vec<UnusedDep> {
    let mut unused = Vec::new();

    // Pre-compute workspace member names (these are never "unused")
    let workspace_member_names: HashSet<String> = self
      .metadata
      .workspace_packages()
      .iter()
      .map(|pkg| pkg.name.to_string())
      .collect();

    // For each workspace member, check declared deps against resolved
    for member in &self.manifests.members {
      // Get resolved deps for this member from the metadata
      let resolved_deps = self.get_resolved_deps_for_member(&member.package_name);

      // Check each declared dependency
      for (dep_key, usage) in &member.dependencies {
        // Skip workspace member deps - they're always structurally "used"
        if workspace_member_names.contains(&dep_key.name) {
          continue;
        }

        // Check if this dep appears in the resolved graph for this member
        if !resolved_deps.contains(&dep_key.name) {
          unused.push(UnusedDep {
            member: member.package_name.clone(),
            dep_name: dep_key.name.clone(),
            kind: usage.kind,
          });
        }
      }
    }

    if !unused.is_empty() {
      println!("  Found {} unused dependencies", unused.len());
    }

    unused
  }

  /// Get all resolved direct dependencies for a workspace member
  ///
  /// Uses the resolved cargo metadata graph to find what deps are actually
  /// used by a member. This is the source of truth for dependency usage.
  fn get_resolved_deps_for_member(&self, member_name: &str) -> HashSet<String> {
    let mut deps = HashSet::new();

    // Check across all targets to ensure we catch platform-specific deps
    for metadata in self.metadata.targets().iter().filter_map(|t| self.metadata.get(t)) {
      if let Some(resolve) = &metadata.resolve {
        // Find the node for this member
        for node in &resolve.nodes {
          if let Some(pkg) = metadata.packages.iter().find(|p| p.id == node.id)
            && pkg.name == member_name
          {
            // Collect all direct dependencies from this node
            for dep in &node.deps {
              deps.insert(dep.name.clone());
            }
          }
        }
      }
    }

    deps
  }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Check if a version string is an exact pin ("=x.y.z")
fn is_exact_pin(version: &str) -> bool {
  version.starts_with('=') && !version.starts_with(">=")
}

/// Extract major version from a declared version string
///
/// Returns the major version number, handling various version formats:
/// - "1.0" -> Some(1)
/// - "^2.3.0" -> Some(2)
/// - ">=0.5" -> Some(0)
fn extract_major_version(version: &str) -> Option<u32> {
  let cleaned = strip_version_op(version);
  let parts: Vec<&str> = cleaned.split('.').collect();
  parts.first().and_then(|s| s.parse().ok())
}

/// Check for major version conflicts in declared versions
///
/// Returns the set of unique major versions found. If the set has more than
/// one element, there's a conflict that cannot be safely unified.
fn find_major_version_conflicts(
  usages: &[&crate::cargo::manifest_analyzer::DepUsage],
) -> std::collections::HashSet<u32> {
  usages
    .iter()
    .filter_map(|u| u.declared_version.as_ref())
    .filter_map(|v| extract_major_version(v))
    .collect()
}

/// Check if two version requirements are compatible
///
/// This is a heuristic check - it compares the major.minor portions
/// to detect obvious incompatibilities like "0.11" vs "0.13".
fn versions_compatible(member_version: &str, workspace_version: &str) -> bool {
  // Strip leading operators for comparison
  let member_ver = strip_version_op(member_version);
  let workspace_ver = strip_version_op(workspace_version);

  // Parse into parts
  let member_parts: Vec<&str> = member_ver.split('.').collect();
  let workspace_parts: Vec<&str> = workspace_ver.split('.').collect();

  // For semver compatibility, major must match (or be 0)
  // For 0.x versions, minor must also match
  if member_parts.is_empty() || workspace_parts.is_empty() {
    return true; // Can't determine, assume compatible
  }

  let member_major = member_parts[0].parse::<u32>().unwrap_or(0);
  let workspace_major = workspace_parts[0].parse::<u32>().unwrap_or(0);

  if member_major != workspace_major {
    return false;
  }

  // For 0.x versions, minor must match
  if member_major == 0 {
    let member_minor = member_parts.get(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    let workspace_minor = workspace_parts.get(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    if member_minor != workspace_minor {
      return false;
    }
  }

  true
}

/// Strip version operators from a version string
fn strip_version_op(version: &str) -> &str {
  version
    .trim_start_matches(">=")
    .trim_start_matches("<=")
    .trim_start_matches('>')
    .trim_start_matches('<')
    .trim_start_matches('=')
    .trim_start_matches('^')
    .trim_start_matches('~')
    .trim()
}

// ============================================================================
// Report Generation
// ============================================================================

/// Generate a markdown report from the unification plan
pub struct UnifyReport;

impl UnifyReport {
  /// Generates a markdown report from the unification plan
  pub fn from_plan(plan: &UnificationPlan) -> String {
    let mut md = String::new();

    md.push_str("# Cargo Rail Unification Report\n\n");
    md.push_str(&format!(
      "Generated: {}\n\n",
      chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    ));

    md.push_str("## Summary\n\n");
    md.push_str(&format!("- **Dependencies Unified:** {}\n", plan.workspace_deps.len()));
    md.push_str(&format!("- **Members Updated:** {}\n", plan.member_edits.len()));
    md.push_str(&format!("- **Transitive Pins:** {}\n", plan.transitive_pins.len()));
    md.push_str(&format!(
      "- **Duplicates Cleaned:** {}\n",
      plan.duplicates_cleaned.len()
    ));
    md.push_str(&format!("- **Dead Features Pruned:** {}\n", plan.pruned_features.len()));
    md.push_str(&format!(
      "- **Version Mismatches:** {}\n",
      plan.version_mismatches.len()
    ));
    md.push_str(&format!("- **Unused Dependencies:** {}\n", plan.unused_deps.len()));
    md.push_str(&format!("- **Issues:** {}\n\n", plan.issues.len()));

    if !plan.workspace_deps.is_empty() {
      md.push_str("## Unified Dependencies\n\n");
      md.push_str("| Dependency | Version | Features | Used By |\n");
      md.push_str("|------------|---------|----------|----------|\n");

      for dep in &plan.workspace_deps {
        let features = if dep.features.is_empty() {
          "(default)".to_string()
        } else {
          dep.features.join(", ")
        };

        let users = if dep.used_by.len() > 3 {
          format!("{} crates", dep.used_by.len())
        } else {
          dep.used_by.join(", ")
        };

        md.push_str(&format!(
          "| {} | {} | {} | {} |\n",
          dep.name, dep.version_req, features, users
        ));
      }
      md.push('\n');
    }

    if !plan.duplicates_cleaned.is_empty() {
      md.push_str("## Duplicates Unified\n\n");
      md.push_str("| Dependency | Selected Version | Previous Versions |\n");
      md.push_str("|------------|------------------|-------------------|\n");

      for dup in &plan.duplicates_cleaned {
        md.push_str(&format!(
          "| {} | {} | {} |\n",
          dup.dep_name,
          dup.selected_version,
          dup.versions_found.join(", ")
        ));
      }
      md.push('\n');
    }

    if !plan.pruned_features.is_empty() {
      md.push_str("## Dead Features Pruned\n\n");
      md.push_str("| Crate | Feature |\n");
      md.push_str("|-------|----------|\n");

      for pf in &plan.pruned_features {
        md.push_str(&format!("| {} | {} |\n", pf.crate_name, pf.feature_name));
      }
      md.push('\n');
    }

    if !plan.version_mismatches.is_empty() {
      md.push_str("## Version Mismatches\n\n");
      md.push_str("| Member | Dependency | Declared | Workspace |\n");
      md.push_str("|--------|------------|----------|------------|\n");

      for mismatch in &plan.version_mismatches {
        md.push_str(&format!(
          "| {} | {} | {} | {} |\n",
          mismatch.member, mismatch.dep_name, mismatch.member_version, mismatch.workspace_version
        ));
      }
      md.push('\n');
    }

    if !plan.unused_deps.is_empty() {
      md.push_str("## Unused Dependencies\n\n");
      md.push_str("| Member | Dependency | Kind |\n");
      md.push_str("|--------|------------|------|\n");

      for ud in &plan.unused_deps {
        let kind_str = match ud.kind {
          DepKind::Normal => "normal",
          DepKind::Dev => "dev",
          DepKind::Build => "build",
        };
        md.push_str(&format!("| {} | {} | {} |\n", ud.member, ud.dep_name, kind_str));
      }
      md.push('\n');
    }

    if !plan.issues.is_empty() {
      md.push_str("## Issues\n\n");
      for issue in &plan.issues {
        let icon = if issue.severity == IssueSeverity::Error {
          "❌"
        } else {
          "⚠️"
        };
        md.push_str(&format!("{} **{}**: {}\n", icon, issue.dep_name, issue.message));
      }
      md.push('\n');
    }

    md
  }

  /// Writes the report to a file
  pub fn write_to_file(plan: &UnificationPlan, path: &std::path::Path) -> RailResult<()> {
    let content = Self::from_plan(plan);

    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
    }

    std::fs::write(path, content)?;
    Ok(())
  }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_extract_major_version() {
    // Simple versions
    assert_eq!(extract_major_version("1.0"), Some(1));
    assert_eq!(extract_major_version("2.3.4"), Some(2));
    assert_eq!(extract_major_version("0.99.3"), Some(0));

    // With operators
    assert_eq!(extract_major_version("^1.0"), Some(1));
    assert_eq!(extract_major_version("~2.0"), Some(2));
    assert_eq!(extract_major_version(">=3.0"), Some(3));
    assert_eq!(extract_major_version("=4.0.0"), Some(4));

    // Edge cases
    assert_eq!(extract_major_version(""), None);
    assert_eq!(extract_major_version("invalid"), None);
  }

  #[test]
  fn test_is_exact_pin() {
    assert!(is_exact_pin("=1.0.0"));
    assert!(is_exact_pin("=0.8.0"));
    assert!(!is_exact_pin(">=1.0.0"));
    assert!(!is_exact_pin("^1.0.0"));
    assert!(!is_exact_pin("1.0.0"));
    assert!(!is_exact_pin("~1.0.0"));
  }

  #[test]
  fn test_strip_version_op() {
    assert_eq!(strip_version_op("=1.0.0"), "1.0.0");
    assert_eq!(strip_version_op("^1.0.0"), "1.0.0");
    assert_eq!(strip_version_op(">=1.0.0"), "1.0.0");
    assert_eq!(strip_version_op("~1.0.0"), "1.0.0");
    assert_eq!(strip_version_op("1.0.0"), "1.0.0");
    assert_eq!(strip_version_op("<=2.0"), "2.0");
    assert_eq!(strip_version_op(">0.5"), "0.5");
  }

  #[test]
  fn test_versions_compatible_major_match() {
    // Same major version (>=1.0) should be compatible
    assert!(versions_compatible("1.0", "1.5"));
    assert!(versions_compatible("^1.0", "1.5"));
    assert!(versions_compatible("2.0", "2.3.1"));
  }

  #[test]
  fn test_versions_compatible_major_mismatch() {
    // Different major versions are incompatible
    assert!(!versions_compatible("1.0", "2.0"));
    assert!(!versions_compatible("^2.0", "3.0"));
  }

  #[test]
  fn test_versions_compatible_zero_major() {
    // For 0.x versions, minor must also match
    assert!(versions_compatible("0.11", "0.11.5"));
    assert!(versions_compatible("^0.11", "0.11"));
    assert!(!versions_compatible("0.11", "0.13"));
    assert!(!versions_compatible("^0.8", "0.9"));
  }

  #[test]
  fn test_versions_compatible_exact_pins() {
    // Exact pins with same version are compatible
    assert!(versions_compatible("=1.0.0", "1.0.0"));
    assert!(versions_compatible("=0.8.0", "0.8"));
    // Different versions are incompatible
    assert!(!versions_compatible("=1.0.0", "2.0.0"));
  }
}
