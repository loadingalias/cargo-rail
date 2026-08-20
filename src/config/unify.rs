//! Unify configuration - controls workspace dependency unification behavior

use serde::{Deserialize, Deserializer, Serialize, de};

/// Defines which consumers may activate private workspace configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConsumerScope {
    /// Consumers may exist outside this workspace; dormant configuration is preserved.
    #[default]
    Open,
    /// This workspace is the complete consumer universe for `publish = false` packages.
    Workspace,
}

impl ConsumerScope {
    /// Stable configuration spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Workspace => "workspace",
        }
    }
}

/// Unify configuration - controls workspace dependency unification behavior
#[derive(Debug, Clone, Serialize)]
pub struct UnifyConfig {
    /// Handle path dependencies? (default: true)
    /// If false, path dependencies are excluded from unification
    #[serde(default = "default_include_paths")]
    pub include_paths: bool,

    /// Handle renamed dependencies (package = "...")? (default: false)
    /// Renamed deps are tricky to unify correctly, opt-in only
    #[serde(default)]
    pub include_renamed: bool,

    /// Host-owned pins for fragmented transitive features and their owning manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transitive_pinning: Option<TransitivePinning>,

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

    /// MSRV computation and inheritance policy.
    #[serde(default)]
    pub msrv_policy: MsrvPolicy,

    /// Consumer boundary used by destructive feature and optional-dependency pruning.
    ///
    /// `open` reports dormant configuration without removing it. `workspace`
    /// authorizes removal from `publish = false` packages after complete
    /// reachability and post-edit graph verification.
    #[serde(default)]
    pub consumer_scope: ConsumerScope,

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

    /// Patterns for features to skip in undeclared feature detection (glob supported)
    /// Default: ["default", "std", "alloc", "*_backend", "*_impl"]
    /// These are features that are typically not actionable or are implementation details.
    #[serde(default = "default_skip_undeclared_patterns")]
    pub skip_undeclared_patterns: Vec<String>,
}

impl Default for UnifyConfig {
    fn default() -> Self {
        Self {
            include_paths: default_include_paths(),
            include_renamed: false,
            transitive_pinning: None,
            exclude: Vec::new(),
            include: Vec::new(),
            max_backups: default_max_backups(),
            msrv_policy: MsrvPolicy::default(),
            consumer_scope: ConsumerScope::default(),
            preserve_features: Vec::new(),
            strict_version_compat: true,
            exact_pin_handling: ExactPinHandling::default(),
            major_version_conflict: MajorVersionConflict::default(),
            skip_undeclared_patterns: default_skip_undeclared_patterns(),
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
    /// - transitive pinning host path exists if configured as a path (not "root")
    /// - transitive pinning host path contains a Cargo.toml
    pub fn validate(&self, workspace_root: &std::path::Path) -> Result<(), crate::error::ConfigError> {
        validate_glob_patterns("unify.preserve_features", &self.preserve_features)?;
        validate_glob_patterns("unify.skip_undeclared_patterns", &self.skip_undeclared_patterns)?;

        if let Some(TransitivePinning {
            host: TransitiveFeatureHost::Path(p),
        }) = &self.transitive_pinning
        {
            // Check for path traversal (security/consistency)
            if p.contains("..") {
                return Err(crate::error::ConfigError::InvalidValue {
                    field: "unify.transitive_pinning.host".to_string(),
                    message: format!("path '{}' contains '..' traversal, which is not allowed", p),
                });
            }

            // Check path is not absolute
            if std::path::Path::new(p).is_absolute() {
                return Err(crate::error::ConfigError::InvalidValue {
                    field: "unify.transitive_pinning.host".to_string(),
                    message: format!("path '{}' is absolute, must be relative to workspace root", p),
                });
            }

            // Check directory exists
            let full_path = workspace_root.join(p);
            if !full_path.exists() {
                return Err(crate::error::ConfigError::InvalidValue {
                    field: "unify.transitive_pinning.host".to_string(),
                    message: format!("path '{}' does not exist", p),
                });
            }

            // Check Cargo.toml exists at that path
            let cargo_toml = full_path.join("Cargo.toml");
            if !cargo_toml.exists() {
                return Err(crate::error::ConfigError::InvalidValue {
                    field: "unify.transitive_pinning.host".to_string(),
                    message: format!("path '{}' does not contain a Cargo.toml", p),
                });
            }
        }

        Ok(())
    }
}

fn validate_glob_patterns(field: &str, patterns: &[String]) -> Result<(), crate::error::ConfigError> {
    for pattern in patterns {
        glob::Pattern::new(pattern).map_err(|error| crate::error::ConfigError::InvalidGlobPattern {
            pattern: pattern.clone(),
            message: format!("{field}: {error}"),
        })?;
    }
    Ok(())
}

/// How to determine the final MSRV (Minimum Supported Rust Version)
///
/// Controls how cargo-rail computes the rust-version to write to [workspace.package].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MsrvSource {
    /// Use the maximum dependency rust-version as the candidate floor.
    ///
    /// A higher existing workspace declaration is retained because dependency
    /// metadata does not prove that the workspace compiles on an older compiler.
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
    /// Use when you want the leanest build graph and accept breakage risk.
    Bump,
}

/// Configuration for where to add dev-dependencies when consolidating transitive features
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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

/// Enabled host-owned pinning policy for fragmented transitive features.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitivePinning {
    /// Manifest that owns the generated transitive dev-dependencies.
    #[serde(default)]
    pub host: TransitiveFeatureHost,
}

/// MSRV computation and member-inheritance policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum MsrvPolicy {
    /// Do not compute or write workspace MSRV.
    Disabled,
    /// Compute workspace MSRV from the selected authoritative sources.
    Compute {
        /// Inputs used to choose the final version.
        #[serde(default)]
        source: MsrvSource,
        /// Whether members inherit the computed workspace value.
        #[serde(default)]
        inherit: bool,
    },
}

impl Default for MsrvPolicy {
    fn default() -> Self {
        Self::Compute {
            source: MsrvSource::default(),
            inherit: false,
        }
    }
}

impl MsrvPolicy {
    /// Source policy when computation is enabled.
    pub const fn source(self) -> Option<MsrvSource> {
        match self {
            Self::Disabled => None,
            Self::Compute { source, .. } => Some(source),
        }
    }

    /// Whether workspace members must inherit the computed value.
    pub const fn inherits(self) -> bool {
        matches!(self, Self::Compute { inherit: true, .. })
    }
}

#[derive(Deserialize)]
#[serde(default)]
struct UnifyConfigInput {
    include_paths: bool,
    include_renamed: bool,
    transitive_pinning: Option<TransitivePinning>,
    pin_transitives: Option<bool>,
    transitive_host: Option<TransitiveFeatureHost>,
    exclude: Vec<String>,
    include: Vec<String>,
    max_backups: usize,
    msrv_policy: Option<MsrvPolicy>,
    msrv: Option<bool>,
    enforce_msrv_inheritance: Option<bool>,
    msrv_source: Option<MsrvSource>,
    consumer_scope: ConsumerScope,
    preserve_features: Vec<String>,
    strict_version_compat: bool,
    exact_pin_handling: ExactPinHandling,
    major_version_conflict: MajorVersionConflict,
    skip_undeclared_patterns: Vec<String>,
}

impl Default for UnifyConfigInput {
    fn default() -> Self {
        let config = UnifyConfig::default();
        Self {
            include_paths: config.include_paths,
            include_renamed: config.include_renamed,
            transitive_pinning: None,
            pin_transitives: None,
            transitive_host: None,
            exclude: config.exclude,
            include: config.include,
            max_backups: config.max_backups,
            msrv_policy: None,
            msrv: None,
            enforce_msrv_inheritance: None,
            msrv_source: None,
            consumer_scope: config.consumer_scope,
            preserve_features: config.preserve_features,
            strict_version_compat: config.strict_version_compat,
            exact_pin_handling: config.exact_pin_handling,
            major_version_conflict: config.major_version_conflict,
            skip_undeclared_patterns: config.skip_undeclared_patterns,
        }
    }
}

impl<'de> Deserialize<'de> for UnifyConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let input = UnifyConfigInput::deserialize(deserializer)?;
        let has_legacy_transitive = input.pin_transitives.is_some() || input.transitive_host.is_some();
        if input.transitive_pinning.is_some() && has_legacy_transitive {
            return Err(de::Error::custom(
                "unify.transitive_pinning cannot be combined with deprecated pin_transitives or transitive_host; run `cargo rail config migrate`",
            ));
        }
        let transitive_pinning = input.transitive_pinning.or_else(|| {
            input.pin_transitives.unwrap_or(false).then(|| TransitivePinning {
                host: input.transitive_host.unwrap_or_default(),
            })
        });

        let has_legacy_msrv =
            input.msrv.is_some() || input.enforce_msrv_inheritance.is_some() || input.msrv_source.is_some();
        if input.msrv_policy.is_some() && has_legacy_msrv {
            return Err(de::Error::custom(
                "unify.msrv_policy cannot be combined with deprecated msrv, msrv_source, or enforce_msrv_inheritance; run `cargo rail config migrate`",
            ));
        }
        let msrv_policy = if let Some(policy) = input.msrv_policy {
            policy
        } else {
            let enabled = input.msrv.unwrap_or(true);
            let inherit = input.enforce_msrv_inheritance.unwrap_or(false);
            if !enabled && inherit {
                return Err(de::Error::custom(
                    "deprecated enforce_msrv_inheritance = true cannot be combined with msrv = false",
                ));
            }
            if enabled {
                MsrvPolicy::Compute {
                    source: input.msrv_source.unwrap_or_default(),
                    inherit,
                }
            } else {
                MsrvPolicy::Disabled
            }
        };

        Ok(Self {
            include_paths: input.include_paths,
            include_renamed: input.include_renamed,
            transitive_pinning,
            exclude: input.exclude,
            include: input.include,
            max_backups: input.max_backups,
            msrv_policy,
            consumer_scope: input.consumer_scope,
            preserve_features: input.preserve_features,
            strict_version_compat: input.strict_version_compat,
            exact_pin_handling: input.exact_pin_handling,
            major_version_conflict: input.major_version_conflict,
            skip_undeclared_patterns: input.skip_undeclared_patterns,
        })
    }
}

// Default Functions

fn default_max_backups() -> usize {
    3
}

fn default_include_paths() -> bool {
    true
}

pub(crate) fn default_true() -> bool {
    true
}

fn default_skip_undeclared_patterns() -> Vec<String> {
    const PATTERNS: &[&str] = &["default", "std", "alloc", "*_backend", "*_impl"];
    PATTERNS.iter().map(|&s| String::from(s)).collect()
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unify_config_defaults() {
        let config = UnifyConfig::default();
        assert!(config.include_paths); // Default: true
        assert!(!config.include_renamed); // Default: false
        assert!(config.transitive_pinning.is_none());
        assert!(config.exclude.is_empty());
        assert!(config.include.is_empty());
        assert_eq!(config.msrv_policy.source(), Some(MsrvSource::Max));
        assert_eq!(config.consumer_scope, ConsumerScope::Open);
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
        assert_eq!(
            config.transitive_pinning,
            Some(TransitivePinning {
                host: TransitiveFeatureHost::Root
            })
        );
        assert!(config.include_paths);
    }

    #[test]
    fn test_unify_config_default_transitive_host() {
        let config = UnifyConfig::default();
        assert!(config.transitive_pinning.is_none());
    }

    #[test]
    fn typed_unify_policies_parse_without_invalid_combinations() {
        let config: UnifyConfig = toml_edit::de::from_str(
            r#"
      transitive_pinning = { host = "crates/host" }
      msrv_policy = { mode = "compute", source = "workspace", inherit = true }
      "#,
        )
        .unwrap();
        assert_eq!(
            config.transitive_pinning,
            Some(TransitivePinning {
                host: TransitiveFeatureHost::Path("crates/host".to_string())
            })
        );
        assert_eq!(
            config.msrv_policy,
            MsrvPolicy::Compute {
                source: MsrvSource::Workspace,
                inherit: true
            }
        );
    }

    #[test]
    fn invalid_legacy_msrv_combination_cannot_be_constructed() {
        let error =
            toml_edit::de::from_str::<UnifyConfig>("msrv = false\nenforce_msrv_inheritance = true\n").unwrap_err();
        assert!(error.to_string().contains("cannot be combined"));
    }

    #[test]
    fn test_consumer_scope_requires_explicit_workspace_contract() {
        let default = UnifyConfig::default();
        assert_eq!(default.consumer_scope, ConsumerScope::Open);

        let config: UnifyConfig = toml_edit::de::from_str("consumer_scope = \"workspace\"").unwrap();
        assert_eq!(config.consumer_scope, ConsumerScope::Workspace);

        let invalid = toml_edit::de::from_str::<UnifyConfig>("consumer_scope = \"private\"");
        assert!(
            invalid.is_err(),
            "unknown trust boundaries must fail configuration parsing"
        );
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
    fn test_new_config_options_parsing() {
        let toml = r#"
      strict_version_compat = false
      exact_pin_handling = "preserve"
    "#;
        let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
        assert!(!config.strict_version_compat);
        assert_eq!(config.exact_pin_handling, ExactPinHandling::Preserve);
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
            transitive_pinning: Some(TransitivePinning {
                host: TransitiveFeatureHost::Root,
            }),
            ..Default::default()
        };
        let workspace = std::env::current_dir().unwrap();
        config.validate(&workspace).unwrap();
    }

    #[test]
    fn test_transitive_host_validate_valid_path() {
        let config = UnifyConfig::default();
        let workspace = std::env::current_dir().unwrap();
        config.validate(&workspace).unwrap();
    }

    #[test]
    fn test_transitive_host_validate_nonexistent_path() {
        let config = UnifyConfig {
            transitive_pinning: Some(TransitivePinning {
                host: TransitiveFeatureHost::Path("nonexistent/path".to_string()),
            }),
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
            transitive_pinning: Some(TransitivePinning {
                host: TransitiveFeatureHost::Path("../somewhere".to_string()),
            }),
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
    fn invalid_feature_globs_fail_validation() {
        for (field, config) in [
            (
                "unify.preserve_features",
                UnifyConfig {
                    preserve_features: vec!["[".to_string()],
                    ..Default::default()
                },
            ),
            (
                "unify.skip_undeclared_patterns",
                UnifyConfig {
                    skip_undeclared_patterns: vec!["[".to_string()],
                    ..Default::default()
                },
            ),
        ] {
            let error = config.validate(std::path::Path::new(".")).unwrap_err();
            assert!(
                matches!(
                  error,
                  crate::error::ConfigError::InvalidGlobPattern {
                    ref message,
                    ..
                  } if message.contains(field)
                ),
                "{field} should reject malformed glob patterns"
            );
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
        assert_eq!(config.msrv_policy.source(), Some(MsrvSource::Max));
    }

    #[test]
    fn test_msrv_source_parsing_deps() {
        let toml = r#"msrv_source = "deps""#;
        let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
        assert_eq!(config.msrv_policy.source(), Some(MsrvSource::Deps));
    }

    #[test]
    fn test_msrv_source_parsing_workspace() {
        let toml = r#"msrv_source = "workspace""#;
        let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
        assert_eq!(config.msrv_policy.source(), Some(MsrvSource::Workspace));
    }

    #[test]
    fn test_msrv_source_parsing_max() {
        let toml = r#"msrv_source = "max""#;
        let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
        assert_eq!(config.msrv_policy.source(), Some(MsrvSource::Max));
    }

    #[test]
    fn test_msrv_source_with_msrv_enabled() {
        let toml = r#"
      msrv = true
      msrv_source = "workspace"
    "#;
        let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
        assert_eq!(config.msrv_policy.source(), Some(MsrvSource::Workspace));
    }

    #[test]
    fn test_msrv_source_with_msrv_disabled() {
        let toml = r#"
      msrv = false
      msrv_source = "deps"
    "#;
        let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
        assert_eq!(config.msrv_policy, MsrvPolicy::Disabled);
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
}
