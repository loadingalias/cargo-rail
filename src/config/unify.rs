//! Unify configuration - controls workspace dependency unification behavior

use serde::{Deserialize, Serialize};

/// Unify configuration - controls workspace dependency unification behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifyConfig {
  /// Handle path dependencies? (default: true)
  /// If false, path dependencies are excluded from unification
  #[serde(default = "default_include_paths")]
  pub include_paths: bool,

  /// Handle renamed dependencies (package = "...")? (default: false)
  /// Renamed deps are tricky to unify correctly, opt-in only
  #[serde(default)]
  pub include_renamed: bool,

  /// Pin transitive-only deps with fragmented features? (default: false)
  /// This is cargo-rail's workspace-hack replacement
  /// When enabled, transitive deps with multiple feature sets are pinned in workspace.dependencies
  /// Only enable if your project uses cargo-hakari or a workspace-hack crate
  #[serde(default)]
  pub pin_transitives: bool,

  /// Where to put pinned transitive dev-deps? (default: "root")
  /// Options: "root" or a path like "crates/foo"
  #[serde(default = "default_transitive_host")]
  pub transitive_host: TransitiveFeatureHost,

  /// Dependencies to exclude from unification (safety hatch)
  #[serde(default)]
  pub exclude: Vec<String>,

  /// Dependencies to force-include in unification (safety hatch)
  #[serde(default)]
  pub include: Vec<String>,

  /// Maximum number of backups to keep (default: 3)
  /// Older backups are automatically cleaned up after successful unify operations
  #[serde(default = "default_max_backups")]
  pub max_backups: usize,

  /// Compute and write MSRV to workspace manifest? (default: true)
  /// When enabled, cargo-rail computes the maximum rust-version from all
  /// resolved dependencies and writes it to [workspace.package].rust-version
  #[serde(default = "default_true")]
  pub msrv: bool,

  /// Prune features not referenced in source code? (default: true)
  /// When enabled, analyzes the resolved dependency graph to detect features
  /// that are declared but never enabled by any consumer across all targets.
  /// This produces the absolute leanest feature set for the workspace.
  #[serde(default = "default_true")]
  pub prune_dead_features: bool,

  /// Strict version compatibility checking (default: true)
  /// When true, version mismatches between member manifests and existing
  /// workspace.dependencies are reported as blocking errors.
  /// When false, they are warnings only.
  #[serde(default = "default_true")]
  pub strict_version_compat: bool,

  /// How to handle exact version pins like "=0.8.0" (default: "warn")
  /// - "skip": Exclude exact-pinned deps from unification
  /// - "preserve": Keep the exact pin operator in workspace.dependencies
  /// - "warn": Convert to caret but emit a warning
  #[serde(default)]
  pub exact_pin_handling: ExactPinHandling,

  /// Detect unused dependencies in workspace members (default: true)
  /// When enabled, compares declared deps against the resolved cargo graph
  /// to find deps that are declared but never actually used.
  #[serde(default = "default_true")]
  pub detect_unused: bool,

  /// Automatically remove unused dependencies when applying (default: true)
  /// Requires detect_unused = true. When enabled, unused deps are removed
  /// from member Cargo.toml files during unify.
  #[serde(default = "default_true")]
  pub remove_unused: bool,
}

impl Default for UnifyConfig {
  fn default() -> Self {
    Self {
      include_paths: default_include_paths(),
      include_renamed: false,
      pin_transitives: false,
      transitive_host: default_transitive_host(),
      exclude: Vec::new(),
      include: Vec::new(),
      max_backups: default_max_backups(),
      msrv: true,
      prune_dead_features: true,
      strict_version_compat: true,
      exact_pin_handling: ExactPinHandling::default(),
      detect_unused: true,
      remove_unused: true,
    }
  }
}

impl UnifyConfig {
  /// Check if a dependency should be excluded from unification
  pub fn should_exclude(&self, dep_name: &str) -> bool {
    self.exclude.iter().any(|e| e == dep_name)
  }

  /// Check if a dependency should be force-included in unification
  pub fn should_include(&self, dep_name: &str) -> bool {
    self.include.iter().any(|i| i == dep_name)
  }
}

// ============================================================================
// Helper Types
// ============================================================================

/// How to handle exact version pins ("=x.y.z") during unification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExactPinHandling {
  /// Exclude exact-pinned deps from unification entirely
  Skip,
  /// Preserve the exact pin operator in workspace.dependencies
  Preserve,
  /// Convert to caret (^) but emit a warning (default)
  #[default]
  Warn,
}

/// Configuration for where to add dev-dependencies when consolidating transitive features
#[derive(Debug, Clone, PartialEq, Default)]
pub enum TransitiveFeatureHost {
  /// Use workspace root Cargo.toml (default)
  #[default]
  Root,
  /// Use a specific member crate (relative path from workspace root)
  Path(String),
}

// Custom serialization/deserialization for TransitiveFeatureHost
impl Serialize for TransitiveFeatureHost {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: serde::Serializer,
  {
    match self {
      TransitiveFeatureHost::Root => serializer.serialize_str("root"),
      TransitiveFeatureHost::Path(path) => serializer.serialize_str(path),
    }
  }
}

impl<'de> Deserialize<'de> for TransitiveFeatureHost {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: serde::Deserializer<'de>,
  {
    struct TransitiveFeatureHostVisitor;

    impl serde::de::Visitor<'_> for TransitiveFeatureHostVisitor {
      type Value = TransitiveFeatureHost;

      fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("'root' or a path string")
      }

      fn visit_str<E>(self, value: &str) -> Result<TransitiveFeatureHost, E>
      where
        E: serde::de::Error,
      {
        match value {
          "root" => Ok(TransitiveFeatureHost::Root),
          path => Ok(TransitiveFeatureHost::Path(path.to_string())),
        }
      }
    }

    deserializer.deserialize_any(TransitiveFeatureHostVisitor)
  }
}

// ============================================================================
// Default Functions
// ============================================================================

fn default_max_backups() -> usize {
  3
}

fn default_include_paths() -> bool {
  true
}

fn default_transitive_host() -> TransitiveFeatureHost {
  TransitiveFeatureHost::Root
}

pub(crate) fn default_true() -> bool {
  true
}

// ============================================================================
// Tests
// ============================================================================

#[test]
fn test_transitive_feature_host_path() {
  // Test that path format works with simplified config
  let toml = r#"
      include_paths = true
      include_renamed = false
      pin_transitives = false
      transitive_host = "path/to/crate"
      exclude = []
      include = []
    "#;

  let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
  assert_eq!(
    config.transitive_host,
    TransitiveFeatureHost::Path("path/to/crate".to_string())
  );
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_unify_config_defaults() {
    let config = UnifyConfig::default();
    assert!(config.include_paths); // Default: true
    assert!(!config.include_renamed); // Default: false
    assert!(!config.pin_transitives); // Default: false (only true for hakari users)
    assert_eq!(config.transitive_host, TransitiveFeatureHost::Root);
    assert!(config.exclude.is_empty());
    assert!(config.include.is_empty());
    assert!(config.msrv); // Default: true
    assert!(config.detect_unused); // Default: true
    assert!(config.remove_unused); // Default: true
  }

  #[test]
  fn test_unify_config_should_exclude() {
    let config = UnifyConfig {
      exclude: vec!["tokio".to_string(), "serde".to_string()],
      ..Default::default()
    };
    assert!(config.should_exclude("tokio"));
    assert!(config.should_exclude("serde"));
    assert!(!config.should_exclude("regex"));
  }

  #[test]
  fn test_unify_config_should_include() {
    let config = UnifyConfig {
      include: vec!["special-dep".to_string()],
      ..Default::default()
    };
    assert!(config.should_include("special-dep"));
    assert!(!config.should_include("normal-dep"));
  }

  #[test]
  fn test_transitive_feature_host_in_full_config() {
    let toml = r#"
      include_paths = true
      include_renamed = false
      pin_transitives = true
      transitive_host = "root"
      exclude = []
      include = []
    "#;

    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert_eq!(config.transitive_host, TransitiveFeatureHost::Root);
    assert!(config.include_paths);
    assert!(config.pin_transitives);
  }

  #[test]
  fn test_unify_config_default_transitive_host() {
    let config = UnifyConfig::default();
    assert_eq!(config.transitive_host, TransitiveFeatureHost::Root);
    assert!(!config.pin_transitives); // Default is false (opt-in for hakari users)
  }

  #[test]
  fn test_prune_dead_features_default() {
    let config = UnifyConfig::default();
    assert!(config.prune_dead_features); // Default: true
  }

  #[test]
  fn test_prune_dead_features_parsing() {
    let toml = r#"prune_dead_features = true"#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert!(config.prune_dead_features);

    let toml = r#"prune_dead_features = false"#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert!(!config.prune_dead_features);
  }

  #[test]
  fn test_strict_version_compat_default() {
    let config = UnifyConfig::default();
    assert!(config.strict_version_compat); // Default is true
  }

  #[test]
  fn test_exact_pin_handling_default() {
    let config = UnifyConfig::default();
    assert_eq!(config.exact_pin_handling, ExactPinHandling::Warn);
  }

  #[test]
  fn test_exact_pin_handling_parsing() {
    let toml = r#"exact_pin_handling = "skip""#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert_eq!(config.exact_pin_handling, ExactPinHandling::Skip);

    let toml = r#"exact_pin_handling = "preserve""#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert_eq!(config.exact_pin_handling, ExactPinHandling::Preserve);

    let toml = r#"exact_pin_handling = "warn""#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert_eq!(config.exact_pin_handling, ExactPinHandling::Warn);
  }

  #[test]
  fn test_detect_unused_default() {
    let config = UnifyConfig::default();
    assert!(config.detect_unused); // Default: true
    assert!(config.remove_unused); // Default: true
  }

  #[test]
  fn test_new_config_options_parsing() {
    let toml = r#"
      strict_version_compat = false
      exact_pin_handling = "preserve"
      detect_unused = true
      remove_unused = true
    "#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert!(!config.strict_version_compat);
    assert_eq!(config.exact_pin_handling, ExactPinHandling::Preserve);
    assert!(config.detect_unused);
    assert!(config.remove_unused);
  }
}
