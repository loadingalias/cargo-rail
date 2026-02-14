//! Change detection configuration.
//!
//! This config is consumed by planner file classification and custom surfaces.

use crate::error::ConfigError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Confidence profile for planner safety behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceProfile {
  /// Most conservative behavior; expands package-scoped execution.
  Strict,
  /// Default trade-off between safety and speed.
  #[default]
  Balanced,
  /// Fastest behavior; minimizes conservative expansion.
  Fast,
}

/// Configuration for planner change detection.
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

  /// When true, unclassified crate-owned files conservatively enable build+test surfaces.
  ///
  /// Set to false to keep aggressive behavior for unknown file kinds.
  #[serde(default = "default_conservative_unclassified_owner_fallback")]
  pub conservative_unclassified_owner_fallback: bool,

  /// Confidence profile used by planner unless explicitly overridden by CLI.
  #[serde(default)]
  pub confidence_profile: ConfidenceProfile,

  /// Optional confidence profile override for bot-authored pull requests.
  ///
  /// When set, planner applies this profile only in bot-authored PR contexts.
  #[serde(default)]
  pub bot_pr_confidence_profile: Option<ConfidenceProfile>,
}

impl Default for ChangeDetectionConfig {
  fn default() -> Self {
    Self {
      infrastructure: default_infrastructure_patterns(),
      custom: HashMap::new(),
      conservative_unclassified_owner_fallback: default_conservative_unclassified_owner_fallback(),
      confidence_profile: ConfidenceProfile::default(),
      bot_pr_confidence_profile: None,
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

    // Validate custom category names + patterns
    for (category, patterns) in &self.custom {
      if !is_valid_custom_category(category) {
        return Err(ConfigError::InvalidValue {
          field: format!("change-detection.custom.{}", category),
          message: "invalid category name; use ASCII letters, digits, '_' or '-' (cannot start with 'custom:')"
            .to_string(),
        });
      }

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

fn default_conservative_unclassified_owner_fallback() -> bool {
  true
}

fn is_valid_custom_category(category: &str) -> bool {
  if category.is_empty() || category.starts_with("custom:") {
    return false;
  }

  category
    .bytes()
    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
  use super::{ChangeDetectionConfig, ConfidenceProfile};
  use std::collections::HashMap;

  #[test]
  fn test_validate_accepts_valid_custom_category_names() {
    let mut custom = HashMap::new();
    custom.insert("verify_models".to_string(), vec!["verify/**".to_string()]);
    custom.insert("bench-extended".to_string(), vec!["perf/**".to_string()]);
    let cfg = ChangeDetectionConfig {
      infrastructure: vec![".github/**".to_string()],
      custom,
      conservative_unclassified_owner_fallback: true,
      confidence_profile: ConfidenceProfile::Balanced,
      bot_pr_confidence_profile: None,
    };
    assert!(cfg.validate().is_ok());
  }

  #[test]
  fn test_validate_rejects_invalid_custom_category_names() {
    let mut custom = HashMap::new();
    custom.insert("custom:verify".to_string(), vec!["verify/**".to_string()]);
    let cfg = ChangeDetectionConfig {
      infrastructure: vec![".github/**".to_string()],
      custom,
      conservative_unclassified_owner_fallback: true,
      confidence_profile: ConfidenceProfile::Balanced,
      bot_pr_confidence_profile: None,
    };
    assert!(cfg.validate().is_err());
  }
}
