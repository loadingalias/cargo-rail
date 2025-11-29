//! Change detection configuration - controls `cargo rail affected` behavior

use crate::error::ConfigError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for change detection (`cargo rail affected`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeDetectionConfig {
  /// Glob patterns for infrastructure files that trigger rebuild_all
  /// Default: [".github/**", "scripts/**", "justfile", "Makefile", ...]
  #[serde(default = "default_infrastructure_patterns")]
  pub infrastructure: Vec<String>,

  /// Custom path patterns and their categories
  /// Example: verify = ["verify/**/*.rs"] for Stateright verification models
  #[serde(default)]
  pub custom: HashMap<String, Vec<String>>,
}

impl Default for ChangeDetectionConfig {
  fn default() -> Self {
    Self {
      infrastructure: default_infrastructure_patterns(),
      custom: HashMap::new(),
    }
  }
}

impl ChangeDetectionConfig {
  /// Validate all glob patterns in the configuration
  pub fn validate(&self) -> Result<(), ConfigError> {
    // Validate infrastructure patterns
    for pattern in &self.infrastructure {
      if let Err(e) = glob::Pattern::new(pattern) {
        return Err(ConfigError::InvalidGlobPattern {
          pattern: pattern.clone(),
          message: e.to_string(),
        });
      }
    }

    // Validate custom category patterns
    for (category, patterns) in &self.custom {
      for pattern in patterns {
        if let Err(e) = glob::Pattern::new(pattern) {
          return Err(ConfigError::InvalidGlobPattern {
            pattern: format!("{}: {}", category, pattern),
            message: e.to_string(),
          });
        }
      }
    }

    Ok(())
  }
}

fn default_infrastructure_patterns() -> Vec<String> {
  vec![
    ".github/**".to_string(),
    "scripts/**".to_string(),
    "justfile".to_string(),
    "Justfile".to_string(),
    "Makefile".to_string(),
    "makefile".to_string(),
    "GNUmakefile".to_string(),
    "*.sh".to_string(),
    "Taskfile.yml".to_string(),
    "Taskfile.yaml".to_string(),
    ".pre-commit-config.yaml".to_string(),
    "deny.toml".to_string(),
    "cliff.toml".to_string(),
    "release.toml".to_string(),
    "release-plz.toml".to_string(),
  ]
}
