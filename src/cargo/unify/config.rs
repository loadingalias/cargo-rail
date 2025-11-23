//! Configuration for workspace dependency unification

use crate::config::{RailConfig, TransitiveFeatureHost, UnifyBackupConfig, UnifyOutputConfig};
use std::collections::HashSet;

/// Configuration for workspace unification
#[derive(Debug, Clone, Default)]
pub struct UnifyConfig {
  /// Allow renamed dependencies to be unified (default: false)
  pub allow_renamed: bool,

  /// Dependencies to exclude from unification
  pub exclude: HashSet<String>,

  /// Dependencies to force-include in unification
  pub include: HashSet<String>,

  /// Consolidate transitive-only crates with fragmented features (default: false)
  pub consolidate_transitives: bool,

  /// Where to add dev-dependencies when consolidating transitive features
  pub transitive_host: TransitiveFeatureHost,

  /// Output configuration
  pub output: UnifyOutputConfig,

  /// Backup configuration
  pub backup: UnifyBackupConfig,
}

impl From<&RailConfig> for UnifyConfig {
  fn from(rail_config: &RailConfig) -> Self {
    Self {
      allow_renamed: rail_config.unify.allow_renamed,
      exclude: rail_config.unify.exclude.iter().cloned().collect(),
      include: rail_config.unify.include.iter().cloned().collect(),
      consolidate_transitives: rail_config.unify.consolidate_transitives,
      transitive_host: rail_config.unify.transitive_host.clone(),
      output: rail_config.unify.output.clone(),
      backup: rail_config.unify.backup.clone(),
    }
  }
}
