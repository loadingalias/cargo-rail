//! Change detection configuration.
//!
//! This config is consumed by planner file classification and custom surfaces.

use crate::error::ConfigError;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

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
  pub custom: FxHashMap<String, Vec<String>>,

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
      custom: FxHashMap::default(),
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
  const PATTERNS: &[&str] = &[
    ".github/**",
    "scripts/**",
    "justfile",
    "Justfile",
    "Makefile",
    "makefile",
    "GNUmakefile",
    "*.sh",
    "Taskfile.yml",
    "Taskfile.yaml",
    ".pre-commit-config.yaml",
    "deny.toml",
    "cliff.toml",
    "release.toml",
    "release-plz.toml",
  ];
  PATTERNS.iter().map(|&s| String::from(s)).collect()
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
  use super::{ChangeDetectionConfig, ConfidenceProfile, FxHashMap};

  #[test]
  fn test_validate_accepts_valid_custom_category_names() {
    let mut custom = FxHashMap::default();
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
    let mut custom = FxHashMap::default();
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
