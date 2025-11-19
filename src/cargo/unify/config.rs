//! Configuration for workspace dependency unification

use std::collections::HashSet;

/// Configuration for workspace unification
#[derive(Debug, Clone)]
pub struct UnifyConfig {
  /// Strategy for selecting dependencies to unify
  ///
  /// Currently only UnifyStrategy::All is supported. This field is kept for
  /// future extensibility (e.g., threshold-based unification, fragmentation-only).
  #[allow(dead_code)]
  pub strategy: UnifyStrategy,

  /// Allow renamed dependencies to be unified (default: false)
  pub allow_renamed: bool,

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
}

impl Default for UnifyConfig {
  fn default() -> Self {
    Self {
      strategy: UnifyStrategy::All,
      allow_renamed: false,
      exclude: HashSet::new(),
      include: HashSet::new(),
      normal_only: false,
    }
  }
}
