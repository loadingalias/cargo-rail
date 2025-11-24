//! Clean unification analyzer using the HYBRID approach
//!
//! This is the core of the new unify implementation that properly integrates
//! with WorkspaceContext and uses multi-target metadata + manifest analysis.

use crate::cargo::{
  manifest_analyzer::{DepKind, ManifestAnalyzer},
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

    // Show dependencies to unify
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

    // Build config from context
    let config = ctx.config.as_ref().map(|c| c.unify.clone()).unwrap_or_default();

    Ok(Self {
      metadata,
      manifests,
      config,
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

      // Get version from metadata
      let versions = self.metadata.all_versions(&dep_key.name);
      if versions.is_empty() {
        continue; // Not in resolved graph
      }

      // Check version compatibility
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

      // Create unified dep
      let unified = UnifiedDep {
        name: dep_key.name.clone(),
        version_req,
        features: features.into_iter().collect(),
        default_features,
        used_by: users.into_iter().collect(),
        target,
        path: None, // Path deps handled separately
      };

      // Compute member edits
      for usage in usage_sites {
        if usage.kind != DepKind::Normal {
          continue; // Only edit normal deps
        }

        // Compute local features (member features - workspace features)
        let local_features: Vec<_> = usage
          .unconditional_features
          .iter()
          .filter(|f| !unified.features.contains(*f))
          .cloned()
          .collect();

        let edit = MemberEdit::UseWorkspace {
          dep_name: dep_key.name.clone(),
          dep_kind: usage.kind,
          local_features,
          is_optional: usage.optional,
        };

        member_edits.entry(usage.used_by.clone()).or_default().push(edit);
      }

      workspace_deps.push(unified);
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
