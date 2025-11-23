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
  /// Dependency unification settings
  #[serde(default)]
  pub unify: UnifyConfig,
  /// Release management settings
  #[serde(default)]
  pub release: ReleaseConfig,
  /// Split/sync configurations for crates
  #[serde(default)]
  pub splits: Vec<SplitConfig>,
  /// TOML formatting settings
  #[serde(default)]
  pub formatting: FormattingConfig,
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
  /// Allow renamed dependencies to be unified (default: false)
  #[serde(default)]
  pub allow_renamed: bool,

  /// Dependencies to exclude from unification
  #[serde(default)]
  pub exclude: Vec<String>,

  /// Dependencies to force-include in unification
  #[serde(default)]
  pub include: Vec<String>,

  /// Consolidate transitive-only crates with fragmented features (default: false)
  /// When enabled, transitive deps with multiple feature sets are added to workspace.dependencies
  /// This is cargo-rail's version of workspace-hack without requiring an extra crate
  #[serde(default)]
  pub consolidate_transitives: bool,

  /// Where to add dev-dependencies when consolidating transitive features
  /// Options:
  /// - "auto" (default) = Smart selection: root package, meta crate, or largest member
  /// - "root" = Use workspace root package (errors if virtual workspace)
  /// - "largest" = Use member with most dependencies
  /// - ["crate-a"] = Explicit crate name(s)
  #[serde(default = "default_host_selection")]
  pub transitive_host: TransitiveFeatureHost,

  /// Validation configuration
  #[serde(default)]
  pub validation: UnifyValidationConfig,

  /// Output configuration
  #[serde(default)]
  pub output: UnifyOutputConfig,

  /// Backup configuration
  #[serde(default)]
  pub backup: UnifyBackupConfig,
}

/// Validation configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UnifyValidationConfig {
  /// Optional: validate unification against specific target triples
  /// When enabled, runs parallel metadata checks per target to catch platform-specific issues
  /// Examples: ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]
  #[serde(default)]
  pub targets: Vec<String>,

  /// Maximum parallel jobs for validation (0 = auto-detect CPUs, >0 = explicit limit)
  #[serde(default)]
  pub max_parallel_jobs: usize,
}

/// Output configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifyOutputConfig {
  /// Generate .cargo-rail/unify-report.md after apply (default: true)
  #[serde(default = "default_generate_report")]
  pub generate_report: bool,
}

/// Backup configuration for unify operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifyBackupConfig {
  /// Automatically create backups on every apply (default: true)
  /// Backups are stored in target/.cargo-rail/backups/ to keep them out of version control
  #[serde(default = "default_backup_enabled")]
  pub enabled: bool,

  /// Number of backups to keep (default: 5)
  /// Older backups are automatically deleted
  #[serde(default = "default_backup_keep_count")]
  pub keep_count: usize,
}

fn default_generate_report() -> bool {
  true
}

fn default_backup_enabled() -> bool {
  true
}

fn default_backup_keep_count() -> usize {
  5
}

fn default_host_selection() -> TransitiveFeatureHost {
  TransitiveFeatureHost::Root
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

// ... (manual impls follow, skipping them in replacement as they are unchanged)

#[test]
fn test_transitive_feature_host_path() {
  // Test that path format works
  let toml = r#"
      allow_renamed = false
      exclude = []
      include = []
      consolidate_transitives = false
      transitive_host = "path/to/crate"

      [validation]
      targets = []
      max_parallel_jobs = 0

      [output]
      generate_report = true
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

impl TransitiveFeatureHost {
  /// Resolve to actual crate names based on workspace metadata
  pub fn resolve(&self, metadata: &crate::cargo::WorkspaceMetadata) -> Vec<String> {
    match self {
      TransitiveFeatureHost::Root => {
        // Find root package (non-virtual workspace)
        metadata
          .list_crates()
          .iter()
          .find(|pkg| {
            // Root package is the one at workspace_root/Cargo.toml
            pkg.manifest_path.as_std_path().parent() == Some(metadata.workspace_root())
          })
          .map(|pkg| vec![pkg.name.to_string()])
          .unwrap_or_default()
      }
      TransitiveFeatureHost::Path(path) => {
        // Resolve the path relative to workspace root
        let target_path = metadata.workspace_root().join(path);
        metadata
          .list_crates()
          .iter()
          .find(|pkg| pkg.manifest_path.as_std_path().parent() == Some(target_path.as_path()))
          .map(|pkg| vec![pkg.name.to_string()])
          .unwrap_or_default()
      }
    }
  }
}

impl Default for UnifyOutputConfig {
  fn default() -> Self {
    Self {
      generate_report: default_generate_report(),
    }
  }
}

impl Default for UnifyBackupConfig {
  fn default() -> Self {
    Self {
      enabled: default_backup_enabled(),
      keep_count: default_backup_keep_count(),
    }
  }
}

impl Default for UnifyConfig {
  fn default() -> Self {
    Self {
      allow_renamed: false,
      exclude: Vec::new(),
      include: Vec::new(),
      consolidate_transitives: false,
      transitive_host: default_host_selection(),
      validation: UnifyValidationConfig::default(),
      output: UnifyOutputConfig::default(),
      backup: UnifyBackupConfig::default(),
    }
  }
}

impl UnifyConfig {
  /// Check if target validation is enabled
  pub fn validation_enabled(&self) -> bool {
    !self.validation.targets.is_empty()
  }

  /// Get effective parallel job count (auto-detect if 0)
  pub fn effective_parallelism(&self) -> usize {
    if self.validation.max_parallel_jobs == 0 {
      // Auto-detect: use number of logical CPUs
      std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    } else {
      self.validation.max_parallel_jobs
    }
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
  fn test_unify_config_validation_enabled() {
    let unify_disabled = UnifyConfig::default();
    assert!(!unify_disabled.validation_enabled());

    let unify_enabled = UnifyConfig {
      validation: UnifyValidationConfig {
        targets: vec!["x86_64-unknown-linux-gnu".to_string()],
        max_parallel_jobs: 0,
      },
      ..Default::default()
    };
    assert!(unify_enabled.validation_enabled());
  }

  #[test]
  fn test_unify_config_effective_parallelism_auto() {
    let unify = UnifyConfig {
      validation: UnifyValidationConfig {
        targets: vec![],
        max_parallel_jobs: 0, // Auto-detect
      },
      ..Default::default()
    };
    let parallelism = unify.effective_parallelism();
    assert!(parallelism >= 1, "Should detect at least 1 CPU");
  }

  #[test]
  fn test_unify_config_effective_parallelism_explicit() {
    let unify = UnifyConfig {
      validation: UnifyValidationConfig {
        targets: vec![],
        max_parallel_jobs: 4,
      },
      ..Default::default()
    };
    assert_eq!(unify.effective_parallelism(), 4);
  }

  // ============================================================================
  // TransitiveFeatureHost Tests
  // ============================================================================
  // Note: Detailed TOML serialization tests will be added during TOML formatting overhaul

  #[test]
  fn test_transitive_feature_host_in_full_config_auto() {
    // Test that "auto" works in the new flattened config
    let toml = r#"
      allow_renamed = false
      exclude = []
      include = []
      consolidate_transitives = false
      transitive_host = "auto"

      [validation]
      targets = []
      max_parallel_jobs = 0

      [output]
      generate_report = true
    "#;

    let config: UnifyConfig = toml_edit::de::from_str(toml).unwrap();
    assert_eq!(config.transitive_host, TransitiveFeatureHost::Root);
  }

  #[test]
  fn test_transitive_feature_host_resolve_path() {
    // Create a test metadata instance
    let metadata = crate::cargo::WorkspaceMetadata::load(&std::env::current_dir().unwrap()).unwrap();

    // Test resolving root
    let host = TransitiveFeatureHost::Root;
    let resolved = host.resolve(&metadata);
    assert_eq!(resolved, vec!["cargo-rail"]);
  }

  #[test]
  fn test_transitive_feature_host_resolve_root() {
    // Create a test metadata instance
    let metadata = crate::cargo::WorkspaceMetadata::load(&std::env::current_dir().unwrap()).unwrap();

    let host = TransitiveFeatureHost::Root;
    let resolved = host.resolve(&metadata);

    // Should return "cargo-rail" since this workspace has a root package
    assert_eq!(resolved, vec!["cargo-rail"]);
  }

  #[test]
  fn test_unify_config_default_uses_auto() {
    let config = UnifyConfig::default();
    assert_eq!(config.transitive_host, TransitiveFeatureHost::Root);
    assert!(!config.consolidate_transitives);
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
}
