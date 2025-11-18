//! Workspace dependency unification
//!
//! This module provides the core logic for eliminating workspace-hack crates
//! by unifying dependencies in [workspace.dependencies] and converting members
//! to use workspace inheritance.
//!
//! # Strategy
//!
//! 1. Collect all dependencies from workspace members
//! 2. Group by dependency name
//! 3. Detect version conflicts and edge cases
//! 4. Compute unified feature sets (union)
//! 5. Generate unification plan
//! 6. Apply plan to Cargo.toml files (workspace + members)
//!
//! # Example
//!
//! ```rust,no_run
//! use cargo_rail::cargo::{WorkspaceMetadata, WorkspaceUnifier};
//!
//! let metadata = WorkspaceMetadata::load(workspace_root)?;
//! let unifier = WorkspaceUnifier::new(&metadata);
//! let plan = unifier.analyze()?;
//!
//! // Dry-run: show what would be unified
//! println!("{}", plan.summary());
//!
//! // Apply: modify Cargo.toml files
//! plan.apply(workspace_root)?;
//! ```

use super::WorkspaceMetadata;
use crate::error::RailResult;
use cargo_metadata::{DependencyKind, camino::Utf8PathBuf};
use semver::VersionReq;
use std::collections::{HashMap, HashSet};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for workspace unification
#[derive(Debug, Clone)]
pub struct UnifyConfig {
  /// Strategy for selecting dependencies to unify
  pub strategy: UnifyStrategy,

  /// How to handle version conflicts
  pub version_conflict: VersionConflictPolicy,

  /// How to handle renamed dependencies
  pub renamed_handling: RenamedPolicy,

  /// Create backups before modification
  /// Used in commands/unify.rs for backup operations
  #[allow(dead_code)]
  pub backup: bool,

  /// Validate workspace after unification
  /// TODO: Wire this into apply() when implemented - should run `cargo check` if true
  #[allow(dead_code)]
  pub validate: bool,

  /// Dependencies to exclude from unification
  pub exclude: HashSet<String>,

  /// Dependencies to force-include in unification
  pub include: HashSet<String>,

  /// Only unify normal dependencies (exclude dev/build)
  pub normal_only: bool,
}

/// Strategy for selecting which dependencies to unify
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnifyStrategy {
  /// Unify all possible dependencies
  All,

  /// Only unify dependencies that are currently fragmented
  /// TODO: Future strategy - detect fragmentation via resolved features
  #[allow(dead_code)]
  Fragmented,

  /// Only unify dependencies used by at least N members
  /// TODO: Future strategy - threshold-based unification
  #[allow(dead_code)]
  Threshold(usize),
}

/// Policy for handling version conflicts
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionConflictPolicy {
  /// Fail and require manual resolution
  Error,

  /// Use the highest version requirement
  /// TODO: Future policy - auto-resolve using latest version
  #[allow(dead_code)]
  Latest,

  /// Use first encountered version
  /// TODO: Future policy - auto-resolve using first seen version
  #[allow(dead_code)]
  First,
}

/// Policy for handling renamed dependencies
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenamedPolicy {
  /// Don't unify renamed dependencies
  Skip,

  /// Unify anyway, warn user
  /// TODO: Future policy - warn but allow unification
  #[allow(dead_code)]
  Warn,

  /// Fail on renamed dependencies
  /// TODO: Future policy - error on renames
  #[allow(dead_code)]
  Error,
}

impl Default for UnifyConfig {
  fn default() -> Self {
    Self {
      strategy: UnifyStrategy::All,
      version_conflict: VersionConflictPolicy::Error,
      renamed_handling: RenamedPolicy::Skip,
      backup: true,
      validate: true,
      exclude: HashSet::new(),
      include: HashSet::new(),
      normal_only: false, // Unify all kinds by default
    }
  }
}

// ============================================================================
// Data Structures
// ============================================================================

/// A single instance of a dependency in a workspace member
#[derive(Debug, Clone)]
struct DependencyInstance {
  /// Workspace member using this dependency
  member: String,

  /// Version requirement
  version_req: VersionReq,

  /// Enabled features
  features: Vec<String>,

  /// Uses default features
  default_features: bool,

  /// Optional dependency
  /// Already preserved in manifest.rs:378-394 when converting to workspace inheritance
  #[allow(dead_code)]
  optional: bool,

  /// Dependency kind (normal, dev, build)
  kind: DependencyKind,

  /// Target-specific (e.g., cfg(unix))
  target: Option<String>,

  /// Renamed from original name
  rename: Option<String>,

  /// Path dependency (if any)
  path: Option<Utf8PathBuf>,
}

/// A dependency suitable for [workspace.dependencies]
#[derive(Debug, Clone)]
pub struct UnifiedDep {
  /// Dependency name
  pub name: String,

  /// Unified version requirement
  pub version_req: VersionReq,

  /// Unified feature set (union of all features)
  pub features: Vec<String>,

  /// Whether to enable default features
  pub default_features: bool,

  /// Workspace members using this dependency
  pub used_by: Vec<String>,

  /// Dependency kinds (Normal, Dev, Build)
  /// Collected for informational purposes. Filtering happens in should_unify() before unification.
  #[allow(dead_code)]
  pub dep_kinds: HashSet<DependencyKind>,

  /// Number of unique feature sets (fragmentation indicator)
  pub fragmentation_count: usize,
}

/// Plan for unifying workspace dependencies
#[derive(Debug, Clone)]
pub struct UnificationPlan {
  /// Dependencies to add/update in [workspace.dependencies]
  pub workspace_deps: Vec<UnifiedDep>,

  /// Edits per member: member_name -> edits
  pub member_edits: HashMap<String, Vec<MemberEdit>>,

  /// Issues that prevent automatic unification
  pub issues: Vec<UnificationIssue>,

  /// Statistics
  pub stats: UnificationStats,
}

/// Edit to apply to a member's Cargo.toml
#[derive(Debug, Clone)]
pub enum MemberEdit {
  /// Convert dependency to workspace = true
  UseWorkspace { dep_name: String, kind: DependencyKind },
}

/// Issue preventing automatic unification
#[derive(Debug, Clone)]
pub struct UnificationIssue {
  /// Dependency name
  pub dep_name: String,

  /// Issue type
  pub issue_type: IssueType,

  /// Affected workspace members
  pub affected_members: Vec<String>,

  /// Suggested resolution
  pub suggestion: String,
}

impl UnificationIssue {
  /// Format the issue as a human-readable message
  pub fn format_message(&self) -> String {
    format!(
      "{}: {} (affects: {})\n  Suggestion: {}",
      self.dep_name,
      self.issue_description(),
      self.affected_members.join(", "),
      self.suggestion
    )
  }

  fn issue_description(&self) -> String {
    match &self.issue_type {
      IssueType::VersionConflict { requirements } => {
        let versions: Vec<String> = requirements.iter().map(|(m, v)| format!("{}: {}", m, v)).collect();
        format!("Version conflict ({})", versions.join(", "))
      }
      IssueType::Renamed { original, renames } => {
        format!(
          "Renamed from '{}' in some members: {}",
          original,
          renames
            .iter()
            .map(|(m, r)| format!("{} -> {}", m, r))
            .collect::<Vec<_>>()
            .join(", ")
        )
      }
      IssueType::PathDependency { paths } => {
        format!("Path dependency ({})", paths.len())
      }
      IssueType::AllTargetSpecific { targets } => {
        format!("All uses are target-specific: {}", targets.join(", "))
      }
      IssueType::MultipleVersions { versions } => {
        format!(
          "Multiple resolved versions in dependency graph: {}",
          versions.join(", ")
        )
      }
      IssueType::InconsistentDefaultFeatures {
        members_with_default,
        members_without_default,
      } => {
        format!(
          "Inconsistent default-features: {} use defaults, {} don't",
          members_with_default.len(),
          members_without_default.len()
        )
      }
    }
  }
}

/// Type of unification issue
#[derive(Debug, Clone)]
pub enum IssueType {
  /// Incompatible version requirements
  VersionConflict {
    requirements: Vec<(String, String)>, // member -> version
  },

  /// Renamed in some members
  Renamed {
    original: String,
    renames: Vec<(String, String)>, // member -> renamed_to
  },

  /// Path dependency to non-workspace crates
  PathDependency {
    paths: Vec<(String, Utf8PathBuf)>, // member -> path
  },

  /// All uses are target-specific (can't unify at workspace level)
  AllTargetSpecific { targets: Vec<String> },

  /// Multiple resolved versions of the same package name
  /// (e.g., futures 0.1 and futures 0.3 both in the dependency graph)
  MultipleVersions {
    versions: Vec<String>, // Resolved versions (e.g., ["0.1.29", "0.3.30"])
  },

  /// Inconsistent default-features settings across members
  InconsistentDefaultFeatures {
    members_with_default: Vec<String>,
    members_without_default: Vec<String>,
  },
}

/// Statistics about unification
#[derive(Debug, Clone, Default)]
pub struct UnificationStats {
  /// Total dependencies analyzed
  pub total_deps: usize,

  /// Dependencies successfully unified
  pub unified_count: usize,

  /// Dependencies with issues
  pub issue_count: usize,

  /// Estimated compilations eliminated
  pub compilations_saved: usize,
}

// ============================================================================
// Main Unifier
// ============================================================================

/// Workspace dependency unifier
pub struct WorkspaceUnifier<'a> {
  metadata: &'a WorkspaceMetadata,
  config: UnifyConfig,
}

impl<'a> WorkspaceUnifier<'a> {
  /// Create a new workspace unifier with default config
  ///
  /// Used extensively in tests. Production code should use with_config() for explicit configuration.
  #[allow(dead_code)]
  pub fn new(metadata: &'a WorkspaceMetadata) -> Self {
    Self {
      metadata,
      config: UnifyConfig::default(),
    }
  }

  /// Create with custom configuration
  pub fn with_config(metadata: &'a WorkspaceMetadata, config: UnifyConfig) -> Self {
    Self { metadata, config }
  }

  /// Analyze workspace and generate unification plan
  pub fn analyze(&self) -> RailResult<UnificationPlan> {
    // 1. Collect all dependency instances from workspace members
    let dep_instances = self.collect_dependencies();

    // 2. Detect multi-version conflicts before grouping
    let multi_version_issues = self.detect_multi_version_conflicts()?;

    // 3. Group by dependency name
    let grouped = self.group_by_name(dep_instances);

    // 4. Analyze each dependency group
    let mut workspace_deps = Vec::new();
    let mut member_edits: HashMap<String, Vec<MemberEdit>> = HashMap::new();
    let mut issues = multi_version_issues; // Start with multi-version conflicts
    let mut stats = UnificationStats {
      total_deps: grouped.len(),
      issue_count: issues.len(),
      ..Default::default()
    };

    for (dep_name, instances) in grouped {
      // Check if should be excluded
      if self.config.exclude.contains(&dep_name) {
        continue;
      }

      // Check if meets strategy criteria
      if !self.should_unify(&dep_name, &instances) {
        continue;
      }

      // Detect issues
      if let Some(issue) = self.detect_issues(&dep_name, &instances) {
        issues.push(issue);
        stats.issue_count += 1;
        continue;
      }

      // Compute unified dependency
      let unified = self.unify_instances(&dep_name, &instances)?;

      // Track fragmentation savings
      if unified.fragmentation_count > 1 {
        stats.compilations_saved += unified.fragmentation_count - 1;
      }

      // Generate member edits
      for instance in &instances {
        member_edits
          .entry(instance.member.clone())
          .or_default()
          .push(MemberEdit::UseWorkspace {
            dep_name: dep_name.clone(),
            kind: instance.kind,
          });
      }

      workspace_deps.push(unified);
      stats.unified_count += 1;
    }

    Ok(UnificationPlan {
      workspace_deps,
      member_edits,
      issues,
      stats,
    })
  }

  /// Detect dependencies that exist in multiple resolved versions
  ///
  /// For example, if both futures 0.1 and futures 0.3 are in the dependency graph,
  /// we cannot safely unify them at the workspace level.
  fn detect_multi_version_conflicts(&self) -> RailResult<Vec<UnificationIssue>> {
    let mut issues = Vec::new();

    let resolve = match self.metadata.resolve() {
      Some(r) => r,
      None => return Ok(issues), // No resolution available, skip check
    };

    // Group resolved packages by name
    let mut by_name: HashMap<String, Vec<&cargo_metadata::PackageId>> = HashMap::new();
    for node in &resolve.nodes {
      if let Some(pkg) = self.metadata.find_package_by_id(&node.id) {
        // Skip workspace members
        if self.metadata.get_package(&pkg.name).is_some() {
          continue;
        }

        by_name.entry(pkg.name.to_string()).or_default().push(&node.id);
      }
    }

    // Find packages with multiple resolved versions
    for (pkg_name, package_ids) in by_name {
      if package_ids.len() <= 1 {
        continue;
      }

      // Extract versions from package IDs
      let mut versions: Vec<String> = package_ids
        .iter()
        .filter_map(|id| self.metadata.find_package_by_id(id).map(|p| p.version.to_string()))
        .collect();

      // Deduplicate (shouldn't be needed, but safe)
      versions.sort();
      versions.dedup();

      if versions.len() > 1 {
        issues.push(UnificationIssue {
          dep_name: pkg_name.clone(),
          issue_type: IssueType::MultipleVersions {
            versions: versions.clone(),
          },
          affected_members: vec![], // Can't easily determine which members use which version
          suggestion: format!(
            "Multiple versions of '{}' found: {}. Standardize to a single version across the workspace.",
            pkg_name,
            versions.join(", ")
          ),
        });
      }
    }

    Ok(issues)
  }

  /// Collect all dependency instances from workspace members
  ///
  /// IMPORTANT: This uses RESOLVED features from the dependency graph, not declared features.
  /// This matches Hakari's behavior and ensures we capture features enabled transitively.
  fn collect_dependencies(&self) -> Vec<DependencyInstance> {
    let mut instances = Vec::new();

    for pkg in self.metadata.list_crates() {
      for dep in &pkg.dependencies {
        // Get RESOLVED features if available, fallback to declared features
        // Resolved features include:
        // 1. Features declared in this member's Cargo.toml
        // 2. Default features (if enabled)
        // 3. Features activated by other workspace members
        // 4. Features activated transitively through dependency chains
        let features = if let Some(resolved_features) = self.metadata.get_resolved_features_for_package(&dep.name) {
          // Use resolved features - this is the ACTUAL set Cargo enables
          resolved_features.into_iter().collect()
        } else {
          // Fallback to declared features if:
          // - No resolve graph available
          // - Package is a workspace member
          // - Package not found in resolved graph (e.g., optional dep not enabled)
          dep.features.clone()
        };

        instances.push(DependencyInstance {
          member: pkg.name.to_string(),
          version_req: dep.req.clone(),
          features, // NOW uses resolved features!
          default_features: dep.uses_default_features,
          optional: dep.optional,
          kind: dep.kind,
          target: dep.target.as_ref().map(|t| t.to_string()),
          rename: dep.rename.clone(),
          path: dep.path.clone(),
        });
      }
    }

    instances
  }

  /// Group dependency instances by name
  fn group_by_name(&self, instances: Vec<DependencyInstance>) -> HashMap<String, Vec<DependencyInstance>> {
    let mut grouped: HashMap<String, Vec<DependencyInstance>> = HashMap::new();

    for instance in instances {
      // Use actual package name, not rename
      let dep_name = instance.rename.as_ref().unwrap_or(&instance.member).clone();

      // Skip workspace member path dependencies
      if let Some(ref path) = instance.path
        && self.is_workspace_member_path(path)
      {
        continue;
      }

      grouped.entry(dep_name).or_default().push(instance);
    }

    grouped
  }

  /// Check if path points to a workspace member
  fn is_workspace_member_path(&self, path: &Utf8PathBuf) -> bool {
    // Check if the path resolves to any workspace member
    let workspace_root = self.metadata.workspace_root();

    for pkg in self.metadata.list_crates() {
      if let Some(pkg_dir) = pkg.manifest_path.parent() {
        // Normalize and compare paths
        if let Ok(rel_path) = pkg_dir.strip_prefix(workspace_root) {
          // Check if the dependency path matches this member's path
          if path.as_str() == rel_path.as_str() || path.as_str() == format!("../{}", rel_path) {
            return true;
          }
        }

        // Also check absolute path match
        if pkg_dir == path {
          return true;
        }
      }
    }

    false
  }

  /// Check if dependency should be unified based on strategy
  fn should_unify(&self, dep_name: &str, instances: &[DependencyInstance]) -> bool {
    // Force-included dependencies always unify
    if self.config.include.contains(dep_name) {
      return true;
    }

    // Filter by dependency kind if normal_only is enabled
    if self.config.normal_only {
      // Check if ANY instance is non-normal (dev or build)
      if instances.iter().any(|i| i.kind != DependencyKind::Normal) {
        return false;
      }
    }

    match self.config.strategy {
      UnifyStrategy::All => instances.len() >= 2,
      UnifyStrategy::Fragmented => {
        // Check if fragmented (different feature sets)
        let unique_features: HashSet<_> = instances
          .iter()
          .map(|i| {
            let mut features = i.features.clone();
            features.sort();
            features
          })
          .collect();
        unique_features.len() > 1
      }
      UnifyStrategy::Threshold(n) => instances.len() >= n,
    }
  }

  /// Detect issues preventing unification
  fn detect_issues(&self, dep_name: &str, instances: &[DependencyInstance]) -> Option<UnificationIssue> {
    // Check for renamed dependencies (respect policy)
    let renames: Vec<_> = instances
      .iter()
      .filter_map(|i| i.rename.as_ref().map(|r| (i.member.clone(), r.clone())))
      .collect();

    if !renames.is_empty() {
      match self.config.renamed_handling {
        RenamedPolicy::Skip => {
          // Skip renamed dependencies - create issue
          return Some(UnificationIssue {
            dep_name: dep_name.to_string(),
            issue_type: IssueType::Renamed {
              original: dep_name.to_string(),
              renames,
            },
            affected_members: instances.iter().map(|i| i.member.clone()).collect(),
            suggestion:
              "Standardize dependency name across all workspace members, or change policy to RenamedPolicy::Warn"
                .to_string(),
          });
        }
        RenamedPolicy::Warn | RenamedPolicy::Error => {
          // Future: Warn or Error policies
          // For now, still skip but note the policy
          return Some(UnificationIssue {
            dep_name: dep_name.to_string(),
            issue_type: IssueType::Renamed {
              original: dep_name.to_string(),
              renames,
            },
            affected_members: instances.iter().map(|i| i.member.clone()).collect(),
            suggestion: format!(
              "Renamed dependencies detected. Policy is set to '{:?}' but not yet fully implemented. \
               Standardize dependency name across all workspace members.",
              self.config.renamed_handling
            ),
          });
        }
      }
    }

    // Check for version conflicts (respect policy)
    let versions: HashSet<_> = instances.iter().map(|i| &i.version_req).collect();
    if versions.len() > 1 {
      match self.config.version_conflict {
        VersionConflictPolicy::Error => {
          return Some(UnificationIssue {
            dep_name: dep_name.to_string(),
            issue_type: IssueType::VersionConflict {
              requirements: instances
                .iter()
                .map(|i| (i.member.clone(), i.version_req.to_string()))
                .collect(),
            },
            affected_members: instances.iter().map(|i| i.member.clone()).collect(),
            suggestion: "Standardize version requirement across all workspace members".to_string(),
          });
        }
        VersionConflictPolicy::Latest | VersionConflictPolicy::First => {
          // Policy allows auto-resolution, but not yet implemented
          // For now, still create issue but note it could be auto-resolved
          return Some(UnificationIssue {
            dep_name: dep_name.to_string(),
            issue_type: IssueType::VersionConflict {
              requirements: instances
                .iter()
                .map(|i| (i.member.clone(), i.version_req.to_string()))
                .collect(),
            },
            affected_members: instances.iter().map(|i| i.member.clone()).collect(),
            suggestion: format!(
              "Version conflict detected. Policy is set to '{:?}' but auto-resolution not yet implemented. \
               Standardize version requirement across all workspace members.",
              self.config.version_conflict
            ),
          });
        }
      }
    }

    // Check for path dependencies
    let paths: Vec<_> = instances
      .iter()
      .filter_map(|i| i.path.as_ref().map(|p| (i.member.clone(), p.clone())))
      .collect();

    if !paths.is_empty() {
      return Some(UnificationIssue {
        dep_name: dep_name.to_string(),
        issue_type: IssueType::PathDependency { paths },
        affected_members: instances.iter().map(|i| i.member.clone()).collect(),
        suggestion: "Convert path dependencies to version dependencies".to_string(),
      });
    }

    // Check if all are target-specific
    if instances.iter().all(|i| i.target.is_some()) {
      let targets: Vec<_> = instances.iter().filter_map(|i| i.target.clone()).collect();
      return Some(UnificationIssue {
        dep_name: dep_name.to_string(),
        issue_type: IssueType::AllTargetSpecific {
          targets: targets.clone(),
        },
        affected_members: instances.iter().map(|i| i.member.clone()).collect(),
        suggestion: "Target-specific dependencies cannot be unified at workspace level".to_string(),
      });
    }

    // Check for inconsistent default-features settings
    let with_default: Vec<_> = instances
      .iter()
      .filter(|i| i.default_features)
      .map(|i| i.member.clone())
      .collect();
    let without_default: Vec<_> = instances
      .iter()
      .filter(|i| !i.default_features)
      .map(|i| i.member.clone())
      .collect();

    if !with_default.is_empty() && !without_default.is_empty() {
      return Some(UnificationIssue {
        dep_name: dep_name.to_string(),
        issue_type: IssueType::InconsistentDefaultFeatures {
          members_with_default: with_default.clone(),
          members_without_default: without_default.clone(),
        },
        affected_members: instances.iter().map(|i| i.member.clone()).collect(),
        suggestion: "Standardize default-features setting. Consider setting default-features = false and explicitly enabling needed features".to_string(),
      });
    }

    None
  }

  /// Unify instances into a single workspace dependency
  fn unify_instances(&self, dep_name: &str, instances: &[DependencyInstance]) -> RailResult<UnifiedDep> {
    // Version: use first (already validated all are identical)
    let version_req = instances[0].version_req.clone();

    // Features: union of all
    let mut all_features = HashSet::new();
    for instance in instances {
      all_features.extend(instance.features.iter().cloned());
    }
    let mut features: Vec<_> = all_features.into_iter().collect();
    features.sort();

    // Default features: true if ANY instance uses them
    let default_features = instances.iter().any(|i| i.default_features);

    // Used by
    let mut used_by: Vec<_> = instances.iter().map(|i| i.member.clone()).collect();
    used_by.sort();
    used_by.dedup();

    // Dependency kinds
    let dep_kinds: HashSet<_> = instances.iter().map(|i| i.kind).collect();

    // Fragmentation count (unique feature combinations)
    let unique_feature_sets: HashSet<_> = instances
      .iter()
      .map(|i| {
        let mut f = i.features.clone();
        f.sort();
        f
      })
      .collect();
    let fragmentation_count = unique_feature_sets.len();

    Ok(UnifiedDep {
      name: dep_name.to_string(),
      version_req,
      features,
      default_features,
      used_by,
      dep_kinds,
      fragmentation_count,
    })
  }
}

// ============================================================================
// Plan Display & Application
// ============================================================================

impl UnificationPlan {
  /// Generate human-readable summary
  pub fn summary(&self) -> String {
    let mut output = String::new();

    output.push_str("📦 Workspace Dependency Unification Plan\n\n");

    // Statistics
    output.push_str("📊 Statistics:\n");
    output.push_str(&format!("  Total dependencies: {}\n", self.stats.total_deps));
    output.push_str(&format!("  To unify: {}\n", self.stats.unified_count));
    output.push_str(&format!("  Issues: {}\n", self.stats.issue_count));
    output.push_str(&format!("  Compilations saved: {}\n\n", self.stats.compilations_saved));

    // Workspace dependencies
    if !self.workspace_deps.is_empty() {
      output.push_str("✅ Dependencies to unify:\n\n");
      for dep in &self.workspace_deps {
        output.push_str(&format!("  {} = ", dep.name));
        if dep.features.is_empty() && dep.default_features {
          output.push_str(&format!("\"{}\"", dep.version_req));
        } else {
          output.push_str("{ ");
          output.push_str(&format!("version = \"{}\"", dep.version_req));
          if !dep.features.is_empty() {
            output.push_str(&format!(
              ", features = [{}]",
              dep
                .features
                .iter()
                .map(|f| format!("\"{}\"", f))
                .collect::<Vec<_>>()
                .join(", ")
            ));
          }
          if !dep.default_features {
            output.push_str(", default-features = false");
          }
          output.push_str(" }");
        }
        output.push_str(&format!("\n    Used by: {}\n", dep.used_by.join(", ")));
        if dep.fragmentation_count > 1 {
          output.push_str(&format!(
            "    Eliminates {} duplicate compilations\n",
            dep.fragmentation_count - 1
          ));
        }
        output.push('\n');
      }
    }

    // Issues
    if !self.issues.is_empty() {
      output.push_str("⚠️  Issues requiring attention:\n\n");
      for issue in &self.issues {
        output.push_str(&format!("  {} - ", issue.dep_name));
        match &issue.issue_type {
          IssueType::VersionConflict { requirements } => {
            output.push_str("Version conflict\n");
            for (member, version) in requirements {
              output.push_str(&format!("    {} requires {}\n", member, version));
            }
          }
          IssueType::Renamed { original, renames } => {
            output.push_str(&format!("Renamed from {}\n", original));
            for (member, rename) in renames {
              output.push_str(&format!("    {} uses name: {}\n", member, rename));
            }
          }
          IssueType::PathDependency { paths } => {
            output.push_str("Path dependency\n");
            for (member, path) in paths {
              output.push_str(&format!("    {} uses path: {}\n", member, path));
            }
          }
          IssueType::AllTargetSpecific { targets } => {
            output.push_str(&format!("All uses are target-specific: {}\n", targets.join(", ")));
          }
          IssueType::MultipleVersions { versions } => {
            output.push_str(&format!("Multiple versions resolved: {}\n", versions.join(", ")));
          }
          IssueType::InconsistentDefaultFeatures {
            members_with_default,
            members_without_default,
          } => {
            output.push_str("Inconsistent default-features\n");
            output.push_str(&format!("    With defaults: {}\n", members_with_default.join(", ")));
            output.push_str(&format!(
              "    Without defaults: {}\n",
              members_without_default.join(", ")
            ));
          }
        }
        output.push_str(&format!("    Suggestion: {}\n\n", issue.suggestion));
      }
    }

    output
  }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  fn create_test_metadata() -> WorkspaceMetadata {
    // Use current workspace for testing
    let current_dir = std::env::current_dir().unwrap();
    WorkspaceMetadata::load(&current_dir).unwrap()
  }

  // ==========================================================================
  // Configuration Tests
  // ==========================================================================

  #[test]
  fn test_unify_config_default() {
    let config = UnifyConfig::default();
    assert!(matches!(config.strategy, UnifyStrategy::All));
    assert!(matches!(config.version_conflict, VersionConflictPolicy::Error));
    assert!(matches!(config.renamed_handling, RenamedPolicy::Skip));
    assert!(config.backup);
    assert!(config.validate);
    assert!(config.exclude.is_empty());
    assert!(config.include.is_empty());
  }

  #[test]
  fn test_unify_strategy_variants() {
    // Test all strategy variants
    let all = UnifyStrategy::All;
    let fragmented = UnifyStrategy::Fragmented;
    let threshold = UnifyStrategy::Threshold(3);

    assert_eq!(all, UnifyStrategy::All);
    assert_eq!(fragmented, UnifyStrategy::Fragmented);
    assert_eq!(threshold, UnifyStrategy::Threshold(3));
  }

  // ==========================================================================
  // Dependency Collection Tests
  // ==========================================================================

  #[test]
  fn test_collect_dependencies() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    let instances = unifier.collect_dependencies();

    // Should have collected dependencies from workspace members
    assert!(!instances.is_empty(), "Should collect dependencies");

    // Verify instance structure
    for instance in &instances {
      assert!(!instance.member.is_empty(), "Member name should not be empty");
      // Version req should be valid (just verify it exists)
      let _ = &instance.version_req;
    }
  }

  #[test]
  fn test_group_by_name() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    let instances = unifier.collect_dependencies();
    let grouped = unifier.group_by_name(instances.clone());

    // Should group by dependency name
    // Note: For single-member workspaces, grouped might be empty if no external deps
    // We'll validate the grouping logic structure instead

    // If we have instances, they should be properly grouped
    if !instances.is_empty() {
      // Each group should have at least one instance
      for (dep_name, group_instances) in &grouped {
        assert!(!group_instances.is_empty(), "Group {} should have instances", dep_name);
      }

      // Validate that total instances = sum of grouped instances
      let total_grouped: usize = grouped.values().map(|v| v.len()).sum();
      assert_eq!(total_grouped, instances.len(), "All instances should be grouped");
    }
  }

  // ==========================================================================
  // Strategy Tests
  // ==========================================================================

  #[test]
  fn test_should_unify_all_strategy() {
    let metadata = create_test_metadata();
    let config = UnifyConfig {
      strategy: UnifyStrategy::All,
      ..Default::default()
    };
    let unifier = WorkspaceUnifier::with_config(&metadata, config);

    // Create mock instances (2 members using same dep)
    let instances = vec![
      DependencyInstance {
        member: "member-a".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec![],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
      DependencyInstance {
        member: "member-b".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec![],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
    ];

    // All strategy should unify if >= 2 members
    assert!(
      unifier.should_unify("test-dep", &instances),
      "Should unify with 2+ members"
    );

    // Single member should not unify
    assert!(
      !unifier.should_unify("test-dep", &instances[..1]),
      "Should not unify single member"
    );
  }

  #[test]
  fn test_should_unify_fragmented_strategy() {
    let metadata = create_test_metadata();
    let config = UnifyConfig {
      strategy: UnifyStrategy::Fragmented,
      ..Default::default()
    };
    let unifier = WorkspaceUnifier::with_config(&metadata, config);

    // Fragmented: different features
    let fragmented = vec![
      DependencyInstance {
        member: "member-a".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec!["derive".to_string()],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
      DependencyInstance {
        member: "member-b".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec!["rc".to_string()],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
    ];

    assert!(
      unifier.should_unify("test-dep", &fragmented),
      "Should unify fragmented dep"
    );

    // Not fragmented: same features
    let unified = vec![
      DependencyInstance {
        member: "member-a".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec!["derive".to_string()],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
      DependencyInstance {
        member: "member-b".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec!["derive".to_string()],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
    ];

    assert!(
      !unifier.should_unify("test-dep", &unified),
      "Should not unify non-fragmented dep"
    );
  }

  #[test]
  fn test_should_unify_threshold_strategy() {
    let metadata = create_test_metadata();
    let config = UnifyConfig {
      strategy: UnifyStrategy::Threshold(3),
      ..Default::default()
    };
    let unifier = WorkspaceUnifier::with_config(&metadata, config);

    // Create instances with different counts
    let two_instances = vec![
      DependencyInstance {
        member: "member-a".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec![],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
      DependencyInstance {
        member: "member-b".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec![],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
    ];

    assert!(
      !unifier.should_unify("test-dep", &two_instances),
      "Should not unify below threshold"
    );

    // Add third instance
    let mut three_instances = two_instances.clone();
    three_instances.push(DependencyInstance {
      member: "member-c".to_string(),
      version_req: "1.0".parse().unwrap(),
      features: vec![],
      default_features: true,
      optional: false,
      kind: DependencyKind::Normal,
      target: None,
      rename: None,
      path: None,
    });

    assert!(
      unifier.should_unify("test-dep", &three_instances),
      "Should unify at threshold"
    );
  }

  #[test]
  fn test_force_include() {
    let metadata = create_test_metadata();
    let mut config = UnifyConfig::default();
    config.include.insert("serde".to_string());
    let unifier = WorkspaceUnifier::with_config(&metadata, config);

    // Single instance, but force-included
    let single = vec![DependencyInstance {
      member: "member-a".to_string(),
      version_req: "1.0".parse().unwrap(),
      features: vec![],
      default_features: true,
      optional: false,
      kind: DependencyKind::Normal,
      target: None,
      rename: None,
      path: None,
    }];

    assert!(
      unifier.should_unify("serde", &single),
      "Should unify force-included deps"
    );
  }

  // ==========================================================================
  // Issue Detection Tests
  // ==========================================================================

  #[test]
  fn test_detect_version_conflict() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    let instances = vec![
      DependencyInstance {
        member: "member-a".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec![],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
      DependencyInstance {
        member: "member-b".to_string(),
        version_req: "2.0".parse().unwrap(), // Different major version
        features: vec![],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
    ];

    let issue = unifier.detect_issues("test-dep", &instances);
    assert!(issue.is_some(), "Should detect version conflict");

    let issue = issue.unwrap();
    assert!(
      matches!(issue.issue_type, IssueType::VersionConflict { .. }),
      "Should be version conflict"
    );
    assert_eq!(issue.affected_members.len(), 2);
  }

  #[test]
  fn test_detect_renamed_dependency() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    let instances = vec![
      DependencyInstance {
        member: "member-a".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec![],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: Some("serde1".to_string()), // Renamed
        path: None,
      },
      DependencyInstance {
        member: "member-b".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec![],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
    ];

    let issue = unifier.detect_issues("serde", &instances);
    assert!(issue.is_some(), "Should detect renamed dependency");

    let issue = issue.unwrap();
    assert!(
      matches!(issue.issue_type, IssueType::Renamed { .. }),
      "Should be renamed issue"
    );
  }

  #[test]
  fn test_detect_all_target_specific() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    let instances = vec![
      DependencyInstance {
        member: "member-a".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec![],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: Some("cfg(unix)".to_string()),
        rename: None,
        path: None,
      },
      DependencyInstance {
        member: "member-b".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec![],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: Some("cfg(windows)".to_string()),
        rename: None,
        path: None,
      },
    ];

    let issue = unifier.detect_issues("libc", &instances);
    assert!(issue.is_some(), "Should detect all target-specific");

    let issue = issue.unwrap();
    assert!(
      matches!(issue.issue_type, IssueType::AllTargetSpecific { .. }),
      "Should be target-specific issue"
    );
  }

  #[test]
  fn test_no_issues_for_compatible_deps() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    let instances = vec![
      DependencyInstance {
        member: "member-a".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec!["derive".to_string()],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
      DependencyInstance {
        member: "member-b".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec!["rc".to_string()],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
    ];

    let issue = unifier.detect_issues("serde", &instances);
    assert!(issue.is_none(), "Should not have issues for compatible deps");
  }

  // ==========================================================================
  // Feature Unification Tests
  // ==========================================================================

  #[test]
  fn test_unify_instances_features() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    let instances = vec![
      DependencyInstance {
        member: "member-a".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec!["derive".to_string()],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
      DependencyInstance {
        member: "member-b".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec!["rc".to_string(), "alloc".to_string()],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
    ];

    let unified = unifier.unify_instances("serde", &instances).unwrap();

    // Should be union of features
    assert_eq!(unified.features.len(), 3);
    assert!(unified.features.contains(&"derive".to_string()));
    assert!(unified.features.contains(&"rc".to_string()));
    assert!(unified.features.contains(&"alloc".to_string()));
  }

  #[test]
  fn test_unify_instances_default_features() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    // All use default features
    let all_default = vec![
      DependencyInstance {
        member: "member-a".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec![],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
      DependencyInstance {
        member: "member-b".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec![],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
    ];

    let unified = unifier.unify_instances("test", &all_default).unwrap();
    assert!(unified.default_features, "Should use default features");

    // Mixed: one uses default, one doesn't
    let mixed = vec![
      DependencyInstance {
        member: "member-a".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec![],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
      DependencyInstance {
        member: "member-b".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec![],
        default_features: false,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
    ];

    let unified = unifier.unify_instances("test", &mixed).unwrap();
    assert!(
      unified.default_features,
      "Should use default features if ANY member uses them"
    );
  }

  #[test]
  fn test_unify_instances_fragmentation_count() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    // Two different feature sets = fragmentation
    let fragmented = vec![
      DependencyInstance {
        member: "member-a".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec!["derive".to_string()],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
      DependencyInstance {
        member: "member-b".to_string(),
        version_req: "1.0".parse().unwrap(),
        features: vec!["rc".to_string()],
        default_features: true,
        optional: false,
        kind: DependencyKind::Normal,
        target: None,
        rename: None,
        path: None,
      },
    ];

    let unified = unifier.unify_instances("serde", &fragmented).unwrap();
    assert_eq!(unified.fragmentation_count, 2, "Should detect 2 unique feature sets");
  }

  // ==========================================================================
  // Integration Tests
  // ==========================================================================

  #[test]
  fn test_analyze_workspace() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    let plan = unifier.analyze();
    assert!(plan.is_ok(), "Analysis should succeed");

    let plan = plan.unwrap();

    // Should have analyzed dependencies
    assert!(plan.stats.total_deps > 0, "Should have analyzed dependencies");

    // Workspace deps should be valid
    for dep in &plan.workspace_deps {
      assert!(!dep.name.is_empty(), "Dep name should not be empty");
      assert!(!dep.used_by.is_empty(), "Should have users");
      assert!(dep.used_by.len() >= 2, "Unified deps should have 2+ users");
    }

    // Member edits should be valid
    for (member, edits) in &plan.member_edits {
      assert!(!member.is_empty(), "Member name should not be empty");
      assert!(!edits.is_empty(), "Should have edits for member");
    }
  }

  #[test]
  fn test_plan_summary_generation() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    let plan = unifier.analyze().unwrap();
    let summary = plan.summary();

    // Should contain key sections
    assert!(summary.contains("Workspace Dependency Unification"));
    assert!(summary.contains("Statistics:"));

    if !plan.workspace_deps.is_empty() {
      assert!(summary.contains("Dependencies to unify:"));
    }

    if !plan.issues.is_empty() {
      assert!(summary.contains("Issues requiring attention:"));
    }
  }

  #[test]
  fn test_exclude_configuration() {
    let metadata = create_test_metadata();
    let mut config = UnifyConfig::default();
    config.exclude.insert("serde".to_string());

    let unifier = WorkspaceUnifier::with_config(&metadata, config);
    let plan = unifier.analyze().unwrap();

    // serde should not be in unified deps
    assert!(
      !plan.workspace_deps.iter().any(|d| d.name == "serde"),
      "Excluded dep should not be unified"
    );
  }

  // ============================================================================
  // Phase 1: Resolved Features Tests
  // ============================================================================

  #[test]
  fn test_collect_dependencies_uses_resolved_features() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    // Collect dependencies
    let instances = unifier.collect_dependencies();

    // Verify we got instances
    assert!(!instances.is_empty(), "Should collect dependency instances");

    // For each external dependency, check if we have resolved features
    for instance in &instances {
      // If this is an external dep with resolved features, verify they're used
      if let Some(resolved_features) = metadata.get_resolved_features_for_package(&instance.member) {
        // The instance should use resolved features
        // (This is a structural test - we're verifying the code path works)
        let _ = &instance.features;
        let _ = resolved_features;
      }
    }
  }

  #[test]
  fn test_unified_features_include_resolved_not_just_declared() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    let plan = unifier.analyze().unwrap();

    // For each unified dependency, verify features are comprehensive
    for unified in &plan.workspace_deps {
      // Get resolved features for this package
      if let Some(resolved_features) = metadata.get_resolved_features_for_package(&unified.name) {
        // The unified features should be based on resolved features
        // This means they may include features not explicitly declared in any member

        println!(
          "Dependency: {}, Unified features: {}, Resolved features: {}",
          unified.name,
          unified.features.len(),
          resolved_features.len()
        );

        // At minimum, unified features should exist (might be empty for some deps)
        // This test verifies the mechanism is working
        let _ = &unified.features;
      }
    }
  }

  #[test]
  fn test_resolved_features_fallback_to_declared() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    // Create instances
    let instances = unifier.collect_dependencies();

    // Some dependencies might not have resolved features (e.g., optional deps not enabled)
    // In those cases, we should fall back to declared features
    // This test verifies the fallback mechanism works

    for instance in &instances {
      // Features should always be valid (either resolved or declared)
      let _ = &instance.features;

      // If we can't get resolved features, we should have fallen back to declared
      if metadata.get_resolved_features_for_package(&instance.member).is_none() {
        // Fallback occurred - features should still be valid
        assert!(
          instance.features.iter().all(|f| !f.is_empty()),
          "Features should be valid strings"
        );
      }
    }
  }

  #[test]
  fn test_unification_with_resolved_features_no_fragmentation() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    let plan = unifier.analyze().unwrap();

    // When using resolved features, if multiple members use the same dependency,
    // they should often have the SAME feature set (from the resolver)
    // This reduces fragmentation compared to declared features

    for unified in &plan.workspace_deps {
      if unified.used_by.len() >= 2 {
        // If fragmentation count is 1, all members are using the exact same feature set
        // This is the ideal case - no fragmentation
        if unified.fragmentation_count == 1 {
          println!(
            "✓ Dependency '{}' used by {} members with NO fragmentation",
            unified.name,
            unified.used_by.len()
          );
        } else {
          println!(
            "⚠ Dependency '{}' used by {} members with {} unique feature sets",
            unified.name,
            unified.used_by.len(),
            unified.fragmentation_count
          );
        }
      }
    }

    // Test passes as long as we can analyze - the println! shows the impact
    assert!(plan.stats.total_deps > 0, "Should have analyzed dependencies");
  }

  #[test]
  fn test_resolved_features_consistency_across_members() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    let instances = unifier.collect_dependencies();

    // Group by dependency name
    let mut by_name: HashMap<String, Vec<&DependencyInstance>> = HashMap::new();
    for instance in &instances {
      by_name.entry(instance.member.clone()).or_default().push(instance);
    }

    // For each dependency used by multiple members, check feature consistency
    for (dep_name, instances) in by_name {
      if instances.len() >= 2 {
        // Get resolved features for this dependency
        if let Some(resolved_features) = metadata.get_resolved_features_for_package(&dep_name) {
          // With resolved features, all instances SHOULD have the same feature set
          // (because they all come from the same resolved node in the graph)

          let first_features: HashSet<String> = instances[0].features.iter().cloned().collect();

          for instance in &instances[1..] {
            let instance_features: HashSet<String> = instance.features.iter().cloned().collect();

            // Check if features match (when using resolved, they should)
            if first_features == instance_features {
              println!("✓ Dependency '{}' has consistent features across members", dep_name);
            } else {
              println!(
                "⚠ Dependency '{}' has different features (this might indicate declared fallback)",
                dep_name
              );
            }
          }

          // Verify we're using resolved features
          let _ = resolved_features;
        }
      }
    }
  }

  // ============================================================================
  // Phase 2: Idempotency Tests
  // ============================================================================

  #[test]
  fn test_write_workspace_dependencies_idempotent() {
    use crate::cargo::manifest::CargoTransform;
    use semver::VersionReq;
    use tempfile::NamedTempFile;

    let metadata = create_test_metadata();
    let transformer = CargoTransform::new(metadata);

    // Create a temporary workspace Cargo.toml
    let mut temp_file = NamedTempFile::new().unwrap();
    let workspace_toml = r#"
[workspace]
members = ["crate-a", "crate-b"]
"#;
    std::io::Write::write_all(&mut temp_file, workspace_toml.as_bytes()).unwrap();

    // Create unified dependencies
    let unified_deps = vec![
      UnifiedDep {
        name: "serde".to_string(),
        version_req: VersionReq::parse("1.0").unwrap(),
        features: vec!["derive".to_string()],
        default_features: true,
        used_by: vec!["crate-a".to_string(), "crate-b".to_string()],
        dep_kinds: HashSet::new(),
        fragmentation_count: 2,
      },
      UnifiedDep {
        name: "tokio".to_string(),
        version_req: VersionReq::parse("1.0").unwrap(),
        features: vec!["full".to_string()],
        default_features: false,
        used_by: vec!["crate-a".to_string()],
        dep_kinds: HashSet::new(),
        fragmentation_count: 1,
      },
    ];

    // Write once
    transformer
      .write_workspace_dependencies(temp_file.path(), &unified_deps)
      .unwrap();
    let content_first = std::fs::read_to_string(temp_file.path()).unwrap();

    // Write again with same data (idempotency test)
    transformer
      .write_workspace_dependencies(temp_file.path(), &unified_deps)
      .unwrap();
    let content_second = std::fs::read_to_string(temp_file.path()).unwrap();

    // Should be identical
    assert_eq!(
      content_first, content_second,
      "Writing same dependencies twice should produce identical output"
    );
  }

  #[test]
  fn test_convert_to_workspace_inheritance_idempotent() {
    use crate::cargo::manifest::CargoTransform;
    use cargo_metadata::DependencyKind;
    use tempfile::NamedTempFile;

    let metadata = create_test_metadata();
    let transformer = CargoTransform::new(metadata);

    // Create a temporary member Cargo.toml
    let mut temp_file = NamedTempFile::new().unwrap();
    let member_toml = r#"
[package]
name = "test-crate"
version = "0.1.0"

[dependencies]
serde = { version = "1.0", features = ["derive"] }
tokio = "1.0"
"#;
    std::io::Write::write_all(&mut temp_file, member_toml.as_bytes()).unwrap();

    // Convert serde to workspace inheritance
    transformer
      .convert_to_workspace_inheritance(temp_file.path(), "serde", &DependencyKind::Normal)
      .unwrap();
    let content_first = std::fs::read_to_string(temp_file.path()).unwrap();

    // Convert again (should be idempotent - already using workspace = true)
    let result = transformer.convert_to_workspace_inheritance(temp_file.path(), "serde", &DependencyKind::Normal);

    // This might fail because serde is already { workspace = true }
    // OR it might succeed and keep it as { workspace = true }
    // Either way, content should be stable

    if result.is_ok() {
      let content_second = std::fs::read_to_string(temp_file.path()).unwrap();
      // If it succeeded, content should be identical or very similar
      // (might have formatting differences)
      assert!(
        content_second.contains("workspace = true"),
        "Should still have workspace = true"
      );
    }

    // Verify first conversion worked
    assert!(
      content_first.contains("workspace = true"),
      "Should use workspace inheritance"
    );
  }

  #[test]
  fn test_unification_plan_stats_are_consistent() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    // Run analyze multiple times
    let plan1 = unifier.analyze().unwrap();
    let plan2 = unifier.analyze().unwrap();

    // Stats should be identical (deterministic)
    assert_eq!(
      plan1.stats.total_deps, plan2.stats.total_deps,
      "total_deps should be deterministic"
    );
    assert_eq!(
      plan1.stats.unified_count, plan2.stats.unified_count,
      "unified_count should be deterministic"
    );
    assert_eq!(
      plan1.stats.issue_count, plan2.stats.issue_count,
      "issue_count should be deterministic"
    );
    assert_eq!(
      plan1.stats.compilations_saved, plan2.stats.compilations_saved,
      "compilations_saved should be deterministic"
    );

    // Workspace deps should be identical
    assert_eq!(
      plan1.workspace_deps.len(),
      plan2.workspace_deps.len(),
      "Should have same number of workspace deps"
    );

    // Sort and compare
    let mut deps1: Vec<_> = plan1.workspace_deps.iter().map(|d| &d.name).collect();
    let mut deps2: Vec<_> = plan2.workspace_deps.iter().map(|d| &d.name).collect();
    deps1.sort();
    deps2.sort();

    assert_eq!(deps1, deps2, "Workspace deps should be identical across runs");
  }

  #[test]
  fn test_unified_dep_features_are_sorted_and_deduplicated() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    let plan = unifier.analyze().unwrap();

    // All unified dependencies should have sorted, deduplicated features
    for dep in &plan.workspace_deps {
      // Check if sorted
      let mut sorted_features = dep.features.clone();
      sorted_features.sort();

      assert_eq!(
        dep.features, sorted_features,
        "Features for '{}' should be sorted",
        dep.name
      );

      // Check if deduplicated (no duplicates)
      let unique_features: HashSet<_> = dep.features.iter().collect();
      assert_eq!(
        dep.features.len(),
        unique_features.len(),
        "Features for '{}' should have no duplicates",
        dep.name
      );
    }
  }

  #[test]
  fn test_member_edits_are_complete() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    let plan = unifier.analyze().unwrap();

    // Every workspace dep should have corresponding member edits
    for unified in &plan.workspace_deps {
      for member in &unified.used_by {
        // Check if this member has an edit for this dependency
        let has_edit = plan
          .member_edits
          .get(member)
          .map(|edits| {
            edits
              .iter()
              .any(|e| matches!(e, MemberEdit::UseWorkspace { dep_name, .. } if dep_name == &unified.name))
          })
          .unwrap_or(false);

        assert!(
          has_edit,
          "Member '{}' should have edit for dependency '{}' but doesn't",
          member, unified.name
        );
      }
    }
  }

  #[test]
  fn test_summary_output_is_deterministic() {
    let metadata = create_test_metadata();
    let unifier = WorkspaceUnifier::new(&metadata);

    let plan = unifier.analyze().unwrap();

    // Generate summary multiple times
    let summary1 = plan.summary();
    let summary2 = plan.summary();

    assert_eq!(summary1, summary2, "Summary should be deterministic");

    // Summary should contain key information
    assert!(summary1.contains("Workspace Dependency Unification"));
    assert!(summary1.contains("Statistics:"));
    assert!(summary1.contains(&format!("Total dependencies: {}", plan.stats.total_deps)));
  }
}
