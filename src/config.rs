use crate::error::{ConfigError, RailError, RailResult, ResultExt};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Configuration for cargo-rail
/// Searched in order: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RailConfig {
  pub workspace: WorkspaceConfig,
  #[serde(default)]
  pub unify: UnifyConfig,
  #[serde(default)]
  pub release: ReleaseConfig,
  #[serde(default)]
  pub splits: Vec<SplitConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
  pub root: PathBuf,
}

/// Unify configuration - controls workspace dependency unification behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifyConfig {
  /// Use --all-features when gathering metadata (default: true)
  /// This ensures the feature union across all workspace members is captured
  #[serde(default = "default_use_all_features")]
  pub use_all_features: bool,

  /// Allow renamed dependencies to be unified (default: false)
  #[serde(default)]
  pub allow_renamed: bool,

  /// Dependencies to exclude from unification
  #[serde(default)]
  pub exclude: Vec<String>,

  /// Dependencies to force-include in unification
  #[serde(default)]
  pub include: Vec<String>,

  /// Optional: validate unification against specific target triples
  /// When enabled, runs parallel metadata checks per target to catch platform-specific issues
  /// Examples: ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]
  #[serde(default)]
  pub validate_targets: Vec<String>,

  /// Maximum parallel jobs for validation (0 = auto-detect CPUs, >0 = explicit limit)
  #[serde(default)]
  pub max_parallel_jobs: usize,

  /// Automatically pin transitive-only crates with fragmented features (default: false)
  /// When enabled, transitive deps with multiple feature sets are added to workspace.dependencies
  #[serde(default)]
  pub pin_transitives: bool,

  /// Crates to add dev-dependencies to when pinning transitives (default: empty = auto-select)
  /// Examples: ["workspace-root"], ["meta-crate"], ["crate-a", "crate-b"]
  #[serde(default)]
  pub pin_hosts: Vec<String>,

  /// Automatically resolve version conflicts by picking the highest version (default: true)
  /// When enabled, unify will proceed with highest version and add warning comments
  /// When disabled, version conflicts are hard blockers
  #[serde(default = "default_auto_resolve_version_conflicts")]
  pub auto_resolve_version_conflicts: bool,

  /// Conflict resolution mode (default: "permissive")
  /// - "permissive": Soft warnings don't block unification (recommended)
  /// - "strict": All conflicts block unification
  #[serde(default = "default_conflict_resolution")]
  pub conflict_resolution: String,

  /// Add conflict marker comments to Cargo.toml files (default: true)
  /// Adds # ⚠️ markers to help identify manual resolution needs
  #[serde(default = "default_add_conflict_comments")]
  pub add_conflict_comments: bool,

  /// Generate .cargo-rail/unify-report.md after apply (default: true)
  #[serde(default = "default_generate_report")]
  pub generate_report: bool,
}

fn default_use_all_features() -> bool {
  true
}

fn default_auto_resolve_version_conflicts() -> bool {
  true
}

fn default_conflict_resolution() -> String {
  "permissive".to_string()
}

fn default_add_conflict_comments() -> bool {
  true
}

fn default_generate_report() -> bool {
  true
}

impl Default for UnifyConfig {
  fn default() -> Self {
    Self {
      use_all_features: default_use_all_features(),
      validate_targets: vec![],
      max_parallel_jobs: 0, // Auto-detect
      pin_transitives: false,
      pin_hosts: vec![],
      auto_resolve_version_conflicts: default_auto_resolve_version_conflicts(),
      conflict_resolution: default_conflict_resolution(),
      add_conflict_comments: default_add_conflict_comments(),
      generate_report: default_generate_report(),
      allow_renamed: false,
      exclude: Vec::new(),
      include: Vec::new(),
    }
  }
}

impl UnifyConfig {
  /// Check if target validation is enabled
  pub fn validation_enabled(&self) -> bool {
    !self.validate_targets.is_empty()
  }

  /// Get effective parallel job count (auto-detect if 0)
  pub fn effective_parallelism(&self) -> usize {
    if self.max_parallel_jobs == 0 {
      // Auto-detect: use number of logical CPUs
      std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    } else {
      self.max_parallel_jobs
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitConfig {
  pub name: String,
  pub remote: String,
  pub branch: String,
  pub mode: SplitMode,
  /// For combined mode: how to structure the split repo
  #[serde(default)]
  pub workspace_mode: WorkspaceMode,
  #[serde(default)]
  pub paths: Vec<CratePath>,
  #[serde(default)]
  pub include: Vec<String>,
  #[serde(default)]
  pub exclude: Vec<String>,

  /// Release configuration: enable/disable publishing for this crate
  #[serde(default = "default_true")]
  pub publish: bool,

  /// Per-crate changelog path override (default: CHANGELOG.md)
  #[serde(default)]
  pub changelog_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CratePath {
  #[serde(rename = "crate")]
  pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SplitMode {
  #[default]
  Single,
  Combined,
}

/// How to structure a combined split repository
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
  fn test_unify_config_default() {
    let unify = UnifyConfig::default();
    assert!(unify.use_all_features);
    assert!(unify.validate_targets.is_empty());
    assert_eq!(unify.max_parallel_jobs, 0); // Auto-detect
  }

  #[test]
  fn test_unify_config_validation_enabled() {
    let unify_disabled = UnifyConfig::default();
    assert!(!unify_disabled.validation_enabled());

    let unify_enabled = UnifyConfig {
      validate_targets: vec!["x86_64-unknown-linux-gnu".to_string()],
      ..Default::default()
    };
    assert!(unify_enabled.validation_enabled());
  }

  #[test]
  fn test_unify_config_effective_parallelism_auto() {
    let unify = UnifyConfig {
      max_parallel_jobs: 0, // Auto-detect
      ..Default::default()
    };
    let parallelism = unify.effective_parallelism();
    assert!(parallelism >= 1, "Should detect at least 1 CPU");
  }

  #[test]
  fn test_unify_config_effective_parallelism_explicit() {
    let unify = UnifyConfig {
      max_parallel_jobs: 4,
      ..Default::default()
    };
    assert_eq!(unify.effective_parallelism(), 4);
  }

  #[test]
  fn test_unify_config_serialization() {
    let unify = UnifyConfig {
      use_all_features: true,
      validate_targets: vec!["x86_64-unknown-linux-gnu".to_string()],
      max_parallel_jobs: 2,
      pin_transitives: false,
      pin_hosts: vec![],
      auto_resolve_version_conflicts: true,
      conflict_resolution: "permissive".to_string(),
      add_conflict_comments: true,
      generate_report: true,
      allow_renamed: false,
      exclude: vec![],
      include: vec![],
    };

    // Serialize to TOML
    let toml = toml_edit::ser::to_string(&unify).unwrap();
    assert!(toml.contains("use_all_features = true"));
    assert!(toml.contains("x86_64-unknown-linux-gnu"));
    assert!(toml.contains("max_parallel_jobs = 2"));
    assert!(toml.contains("auto_resolve_version_conflicts = true"));
    assert!(toml.contains("conflict_resolution = \"permissive\""));

    // Deserialize back
    let parsed: UnifyConfig = toml_edit::de::from_str(&toml).unwrap();
    assert_eq!(parsed.use_all_features, unify.use_all_features);
    assert_eq!(
      parsed.auto_resolve_version_conflicts,
      unify.auto_resolve_version_conflicts
    );
    assert_eq!(parsed.conflict_resolution, unify.conflict_resolution);
    assert_eq!(parsed.add_conflict_comments, unify.add_conflict_comments);
    assert_eq!(parsed.generate_report, unify.generate_report);
    assert_eq!(parsed.validate_targets, unify.validate_targets);
    assert_eq!(parsed.max_parallel_jobs, unify.max_parallel_jobs);
  }
}
