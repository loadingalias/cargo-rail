use crate::error::{ConfigError, RailError, RailResult, ResultExt};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Configuration for cargo-rail
/// Searched in order: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RailConfig {
  /// Workspace configuration
  pub workspace: WorkspaceConfig,
  /// Target triples for multi-platform validation (workspace-wide)
  /// Detected via `cargo rail init`, used by multiple commands
  #[serde(default)]
  pub targets: Vec<String>,
  /// Dependency unification settings
  #[serde(default)]
  pub unify: UnifyConfig,
  /// Release management settings
  #[serde(default)]
  pub release: ReleaseConfig,
  /// Change detection settings (for `affected` command)
  #[serde(default, rename = "change-detection")]
  pub change_detection: ChangeDetectionConfig,
  /// Per-crate configuration (overrides workspace defaults)
  #[serde(default)]
  pub crates: std::collections::HashMap<String, CrateConfig>,
  /// TOML formatting settings
  #[serde(default)]
  pub formatting: FormattingConfig,
}

/// Per-crate configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CrateConfig {
  /// Split/sync configuration for this crate
  pub split: Option<CrateSplitConfig>,
  /// Release configuration for this crate
  pub release: Option<CrateReleaseConfig>,
  /// Changelog configuration for this crate
  pub changelog: Option<ChangelogConfig>,
  /// Sync configuration for this crate (future use)
  pub sync: Option<CrateSyncConfig>,
}

/// Split configuration for a crate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrateSplitConfig {
  /// Remote repository URL or local path
  pub remote: String,
  /// Git branch to use
  pub branch: String,
  /// Split mode (single or combined)
  pub mode: SplitMode,
  /// For combined mode: how to structure the split repo
  #[serde(default)]
  pub workspace_mode: WorkspaceMode,
  /// Crate paths to include in the split
  #[serde(default)]
  pub paths: Vec<CratePath>,
  /// Additional files/directories to include
  #[serde(default)]
  pub include: Vec<String>,
  /// Files/directories to exclude
  #[serde(default)]
  pub exclude: Vec<String>,
}

/// Release configuration for a crate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrateReleaseConfig {
  /// Enable/disable publishing for this crate (overrides Cargo.toml)
  #[serde(default = "default_true")]
  pub publish: bool,
}

/// Changelog configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangelogConfig {
  /// Path to changelog file (relative to crate root)
  pub path: Option<PathBuf>,
  /// Exclude this crate from changelog generation?
  #[serde(default)]
  pub skip: bool,
}

/// Sync configuration for a crate (future use)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CrateSyncConfig {
  // Future sync-specific settings
}

/// Workspace location configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
  /// Path to workspace root
  pub root: PathBuf,
}

/// Unify configuration - controls workspace dependency unification behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifyConfig {
  /// Handle path dependencies? (default: true)
  /// If false, path dependencies are excluded from unification
  #[serde(default = "default_include_paths")]
  pub include_paths: bool,

  /// Handle renamed dependencies (package = "...")? (default: false)
  /// Renamed deps are tricky to unify correctly, opt-in only
  #[serde(default, alias = "allow_renamed")]
  pub include_renamed: bool,

  /// Pin transitive-only deps with fragmented features? (default: true)
  /// This is cargo-rail's workspace-hack replacement
  /// When enabled, transitive deps with multiple feature sets are pinned in workspace.dependencies
  #[serde(default = "default_pin_transitives")]
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

  /// Compute and write MSRV to workspace manifest? (default: false)
  /// When enabled, cargo-rail computes the maximum rust-version from all
  /// resolved dependencies and writes it to [workspace.package].rust-version
  #[serde(default)]
  pub msrv: bool,

  /// Prune features not referenced in source code? (default: false)
  /// When enabled, analyzes the resolved dependency graph to detect features
  /// that are declared but never enabled by any consumer across all targets.
  /// This produces the absolute leanest feature set for the workspace.
  #[serde(default)]
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

  /// Detect unused dependencies in workspace members (default: false)
  /// When enabled, compares declared deps against the resolved cargo graph
  /// to find deps that are declared but never actually used.
  #[serde(default)]
  pub detect_unused: bool,

  /// Automatically remove unused dependencies when applying (default: false)
  /// Requires detect_unused = true. When enabled, unused deps are removed
  /// from member Cargo.toml files during unify apply.
  #[serde(default)]
  pub remove_unused: bool,
}

fn default_max_backups() -> usize {
  3
}

fn default_include_paths() -> bool {
  true
}

fn default_pin_transitives() -> bool {
  true
}

fn default_transitive_host() -> TransitiveFeatureHost {
  TransitiveFeatureHost::Root
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

/// Configuration for where to add dev-dependencies when consolidating transitive features
#[derive(Debug, Clone, PartialEq, Default)]
pub enum TransitiveFeatureHost {
  /// Use workspace root Cargo.toml (default)
  #[default]
  Root,
  /// Use a specific member crate (relative path from workspace root)
  Path(String),
}

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

// Custom serialization/deserialization
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

    impl<'de> serde::de::Visitor<'de> for TransitiveFeatureHostVisitor {
      type Value = TransitiveFeatureHost;

      fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("'root' or a path string")
      }

      fn visit_str<E>(self, value: &str) -> Result<TransitiveFeatureHost, E>
      where
        E: serde::de::Error,
      {
        match value {
          "root" | "auto" => Ok(TransitiveFeatureHost::Root), // "auto" for backward compatibility
          path => Ok(TransitiveFeatureHost::Path(path.to_string())),
        }
      }
    }

    deserializer.deserialize_any(TransitiveFeatureHostVisitor)
  }
}

// TransitiveFeatureHost doesn't need a resolve method anymore
// The unify command handles path resolution directly

impl Default for UnifyConfig {
  fn default() -> Self {
    Self {
      include_paths: default_include_paths(),
      include_renamed: false,
      pin_transitives: default_pin_transitives(),
      transitive_host: default_transitive_host(),
      exclude: Vec::new(),
      include: Vec::new(),
      max_backups: default_max_backups(),
      msrv: false,
      prune_dead_features: false,
      strict_version_compat: true,
      exact_pin_handling: ExactPinHandling::default(),
      detect_unused: false,
      remove_unused: false,
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

/// Release configuration (workspace-wide defaults)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseConfig {
  /// Git tag prefix (default: "v")
  #[serde(default = "default_tag_prefix")]
  pub tag_prefix: String,

  /// Tag format template (default: "{crate}-v{version}" for monorepos, "v{version}" for single crates)
  /// Variables: {crate}, {version}
  #[serde(default = "default_tag_format")]
  pub tag_format: String,

  /// Require clean working directory before release (default: true)
  #[serde(default = "default_true")]
  pub require_clean: bool,

  /// Delay between crate publishes in seconds (default: 5)
  #[serde(default = "default_publish_delay")]
  pub publish_delay: u64,

  /// Create GitHub releases via gh CLI (default: false)
  #[serde(default)]
  pub create_github_release: bool,

  /// Sign git tags with GPG/SSH (default: false)
  #[serde(default)]
  pub sign_tags: bool,

  /// Default changelog path for all crates (default: "CHANGELOG.md")
  #[serde(default = "default_changelog_path")]
  pub changelog_path: String,

  /// Crates that should not generate changelog entries
  #[serde(default)]
  pub skip_changelog_for: Vec<String>,

  /// If true, error when there are no changelog entries for a crate
  #[serde(default)]
  pub require_changelog_entries: bool,
}

impl Default for ReleaseConfig {
  fn default() -> Self {
    Self {
      tag_prefix: default_tag_prefix(),
      tag_format: default_tag_format(),
      require_clean: true,
      publish_delay: default_publish_delay(),
      create_github_release: false,
      sign_tags: false,
      changelog_path: default_changelog_path(),
      skip_changelog_for: Vec::new(),
      require_changelog_entries: false,
    }
  }
}

/// Configuration for TOML formatting
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FormattingConfig {
  /// Add "Managed by cargo-rail" header (default: false)
  #[serde(default)]
  pub add_header: bool,
}

/// Configuration for change detection (`cargo rail affected`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeDetectionConfig {
  /// Glob patterns for infrastructure files that trigger rebuild_all (default: [".github/**", "scripts/**", "justfile", "Makefile"])
  #[serde(default = "default_infrastructure_patterns")]
  pub infrastructure: Vec<String>,

  /// Custom path patterns and their categories
  /// Example: verify = ["verify/**/*.rs"] for Stateright verification models
  #[serde(default)]
  pub custom: std::collections::HashMap<String, Vec<String>>,
}

impl Default for ChangeDetectionConfig {
  fn default() -> Self {
    Self {
      infrastructure: default_infrastructure_patterns(),
      custom: std::collections::HashMap::new(),
    }
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

fn default_tag_prefix() -> String {
  "v".to_string()
}

fn default_tag_format() -> String {
  "{crate}-v{version}".to_string()
}

fn default_publish_delay() -> u64 {
  5
}

fn default_changelog_path() -> String {
  "CHANGELOG.md".to_string()
}

fn default_true() -> bool {
  true
}

/// Configuration for splitting/syncing a crate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitConfig {
  /// Crate name
  pub name: String,
  /// Remote repository URL or local path
  pub remote: String,
  /// Git branch to use
  pub branch: String,
  /// Split mode (single or combined)
  pub mode: SplitMode,
  /// For combined mode: how to structure the split repo
  #[serde(default)]
  pub workspace_mode: WorkspaceMode,
  /// Crate paths to include in the split
  #[serde(default)]
  pub paths: Vec<CratePath>,
  /// Additional files/directories to include
  #[serde(default)]
  pub include: Vec<String>,
  /// Files/directories to exclude
  #[serde(default)]
  pub exclude: Vec<String>,

  /// Release configuration: enable/disable publishing for this crate
  #[serde(default = "default_true")]
  pub publish: bool,

  /// Per-crate changelog path override (default: CHANGELOG.md)
  #[serde(default)]
  pub changelog_path: Option<PathBuf>,
}

/// Path to a crate in the workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CratePath {
  /// Path to the crate directory
  #[serde(rename = "crate")]
  pub path: PathBuf,
}

/// Split mode: single crate or combined multi-crate
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SplitMode {
  /// Single crate per repository
  #[default]
  Single,
  /// Multiple crates in one repository
  Combined,
}

/// How to structure a combined split repository
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceMode {
  /// Multiple standalone crates in one repo (no workspace structure)
  #[default]
  Standalone,
  /// Workspace structure with root Cargo.toml (mirrors monorepo)
  Workspace,
}

impl RailConfig {
  /// Find config file in search order: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml
  ///
  /// On Windows, this handles path canonicalization issues (UNC paths, 8.3 short names)
  /// by checking both the original path and its parent's canonicalization.
  pub fn find_config_path(path: &Path) -> Option<PathBuf> {
    let candidates = [
      path.join("rail.toml"),
      path.join(".rail.toml"),
      path.join(".cargo").join("rail.toml"),
      path.join(".config").join("rail.toml"),
    ];

    // First, try the candidates as-is
    if let Some(found) = candidates.iter().find(|p| p.exists()) {
      return Some(found.to_path_buf());
    }

    // On Windows, if path is canonicalized (e.g., from cargo metadata),
    // we may need to check using the original non-canonicalized path.
    // We do this by checking if a de-canonicalized version exists.
    #[cfg(target_os = "windows")]
    {
      // 1. Try canonicalizing the path and searching there
      // This handles 8.3 short paths vs long paths issues (RUNNER~1 vs runneradmin)
      if let Ok(canonical) = path.canonicalize() {
        let canonical_candidates = [
          canonical.join("rail.toml"),
          canonical.join(".rail.toml"),
          canonical.join(".cargo").join("rail.toml"),
          canonical.join(".config").join("rail.toml"),
        ];
        if let Some(found) = canonical_candidates.iter().find(|p| p.exists()) {
          return Some(found.to_path_buf());
        }
      }

      // 2. Try to find the config by reading the directory entries
      // This handles cases where exact path string matching fails but the file is in the directory
      if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
          let file_name = entry.file_name();
          let file_name_str = file_name.to_string_lossy();

          // Check if this entry matches any of our config file names
          if file_name_str == "rail.toml" || file_name_str == ".rail.toml" {
            return Some(entry.path());
          }
        }
      }

      // Also check subdirectories .cargo and .config via read_dir
      for subdir in &[".cargo", ".config"] {
        let subdir_path = path.join(subdir);
        if let Ok(entries) = std::fs::read_dir(&subdir_path) {
          for entry in entries.flatten() {
            let file_name = entry.file_name();
            if file_name.to_string_lossy() == "rail.toml" {
              return Some(entry.path());
            }
          }
        }
      }
    }

    None
  }

  /// Load config from rail.toml (searches multiple locations)
  pub fn load(path: &Path) -> RailResult<Self> {
    let config_path = Self::find_config_path(path).ok_or_else(|| {
      RailError::Config(ConfigError::NotFound {
        workspace_root: path.to_path_buf(),
      })
    })?;

    let content = fs::read_to_string(&config_path)
      .with_context(|| format!("Failed to read config from {}", config_path.display()))?;
    let config: RailConfig = toml_edit::de::from_str(&content)
      .with_context(|| format!("Failed to parse config from {}", config_path.display()))?;

    Ok(config)
  }

  /// Get all crates that have split configuration
  pub fn get_split_crates(&self) -> Vec<(&str, &CrateSplitConfig)> {
    self
      .crates
      .iter()
      .filter_map(|(name, config)| config.split.as_ref().map(|split| (name.as_str(), split)))
      .collect()
  }

  /// Helper to build all SplitConfigs from unified crate config
  /// Used for backward compatibility with commands expecting `Vec<SplitConfig>`
  pub fn build_split_configs(&self) -> Vec<SplitConfig> {
    self
      .crates
      .iter()
      .filter_map(|(name, config)| {
        config.split.as_ref().map(|split| SplitConfig {
          name: name.clone(),
          remote: split.remote.clone(),
          branch: split.branch.clone(),
          mode: split.mode.clone(),
          workspace_mode: split.workspace_mode.clone(),
          paths: split.paths.clone(),
          include: split.include.clone(),
          exclude: split.exclude.clone(),
          publish: config.release.as_ref().map(|r| r.publish).unwrap_or(true),
          changelog_path: config.changelog.as_ref().and_then(|c| c.path.clone()),
        })
      })
      .collect()
  }
}

impl SplitConfig {
  /// Get the path(s) for this split configuration
  pub fn get_paths(&self) -> Vec<&PathBuf> {
    self.paths.iter().map(|cp| &cp.path).collect()
  }

  /// Determine the target repository path for this split configuration
  ///
  /// For local paths (testing), returns the path as-is.
  /// For remote URLs, extracts the repo name and places it adjacent to workspace root.
  pub fn target_repo_path(&self, workspace_root: &Path) -> PathBuf {
    if crate::utils::is_local_path(&self.remote) {
      PathBuf::from(&self.remote)
    } else {
      let remote_name = self
        .remote
        .rsplit('/')
        .next()
        .unwrap_or(&self.name)
        .trim_end_matches(".git");
      workspace_root.join("..").join(remote_name)
    }
  }

  /// Check if this split is using a local path (testing mode)
  pub fn is_local_testing(&self) -> bool {
    crate::utils::is_local_path(&self.remote)
  }

  /// Validate the split configuration
  pub fn validate(&self) -> RailResult<()> {
    // Check paths exist
    if self.paths.is_empty() {
      return Err(RailError::with_help(
        format!("Split '{}' must have at least one crate path", self.name),
        "Add at least one crate path in rail.toml under [[splits]]",
      ));
    }

    // Check remote is not empty
    if self.remote.is_empty() {
      return Err(RailError::Config(ConfigError::MissingField {
        field: format!("remote for split '{}'", self.name),
      }));
    }

    // Validate mode-specific requirements
    match self.mode {
      SplitMode::Single => {
        if self.paths.len() != 1 {
          return Err(RailError::with_help(
            format!(
              "Single mode split '{}' must have exactly one path (found {})",
              self.name,
              self.paths.len()
            ),
            "Change mode to 'combined' or remove extra paths",
          ));
        }
      }
      SplitMode::Combined => {
        if self.paths.len() < 2 {
          return Err(RailError::with_help(
            format!(
              "Combined mode split '{}' should have multiple paths (found {})",
              self.name,
              self.paths.len()
            ),
            "Change mode to 'single' or add more crate paths",
          ));
        }
      }
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  // Tests for remaining configs can go here

  // ============================================================================
  // Unify Config Tests
  // ============================================================================

  #[test]
  fn test_unify_config_defaults() {
    let config = UnifyConfig::default();
    assert!(config.include_paths); // Default: true
    assert!(!config.include_renamed); // Default: false
    assert!(config.pin_transitives); // Default: true
    assert_eq!(config.transitive_host, TransitiveFeatureHost::Root);
    assert!(config.exclude.is_empty());
    assert!(config.include.is_empty());
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

  // ============================================================================
  // TransitiveFeatureHost Tests
  // ============================================================================
  // Note: Detailed TOML serialization tests will be added during TOML formatting overhaul

  #[test]
  fn test_transitive_feature_host_in_full_config() {
    // Test that the simplified config works with TOML
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
    assert!(config.pin_transitives); // Default is true (workspace-hack replacement)
  }

  // ============================================================================
  // Split Config Validation Tests
  // ============================================================================

  #[test]
  fn test_split_config_validate_empty_paths() {
    let config = SplitConfig {
      name: "test-crate".to_string(),
      remote: "git@github.com:user/test.git".to_string(),
      branch: "main".to_string(),
      mode: SplitMode::Single,
      workspace_mode: WorkspaceMode::default(),
      paths: vec![],
      include: vec![],
      exclude: vec![],
      publish: true,
      changelog_path: None,
    };

    let result = config.validate();
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("at least one crate path"));
  }

  #[test]
  fn test_split_config_validate_empty_remote() {
    let config = SplitConfig {
      name: "test-crate".to_string(),
      remote: "".to_string(),
      branch: "main".to_string(),
      mode: SplitMode::Single,
      workspace_mode: WorkspaceMode::default(),
      paths: vec![CratePath {
        path: PathBuf::from("crates/test"),
      }],
      include: vec![],
      exclude: vec![],
      publish: true,
      changelog_path: None,
    };

    let result = config.validate();
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("remote"));
  }

  #[test]
  fn test_split_config_validate_single_mode_multiple_paths() {
    let config = SplitConfig {
      name: "test-crate".to_string(),
      remote: "git@github.com:user/test.git".to_string(),
      branch: "main".to_string(),
      mode: SplitMode::Single,
      workspace_mode: WorkspaceMode::default(),
      paths: vec![
        CratePath {
          path: PathBuf::from("crates/a"),
        },
        CratePath {
          path: PathBuf::from("crates/b"),
        },
      ],
      include: vec![],
      exclude: vec![],
      publish: true,
      changelog_path: None,
    };

    let result = config.validate();
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("Single mode"));
    assert!(err_msg.contains("exactly one path"));
  }

  #[test]
  fn test_split_config_validate_combined_mode_single_path() {
    let config = SplitConfig {
      name: "test-crate".to_string(),
      remote: "git@github.com:user/test.git".to_string(),
      branch: "main".to_string(),
      mode: SplitMode::Combined,
      workspace_mode: WorkspaceMode::default(),
      paths: vec![CratePath {
        path: PathBuf::from("crates/a"),
      }],
      include: vec![],
      exclude: vec![],
      publish: true,
      changelog_path: None,
    };

    let result = config.validate();
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("Combined mode"));
    assert!(err_msg.contains("multiple paths"));
  }

  #[test]
  fn test_split_config_validate_valid() {
    let config = SplitConfig {
      name: "test-crate".to_string(),
      remote: "git@github.com:user/test.git".to_string(),
      branch: "main".to_string(),
      mode: SplitMode::Single,
      workspace_mode: WorkspaceMode::default(),
      paths: vec![CratePath {
        path: PathBuf::from("crates/test"),
      }],
      include: vec![],
      exclude: vec![],
      publish: true,
      changelog_path: None,
    };

    assert!(config.validate().is_ok());
  }

  #[test]
  fn test_prune_dead_features_default() {
    let config = UnifyConfig::default();
    assert!(!config.prune_dead_features);
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

  // ============================================================================
  // New Config Options Tests
  // ============================================================================

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
    assert!(!config.detect_unused);
    assert!(!config.remove_unused);
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
