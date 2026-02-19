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
  ///
  /// Workspace-member dependencies are handled as connected cohorts.
  /// Excluding one member excludes the full cohort atomically to avoid
  /// local-vs-registry split graphs.
  #[serde(default)]
  pub exclude: Vec<String>,

  /// Dependencies to force-include in unification (safety hatch)
  ///
  /// Workspace-member cohorts are auto-included by cargo-rail to prevent
  /// single-user threshold splits; this option is mainly for non-member deps.
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

  /// Enforce MSRV inheritance on workspace members (default: false)
  ///
  /// When enabled, `cargo rail unify` ensures every workspace member's Cargo.toml
  /// has `package.rust-version = { workspace = true }`, so that
  /// `[workspace.package].rust-version` is actually enforced across the workspace.
  ///
  /// Note: this requires `msrv = true` and a workspace MSRV that exists or can be computed.
  #[serde(default)]
  pub enforce_msrv_inheritance: bool,

  /// How to determine the final MSRV value (default: "max")
  /// - "deps": Use maximum from dependencies only (original behavior)
  /// - "workspace": Preserve existing rust-version, warn if deps need higher
  /// - "max": Take max(workspace, deps) - explicit workspace setting wins if higher
  #[serde(default)]
  pub msrv_source: MsrvSource,

  /// Prune features not referenced in source code? (default: true)
  /// When enabled, analyzes the resolved dependency graph to detect features
  /// that are declared but never enabled by any consumer across all targets.
  /// This produces the absolute leanest feature set for the workspace.
  #[serde(default = "default_true")]
  pub prune_dead_features: bool,

  /// Features to preserve from dead feature pruning (glob patterns supported)
  /// Use this to keep features intended for future use or external consumers.
  /// Examples: ["future-api", "unstable-*", "bench*"]
  #[serde(default)]
  pub preserve_features: Vec<String>,

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

  /// How to handle major version conflicts (default: "warn")
  /// - "warn": Skip unification and emit a warning (both versions stay in graph)
  /// - "bump": Force unify to highest resolved version (user accepts breakage risk)
  #[serde(default)]
  pub major_version_conflict: MajorVersionConflict,

  /// Detect unused dependencies in workspace members (default: true)
  /// When enabled, uses two signals:
  /// - Graph-level: declared deps absent from the resolved cargo graph.
  /// - Source-level: rustc `unused_crate_dependencies` diagnostics for deps
  ///   that are resolved but never referenced in source.
  ///
  /// Optional deps and deps behind unconfigured target constraints are
  /// conservatively skipped to avoid false positives.
  #[serde(default = "default_true")]
  pub detect_unused: bool,

  /// Automatically remove unused dependencies when applying (default: true)
  /// Requires detect_unused = true. When enabled, unused deps are removed
  /// from member Cargo.toml files during unify.
  #[serde(default = "default_true")]
  pub remove_unused: bool,

  /// Detect undeclared feature dependencies (default: true)
  /// When enabled, compares resolved features against declared features in Cargo.toml
  /// to find crates that rely on Cargo's feature unification to "borrow" features
  /// from other workspace members. After unification, standalone builds of these
  /// crates will fail. Reports as warnings to help fix before unification.
  #[serde(default = "default_true")]
  pub detect_undeclared_features: bool,

  /// Auto-fix undeclared feature dependencies (default: true)
  /// When enabled (and detect_undeclared_features is true), automatically adds
  /// missing features to each crate's Cargo.toml instead of just warning.
  /// This produces a cleaner graph where standalone builds work correctly.
  #[serde(default = "default_true")]
  pub fix_undeclared_features: bool,

  /// Patterns for features to skip in undeclared feature detection (glob supported)
  /// Default: ["default", "std", "alloc", "*_backend", "*_impl"]
  /// These are features that are typically not actionable or are implementation details.
  #[serde(default = "default_skip_undeclared_patterns")]
  pub skip_undeclared_patterns: Vec<String>,

  /// Sort dependencies alphabetically when writing Cargo.toml files (default: true)
  /// When false, preserves existing order and appends new deps at end.
  #[serde(default = "default_true")]
  pub sort_dependencies: bool,
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
      enforce_msrv_inheritance: false,
      msrv_source: MsrvSource::default(),
      prune_dead_features: true,
      preserve_features: Vec::new(),
      strict_version_compat: true,
      exact_pin_handling: ExactPinHandling::default(),
      major_version_conflict: MajorVersionConflict::default(),
      detect_unused: true,
      remove_unused: true,
      detect_undeclared_features: true,
      fix_undeclared_features: true,
      skip_undeclared_patterns: default_skip_undeclared_patterns(),
      sort_dependencies: true,
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

  /// Check if a feature should be preserved from dead feature pruning
  ///
  /// Supports glob patterns (e.g., "unstable-*", "bench*")
  pub fn should_preserve_feature(&self, feature_name: &str) -> bool {
    self.preserve_features.iter().any(|pattern| {
      if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
        // Use glob matching for patterns with wildcards
        glob::Pattern::new(pattern)
          .map(|p| p.matches(feature_name))
          .unwrap_or(false)
      } else {
        // Exact match for literal patterns
        pattern == feature_name
      }
    })
  }

  /// Check if a feature should be skipped in undeclared feature detection
  ///
  /// Supports glob patterns (e.g., "*_backend", "*_impl")
  pub fn should_skip_undeclared_feature(&self, feature_name: &str) -> bool {
    self.skip_undeclared_patterns.iter().any(|pattern| {
      if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
        // Use glob matching for patterns with wildcards
        glob::Pattern::new(pattern)
          .map(|p| p.matches(feature_name))
          .unwrap_or(false)
      } else {
        // Exact match for literal patterns
        pattern == feature_name
      }
    })
  }

  /// Validate unify configuration against the workspace
  ///
  /// Checks:
  /// - transitive_host path exists if configured as a path (not "root")
  /// - transitive_host path contains a Cargo.toml (is a valid crate/workspace)
  pub fn validate(&self, workspace_root: &std::path::Path) -> Result<(), crate::error::ConfigError> {
    // Only validate if pin_transitives is enabled and transitive_host is a path
    if self.pin_transitives
      && let TransitiveFeatureHost::Path(p) = &self.transitive_host
    {
      // Check for path traversal (security/consistency)
      if p.contains("..") {
        return Err(crate::error::ConfigError::InvalidValue {
          field: "unify.transitive_host".to_string(),
          message: format!("path '{}' contains '..' traversal, which is not allowed", p),
        });
      }

      // Check path is not absolute
      if std::path::Path::new(p).is_absolute() {
        return Err(crate::error::ConfigError::InvalidValue {
          field: "unify.transitive_host".to_string(),
          message: format!("path '{}' is absolute, must be relative to workspace root", p),
        });
      }

      // Check directory exists
      let full_path = workspace_root.join(p);
      if !full_path.exists() {
        return Err(crate::error::ConfigError::InvalidValue {
          field: "unify.transitive_host".to_string(),
          message: format!("path '{}' does not exist", p),
        });
      }

      // Check Cargo.toml exists at that path
      let cargo_toml = full_path.join("Cargo.toml");
      if !cargo_toml.exists() {
        return Err(crate::error::ConfigError::InvalidValue {
          field: "unify.transitive_host".to_string(),
          message: format!("path '{}' does not contain a Cargo.toml", p),
        });
      }
    }

    Ok(())
  }
}

// Helper Types

/// How to determine the final MSRV (Minimum Supported Rust Version)
///
/// Controls how cargo-rail computes the rust-version to write to [workspace.package].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MsrvSource {
  /// Use maximum from dependencies only (original behavior)
  ///
  /// Computes the highest rust-version from all resolved dependencies.
  /// Overwrites any existing workspace rust-version.
  Deps,
  /// Preserve existing workspace rust-version
  ///
  /// Keeps the existing [workspace.package].rust-version unchanged.
  /// Emits a warning if dependencies require a higher version.
  Workspace,
  /// Take max(workspace, deps) - default
  ///
  /// Uses the higher of the existing workspace rust-version or the
  /// maximum from dependencies. Your explicit workspace setting wins
  /// if it requires a higher Rust version than your dependencies.
  #[default]
  Max,
}

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

/// How to handle major version conflicts during unification
///
/// Major version conflicts occur when the same dependency is declared with
/// different major versions across workspace members (e.g., `serde = "1.0"` and `serde = "2.0"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MajorVersionConflict {
  /// Skip unification and emit a warning (default)
  ///
  /// Both versions remain in the build graph. This is the safe choice when
  /// you want to avoid breaking changes but may result in duplicate compilation.
  #[default]
  Warn,
  /// Force unify to the highest resolved version
  ///
  /// Uses the highest version from the resolved metadata across all target triples.
  /// This works in ~85% of cases; the remaining ~15% may break the codebase.
  /// Use when you want the leanest build graph and accept breakage risk.
  Bump,
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

// Default Functions

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

fn default_skip_undeclared_patterns() -> Vec<String> {
  const PATTERNS: &[&str] = &["default", "std", "alloc", "*_backend", "*_impl"];
  PATTERNS.iter().map(|&s| String::from(s)).collect()
}

// Tests

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

  #[test]
  fn test_major_version_conflict_default() {
    let config = UnifyConfig::default();
    assert_eq!(config.major_version_conflict, MajorVersionConflict::Warn);
  }

  #[test]
  fn test_major_version_conflict_parsing() {
    let toml = r#"major_version_conflict = "warn""#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert_eq!(config.major_version_conflict, MajorVersionConflict::Warn);

    let toml = r#"major_version_conflict = "bump""#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert_eq!(config.major_version_conflict, MajorVersionConflict::Bump);
  }

  #[test]
  fn test_major_version_conflict_with_other_options() {
    let toml = r#"
      strict_version_compat = false
      exact_pin_handling = "preserve"
      major_version_conflict = "bump"
    "#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert!(!config.strict_version_compat);
    assert_eq!(config.exact_pin_handling, ExactPinHandling::Preserve);
    assert_eq!(config.major_version_conflict, MajorVersionConflict::Bump);
  }

  #[test]
  fn test_transitive_host_validate_root() {
    // Root should always be valid
    let config = UnifyConfig {
      pin_transitives: true,
      transitive_host: TransitiveFeatureHost::Root,
      ..Default::default()
    };
    let workspace = std::env::current_dir().unwrap();
    assert!(config.validate(&workspace).is_ok());
  }

  #[test]
  fn test_transitive_host_validate_valid_path() {
    // src/ exists and doesn't have a Cargo.toml - but pin_transitives=false skips validation
    let config = UnifyConfig {
      pin_transitives: false,
      transitive_host: TransitiveFeatureHost::Path("src".to_string()),
      ..Default::default()
    };
    let workspace = std::env::current_dir().unwrap();
    assert!(config.validate(&workspace).is_ok());
  }

  #[test]
  fn test_transitive_host_validate_nonexistent_path() {
    let config = UnifyConfig {
      pin_transitives: true,
      transitive_host: TransitiveFeatureHost::Path("nonexistent/path".to_string()),
      ..Default::default()
    };
    let workspace = std::env::current_dir().unwrap();
    let result = config.validate(&workspace);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, crate::error::ConfigError::InvalidValue { .. }));
  }

  #[test]
  fn test_transitive_host_validate_path_traversal() {
    let config = UnifyConfig {
      pin_transitives: true,
      transitive_host: TransitiveFeatureHost::Path("../somewhere".to_string()),
      ..Default::default()
    };
    let workspace = std::env::current_dir().unwrap();
    let result = config.validate(&workspace);
    assert!(result.is_err());
    let err = result.unwrap_err();
    if let crate::error::ConfigError::InvalidValue { message, .. } = err {
      assert!(message.contains(".."));
    } else {
      panic!("Expected InvalidValue error");
    }
  }

  #[test]
  fn test_preserve_features_default() {
    let config = UnifyConfig::default();
    assert!(config.preserve_features.is_empty());
  }

  #[test]
  fn test_preserve_features_parsing() {
    let toml = r#"
      preserve_features = ["future-api", "unstable-*", "bench*"]
    "#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert_eq!(config.preserve_features.len(), 3);
    assert!(config.preserve_features.contains(&"future-api".to_string()));
    assert!(config.preserve_features.contains(&"unstable-*".to_string()));
    assert!(config.preserve_features.contains(&"bench*".to_string()));
  }

  #[test]
  fn test_should_preserve_feature_exact_match() {
    let config = UnifyConfig {
      preserve_features: vec!["future-api".to_string(), "experimental".to_string()],
      ..Default::default()
    };
    assert!(config.should_preserve_feature("future-api"));
    assert!(config.should_preserve_feature("experimental"));
    assert!(!config.should_preserve_feature("other-feature"));
  }

  #[test]
  fn test_should_preserve_feature_glob_wildcard() {
    let config = UnifyConfig {
      preserve_features: vec!["unstable-*".to_string()],
      ..Default::default()
    };
    assert!(config.should_preserve_feature("unstable-api"));
    assert!(config.should_preserve_feature("unstable-feature"));
    assert!(config.should_preserve_feature("unstable-"));
    assert!(!config.should_preserve_feature("unstable")); // No trailing dash
    assert!(!config.should_preserve_feature("stable-api"));
  }

  #[test]
  fn test_should_preserve_feature_glob_suffix() {
    let config = UnifyConfig {
      preserve_features: vec!["bench*".to_string()],
      ..Default::default()
    };
    assert!(config.should_preserve_feature("bench"));
    assert!(config.should_preserve_feature("benchmark"));
    assert!(config.should_preserve_feature("benchmarks"));
    assert!(!config.should_preserve_feature("prebench"));
  }

  #[test]
  fn test_should_preserve_feature_glob_question_mark() {
    let config = UnifyConfig {
      preserve_features: vec!["test-?".to_string()],
      ..Default::default()
    };
    assert!(config.should_preserve_feature("test-a"));
    assert!(config.should_preserve_feature("test-1"));
    assert!(!config.should_preserve_feature("test-ab")); // Two chars
    assert!(!config.should_preserve_feature("test-")); // No char
  }

  #[test]
  fn test_should_preserve_feature_multiple_patterns() {
    let config = UnifyConfig {
      preserve_features: vec!["future-api".to_string(), "unstable-*".to_string(), "bench*".to_string()],
      ..Default::default()
    };
    // Exact match
    assert!(config.should_preserve_feature("future-api"));
    // Glob matches
    assert!(config.should_preserve_feature("unstable-feature"));
    assert!(config.should_preserve_feature("benchmark"));
    // Non-matches
    assert!(!config.should_preserve_feature("stable-api"));
    assert!(!config.should_preserve_feature("other"));
  }

  #[test]
  fn test_should_preserve_feature_empty_list() {
    let config = UnifyConfig::default();
    assert!(!config.should_preserve_feature("any-feature"));
  }

  #[test]
  fn test_msrv_source_default() {
    let config = UnifyConfig::default();
    assert_eq!(config.msrv_source, MsrvSource::Max);
  }

  #[test]
  fn test_msrv_source_parsing_deps() {
    let toml = r#"msrv_source = "deps""#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert_eq!(config.msrv_source, MsrvSource::Deps);
  }

  #[test]
  fn test_msrv_source_parsing_workspace() {
    let toml = r#"msrv_source = "workspace""#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert_eq!(config.msrv_source, MsrvSource::Workspace);
  }

  #[test]
  fn test_msrv_source_parsing_max() {
    let toml = r#"msrv_source = "max""#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert_eq!(config.msrv_source, MsrvSource::Max);
  }

  #[test]
  fn test_msrv_source_with_msrv_enabled() {
    let toml = r#"
      msrv = true
      msrv_source = "workspace"
    "#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert!(config.msrv);
    assert_eq!(config.msrv_source, MsrvSource::Workspace);
  }

  #[test]
  fn test_msrv_source_with_msrv_disabled() {
    let toml = r#"
      msrv = false
      msrv_source = "deps"
    "#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert!(!config.msrv);
    assert_eq!(config.msrv_source, MsrvSource::Deps);
  }

  #[test]
  fn test_detect_undeclared_features_default() {
    let config = UnifyConfig::default();
    assert!(config.detect_undeclared_features); // Default: true
  }

  #[test]
  fn test_fix_undeclared_features_default() {
    let config = UnifyConfig::default();
    assert!(config.fix_undeclared_features); // Default: true
  }

  #[test]
  fn test_detect_undeclared_features_parsing_true() {
    let toml = r#"detect_undeclared_features = true"#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert!(config.detect_undeclared_features);
  }

  #[test]
  fn test_detect_undeclared_features_parsing_false() {
    let toml = r#"detect_undeclared_features = false"#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert!(!config.detect_undeclared_features);
  }

  #[test]
  fn test_fix_undeclared_features_parsing_true() {
    let toml = r#"fix_undeclared_features = true"#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert!(config.fix_undeclared_features);
  }

  #[test]
  fn test_fix_undeclared_features_parsing_false() {
    let toml = r#"fix_undeclared_features = false"#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert!(!config.fix_undeclared_features);
  }

  #[test]
  fn test_undeclared_features_both_options() {
    let toml = r#"
      detect_undeclared_features = true
      fix_undeclared_features = false
    "#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert!(config.detect_undeclared_features);
    assert!(!config.fix_undeclared_features);
  }

  #[test]
  fn test_undeclared_features_with_other_options() {
    let toml = r#"
      detect_unused = true
      remove_unused = true
      detect_undeclared_features = true
      fix_undeclared_features = true
      prune_dead_features = false
    "#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert!(config.detect_unused);
    assert!(config.remove_unused);
    assert!(config.detect_undeclared_features);
    assert!(config.fix_undeclared_features);
    assert!(!config.prune_dead_features);
  }

  #[test]
  fn test_undeclared_features_detect_disabled_fix_enabled() {
    // Edge case: fix enabled but detect disabled
    // This is a valid config but fix won't do anything if detect is off
    let toml = r#"
      detect_undeclared_features = false
      fix_undeclared_features = true
    "#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert!(!config.detect_undeclared_features);
    assert!(config.fix_undeclared_features);
  }

  // skip_undeclared_patterns Tests

  #[test]
  fn test_skip_undeclared_patterns_default() {
    let config = UnifyConfig::default();
    assert!(!config.skip_undeclared_patterns.is_empty());
    assert!(config.skip_undeclared_patterns.contains(&"default".to_string()));
    assert!(config.skip_undeclared_patterns.contains(&"std".to_string()));
    assert!(config.skip_undeclared_patterns.contains(&"alloc".to_string()));
    assert!(config.skip_undeclared_patterns.contains(&"*_backend".to_string()));
    assert!(config.skip_undeclared_patterns.contains(&"*_impl".to_string()));
  }

  #[test]
  fn test_skip_undeclared_patterns_parsing() {
    let toml = r#"
      skip_undeclared_patterns = ["default", "std", "custom-*"]
    "#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert_eq!(config.skip_undeclared_patterns.len(), 3);
    assert!(config.skip_undeclared_patterns.contains(&"default".to_string()));
    assert!(config.skip_undeclared_patterns.contains(&"std".to_string()));
    assert!(config.skip_undeclared_patterns.contains(&"custom-*".to_string()));
  }

  #[test]
  fn test_skip_undeclared_patterns_empty() {
    let toml = r#"
      skip_undeclared_patterns = []
    "#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert!(config.skip_undeclared_patterns.is_empty());
  }

  #[test]
  fn test_should_skip_undeclared_feature_exact_match() {
    let config = UnifyConfig {
      skip_undeclared_patterns: vec!["default".to_string(), "std".to_string()],
      ..Default::default()
    };
    assert!(config.should_skip_undeclared_feature("default"));
    assert!(config.should_skip_undeclared_feature("std"));
    assert!(!config.should_skip_undeclared_feature("derive"));
  }

  #[test]
  fn test_should_skip_undeclared_feature_glob_suffix() {
    let config = UnifyConfig {
      skip_undeclared_patterns: vec!["*_backend".to_string()],
      ..Default::default()
    };
    assert!(config.should_skip_undeclared_feature("sqlite_backend"));
    assert!(config.should_skip_undeclared_feature("postgres_backend"));
    assert!(config.should_skip_undeclared_feature("_backend")); // Just suffix
    assert!(!config.should_skip_undeclared_feature("backend"));
    assert!(!config.should_skip_undeclared_feature("backend_"));
  }

  #[test]
  fn test_should_skip_undeclared_feature_glob_prefix() {
    let config = UnifyConfig {
      skip_undeclared_patterns: vec!["unstable-*".to_string()],
      ..Default::default()
    };
    assert!(config.should_skip_undeclared_feature("unstable-api"));
    assert!(config.should_skip_undeclared_feature("unstable-internal"));
    assert!(config.should_skip_undeclared_feature("unstable-")); // Just prefix
    assert!(!config.should_skip_undeclared_feature("unstable"));
  }

  #[test]
  fn test_should_skip_undeclared_feature_glob_question_mark() {
    let config = UnifyConfig {
      skip_undeclared_patterns: vec!["test-?".to_string()],
      ..Default::default()
    };
    assert!(config.should_skip_undeclared_feature("test-1"));
    assert!(config.should_skip_undeclared_feature("test-a"));
    assert!(!config.should_skip_undeclared_feature("test-12"));
    assert!(!config.should_skip_undeclared_feature("test-"));
  }

  #[test]
  fn test_should_skip_undeclared_feature_multiple_patterns() {
    let config = UnifyConfig {
      skip_undeclared_patterns: vec![
        "default".to_string(),
        "std".to_string(),
        "*_backend".to_string(),
        "*_impl".to_string(),
      ],
      ..Default::default()
    };
    assert!(config.should_skip_undeclared_feature("default"));
    assert!(config.should_skip_undeclared_feature("std"));
    assert!(config.should_skip_undeclared_feature("sqlite_backend"));
    assert!(config.should_skip_undeclared_feature("sync_impl"));
    assert!(!config.should_skip_undeclared_feature("derive"));
    assert!(!config.should_skip_undeclared_feature("serde"));
  }

  #[test]
  fn test_should_skip_undeclared_feature_empty_patterns() {
    let config = UnifyConfig {
      skip_undeclared_patterns: vec![],
      ..Default::default()
    };
    // Nothing should be skipped with empty patterns
    assert!(!config.should_skip_undeclared_feature("default"));
    assert!(!config.should_skip_undeclared_feature("std"));
    assert!(!config.should_skip_undeclared_feature("anything"));
  }

  // sort_dependencies Tests

  #[test]
  fn test_sort_dependencies_default() {
    let config = UnifyConfig::default();
    assert!(config.sort_dependencies); // Default is true (alphabetical)
  }

  #[test]
  fn test_sort_dependencies_parsing_true() {
    let toml = r#"sort_dependencies = true"#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert!(config.sort_dependencies);
  }

  #[test]
  fn test_sort_dependencies_parsing_false() {
    let toml = r#"sort_dependencies = false"#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert!(!config.sort_dependencies);
  }

  #[test]
  fn test_sort_dependencies_with_other_options() {
    let toml = r#"
      detect_unused = true
      remove_unused = true
      sort_dependencies = false
    "#;
    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert!(config.detect_unused);
    assert!(config.remove_unused);
    assert!(!config.sort_dependencies);
  }
}
