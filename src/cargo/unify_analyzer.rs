//! Clean unification analyzer using the HYBRID approach
//!
//! This is the core of the new unify implementation that properly integrates
//! with WorkspaceContext and uses multi-target metadata + manifest analysis.

use crate::cargo::{
  manifest_analyzer::{DepKind, ExistingWorkspaceDep, ManifestAnalyzer, parse_existing_workspace_deps},
  multi_target_metadata::MultiTargetMetadata,
};
use crate::config::UnifyConfig;
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
      println!("Using pre-loaded multi-target metadata");
      (**cached).clone()
    } else {
      // Not pre-loaded (no targets configured) - load default
      let targets = ctx.config.as_ref().map(|c| c.targets.clone()).unwrap_or_default();

      println!(
        "Loading metadata for {} target(s)...",
        if targets.is_empty() { 1 } else { targets.len() }
      );

      MultiTargetMetadata::load_parallel(ctx.workspace_root(), &targets)?
    };

    println!("Parsing workspace manifests...");

    // Parse all manifests once
    let workspace_packages = metadata.workspace_packages();
    let manifests = ManifestAnalyzer::parse_workspace(ctx.workspace_root(), &workspace_packages)?;

    // Parse existing workspace.dependencies to avoid duplicates
    let existing_workspace_deps = parse_existing_workspace_deps(ctx.workspace_root())?;
    if !existing_workspace_deps.is_empty() {
      println!(
        "Found {} existing workspace dependencies",
        existing_workspace_deps.len()
      );
    }

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

      // Get version from metadata - but ONLY from direct dependencies of workspace members
      // This uses direct_dep_versions() which filters out transitive dependencies
      let versions = self.metadata.direct_dep_versions(&dep_key.name);
      if versions.is_empty() {
        continue; // Not a direct dependency of any workspace member
      }

      // Check version compatibility across workspace members
      let unique_versions: HashSet<_> = versions.values().collect();
      if unique_versions.len() > 1 {
        issues.push(UnifyIssue {
          dep_name: dep_key.name.clone(),
          severity: IssueSeverity::Warning,
          message: format!(
            "Version conflict - skipping (found versions: {})",
            unique_versions
              .iter()
              .map(|v| v.to_string())
              .collect::<Vec<_>>()
              .join(", ")
          ),
        });
        continue;
      }

      let version = unique_versions.into_iter().next().unwrap();
      let version_req = VersionReq::parse(&format!("^{}", version))
        .unwrap_or_else(|_| VersionReq::parse(&version.to_string()).unwrap());

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
      let usage_sites = self.manifests.get_usage_sites(dep_key);
      let users: HashSet<_> = usage_sites.iter().map(|u| u.used_by.clone()).collect();

      // Check if any usage has a path (path dependencies)
      // If so, check if the dep is a workspace member to include path in workspace.dependencies
      let dep_path: Option<PathBuf> = usage_sites.iter().find_map(|u| {
        u.path.as_ref().and_then(|p| {
          // Check if this dep name matches a workspace member
          let workspace_members: HashSet<String> = self
            .metadata
            .workspace_packages()
            .iter()
            .map(|pkg| pkg.name.to_string())
            .collect();
          if workspace_members.contains(&dep_key.name) {
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
      });

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

        let edit = MemberEdit::UseWorkspace {
          dep_name: dep_key.name.clone(),
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

    // Run validation
    let validation_results = self.validate_targets()?;

    Ok(UnificationPlan {
      workspace_deps,
      member_edits,
      member_paths,
      transitive_pins,
      validation_results,
      issues,
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
