//! Unification strategy logic

use super::config::UnifyConfig;
use super::types::DependencyInstance;
use cargo_metadata::DependencyKind;

/// Check if dependency should be unified based on strategy
pub fn should_unify(dep_name: &str, instances: &[DependencyInstance], config: &UnifyConfig) -> bool {
  // Force-included dependencies always unify
  if config.include.contains(dep_name) {
    return true;
  }

  // Filter by dependency kind if normal_only is enabled
  if config.normal_only {
    // Check if ANY instance is non-normal (dev or build)
    if instances.iter().any(|i| i.kind != DependencyKind::Normal) {
      return false;
    }
  }

  // Unify if used by 2+ members (UnifyStrategy::All)
  instances.len() >= 2
}
