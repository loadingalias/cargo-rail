//! Change detection configuration.
//!
//! This config is consumed by planner file classification and custom surfaces.

use crate::error::ConfigError;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Deserializer, Serialize};

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

/// Policy for unknown file kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UnknownFilePolicy {
    /// Treat unknown files as docs-only.
    Docs,
    /// Crate-owned unknown files enable build+test; everything else stays docs-only.
    OwnedBuildTest,
    /// Workspace-scoped unknown files enable infra; everything else stays docs-only.
    WorkspaceInfra,
    /// Crate-owned unknown files enable build+test and non-crate unknown files enable infra.
    #[default]
    Strict,
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

    /// Policy for unknown file kinds.
    ///
    /// Accepts legacy boolean values for backward compatibility:
    /// - `true`  => `owned_build_test`
    /// - `false` => `docs`
    #[serde(
        default = "default_unknown_file_policy",
        alias = "conservative_unclassified_owner_fallback",
        deserialize_with = "deserialize_unknown_file_policy"
    )]
    pub unknown_file_policy: UnknownFilePolicy,

    /// Confidence profile used by planner unless explicitly overridden by CLI.
    #[serde(default)]
    pub confidence_profile: ConfidenceProfile,
}

impl Default for ChangeDetectionConfig {
    fn default() -> Self {
        Self {
            infrastructure: default_infrastructure_patterns(),
            custom: FxHashMap::default(),
            unknown_file_policy: default_unknown_file_policy(),
            confidence_profile: ConfidenceProfile::default(),
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
                    message:
                        "invalid category name; use ASCII letters, digits, '_' or '-' (cannot start with 'custom:')"
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

fn default_unknown_file_policy() -> UnknownFilePolicy {
    UnknownFilePolicy::Strict
}

fn deserialize_unknown_file_policy<'de, D>(deserializer: D) -> Result<UnknownFilePolicy, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum UnknownFilePolicyInput {
        Bool(bool),
        Policy(UnknownFilePolicy),
    }

    match UnknownFilePolicyInput::deserialize(deserializer)? {
        UnknownFilePolicyInput::Bool(true) => Ok(UnknownFilePolicy::OwnedBuildTest),
        UnknownFilePolicyInput::Bool(false) => Ok(UnknownFilePolicy::Docs),
        UnknownFilePolicyInput::Policy(policy) => Ok(policy),
    }
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
    use super::{ChangeDetectionConfig, ConfidenceProfile, FxHashMap, UnknownFilePolicy};

    #[test]
    fn test_validate_accepts_valid_custom_category_names() {
        let mut custom = FxHashMap::default();
        custom.insert("verify_models".to_string(), vec!["verify/**".to_string()]);
        custom.insert("bench-extended".to_string(), vec!["perf/**".to_string()]);
        let cfg = ChangeDetectionConfig {
            infrastructure: vec![".github/**".to_string()],
            custom,
            unknown_file_policy: UnknownFilePolicy::Strict,
            confidence_profile: ConfidenceProfile::Balanced,
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn test_validate_rejects_invalid_custom_category_names() {
        let mut custom = FxHashMap::default();
        custom.insert("custom:verify".to_string(), vec!["verify/**".to_string()]);
        let cfg = ChangeDetectionConfig {
            infrastructure: vec![".github/**".to_string()],
            custom,
            unknown_file_policy: UnknownFilePolicy::Strict,
            confidence_profile: ConfidenceProfile::Balanced,
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_legacy_bool_unknown_file_policy_true_maps_to_owned_build_test() {
        let cfg: ChangeDetectionConfig = toml_edit::de::from_str(
            r#"
      conservative_unclassified_owner_fallback = true
      "#,
        )
        .expect("legacy bool config should parse");

        assert_eq!(cfg.unknown_file_policy, UnknownFilePolicy::OwnedBuildTest);
    }

    #[test]
    fn test_legacy_bool_unknown_file_policy_false_maps_to_docs() {
        let cfg: ChangeDetectionConfig = toml_edit::de::from_str(
            r#"
      conservative_unclassified_owner_fallback = false
      "#,
        )
        .expect("legacy bool config should parse");

        assert_eq!(cfg.unknown_file_policy, UnknownFilePolicy::Docs);
    }
}
