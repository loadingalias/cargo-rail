//! Core types for dependency unification

use cargo_metadata::{DependencyKind, camino::Utf8PathBuf};
use semver::VersionReq;
use std::collections::{HashMap, HashSet};

/// A single instance of a dependency in a workspace member
#[derive(Debug, Clone)]
pub struct DependencyInstance {
  /// Workspace member using this dependency
  pub member: String,

  /// Dependency package name
  pub name: String,

  /// Version requirement
  pub version_req: VersionReq,

  /// Enabled features (uses resolved features from dependency graph)
  pub features: Vec<String>,

  /// Uses default features
  pub default_features: bool,

  /// Optional dependency
  ///
  /// Note: Collected for completeness. The `optional` flag is preserved during
  /// conversion to workspace inheritance (see manifest.rs convert_to_workspace_inheritance).
  #[allow(dead_code)]
  pub optional: bool,

  /// Dependency kind (normal, dev, build)
  pub kind: DependencyKind,

  /// Target-specific (e.g., cfg(unix))
  pub target: Option<String>,

  /// Renamed from original name
  pub rename: Option<String>,

  /// Path dependency (if any)
  pub path: Option<Utf8PathBuf>,
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
  pub dep_kinds: HashSet<DependencyKind>,

  /// Number of unique feature sets (fragmentation indicator)
  pub fragmentation_count: usize,

  /// Path dependency (workspace-relative path for workspace member deps)
  pub path: Option<Utf8PathBuf>,
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

  /// Transitive-only crates with fragmented feature sets (informational)
  pub transitive_fragmentations: Vec<super::transitive::TransitiveFragmentation>,

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
  MultipleVersions {
    versions: Vec<String>, // Resolved versions
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
