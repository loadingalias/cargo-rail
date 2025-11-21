//! Unification strategy logic

use super::config::UnifyConfig;
use super::types::DependencyInstance;

/// Check if dependency should be unified based on strategy
pub fn should_unify(dep_name: &str, instances: &[DependencyInstance], config: &UnifyConfig) -> bool {
  // Force-included dependencies always unify
  if config.include.contains(dep_name) {
    return true;
  }

  // Unify if used by 2+ members (UnifyStrategy::All)
  instances.len() >= 2
}
