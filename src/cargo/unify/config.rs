//! Configuration for workspace dependency unification

use crate::config::RailConfig;
use std::collections::HashSet;

/// Configuration for workspace unification
#[derive(Debug, Clone)]
pub struct UnifyConfig {
  /// Allow renamed dependencies to be unified (default: false)
  pub allow_renamed: bool,

  /// Dependencies to exclude from unification
  pub exclude: HashSet<String>,

  /// Dependencies to force-include in unification
  pub include: HashSet<String>,

  /// Auto-resolve version conflicts by picking the highest version (default: true)
  pub auto_resolve_version_conflicts: bool,

  /// Add conflict comments to workspace.dependencies (default: true)
  pub add_conflict_comments: bool,

  /// Generate a detailed unification report (default: true)
  pub generate_report: bool,
}

impl Default for UnifyConfig {
  fn default() -> Self {
    Self {
      allow_renamed: false,
      exclude: HashSet::new(),
      include: HashSet::new(),
      auto_resolve_version_conflicts: true,
      add_conflict_comments: true,
      generate_report: true,
    }
  }
}

impl From<&RailConfig> for UnifyConfig {
  fn from(rail_config: &RailConfig) -> Self {
    Self {
      allow_renamed: rail_config.unify.allow_renamed,
      exclude: rail_config.unify.exclude.iter().cloned().collect(),
      include: rail_config.unify.include.iter().cloned().collect(),
      auto_resolve_version_conflicts: rail_config.unify.conflicts.auto_resolve,
      add_conflict_comments: rail_config.unify.conflicts.add_markers,
      generate_report: rail_config.unify.output.generate_report,
    }
  }
}
