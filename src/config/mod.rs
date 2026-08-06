//! Typed `rail.toml` configuration and discovery.

mod cache;
mod change_detection;
mod release;
mod run;
pub(crate) mod schema;
mod split;
mod unify;

pub use cache::CacheConfig;
pub use change_detection::{ChangeDetectionConfig, ConfidenceProfile, UnknownFilePolicy};
pub use release::{
  ChangelogConfig, ChangelogFilters, ChangelogRelativeTo, ChangelogShape, CommitPolicy, CrateReleaseConfig, GroupSpec,
  Pre1BreakingBump, ReleaseConfig, ReleaseRemoteEffects, ReleaseSource, RequireChangeFiles, SemverCheckPolicy,
};
pub(crate) use run::{BUILTIN_ACTION_NAMES, MAX_ACTIONS, first_repository_output_overlap};
pub use run::{
  CargoEnvironmentValue, RepositoryAction, RepositoryActionKind, RepositoryEnvironment, RepositoryEnvironmentEntry,
  RepositoryPackageSelection, RunBaseline, RunConfig, RunProfile, is_builtin_profile,
};
pub use split::{CratePath, CrateSplitConfig, SplitConfig, SplitMode, WorkspaceMode};
pub use unify::{
  ConsumerScope, ExactPinHandling, MajorVersionConflict, MsrvPolicy, MsrvSource, TransitiveFeatureHost,
  TransitivePinning, UnifyConfig,
};

use crate::error::{ConfigError, RailError, RailResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// One discovered repository configuration retained across a pre-context cache shortcut.
///
/// Absence is captured as deliberately as file bytes so a configuration change
/// cannot authorize cache work from a different filesystem moment.
pub(crate) struct CapturedDiscoveredConfig {
  workspace_root: PathBuf,
  file: Option<(PathBuf, Vec<u8>)>,
  config: Option<Box<RailConfig>>,
}

impl CapturedDiscoveredConfig {
  pub(crate) fn capture(workspace_root: &Path) -> RailResult<Self> {
    let Some(path) = RailConfig::find_config_path(workspace_root) else {
      return Ok(Self {
        workspace_root: workspace_root.to_path_buf(),
        file: None,
        config: None,
      });
    };
    let (config, bytes) = RailConfig::load_path_with_bytes(&path)?;
    Ok(Self {
      workspace_root: workspace_root.to_path_buf(),
      file: Some((path, bytes)),
      config: Some(Box::new(config)),
    })
  }

  pub(crate) fn config(&self) -> Option<&RailConfig> {
    self.config.as_deref()
  }

  pub(crate) fn cache_enabled(&self) -> bool {
    self.config().is_none_or(|config| config.cache.enabled)
  }

  pub(crate) fn validate_unchanged(&self) -> bool {
    match &self.file {
      None => RailConfig::find_config_path(&self.workspace_root).is_none(),
      Some((path, bytes)) => {
        RailConfig::find_config_path(&self.workspace_root).as_ref() == Some(path)
          && fs::read(path).is_ok_and(|current| current.as_slice() == bytes.as_slice())
      }
    }
  }
}

/// Configuration for cargo-rail
/// Searched in order: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RailConfig {
  /// Target triples for multi-platform validation (workspace-wide)
  /// Detected via `cargo rail init`, used by multiple commands
  #[serde(default)]
  pub targets: Vec<String>,
  /// Build-result cache policy.
  #[serde(default)]
  pub cache: CacheConfig,
  /// Dependency unification settings
  #[serde(default)]
  pub unify: UnifyConfig,
  /// Release management settings
  #[serde(default)]
  pub release: ReleaseConfig,
  /// Change detection settings (for planner classification)
  #[serde(default, rename = "change-detection")]
  pub change_detection: ChangeDetectionConfig,
  /// Run profile settings for `cargo rail run`.
  #[serde(default)]
  pub run: RunConfig,
  /// Per-crate configuration (overrides workspace defaults)
  #[serde(default)]
  pub crates: BTreeMap<String, CrateConfig>,
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
}

/// Result of attempting to load configuration
pub enum ConfigLoadResult {
  /// Config loaded successfully
  Loaded(Box<RailConfig>),
  /// Config file found but failed to parse
  ParseError {
    /// Path to the config file that failed to parse
    path: PathBuf,
    /// Error message describing the parse failure
    message: String,
  },
  /// No config file found
  NotFound,
}

impl RailConfig {
  fn parse_path(path: &Path) -> Result<(Self, Vec<u8>), String> {
    let bytes = fs::read(path).map_err(|error| format!("failed to read file: {error}"))?;
    let content = std::str::from_utf8(&bytes).map_err(|error| format!("file is not valid UTF-8: {error}"))?;
    let doc: toml_edit::DocumentMut = content
      .parse()
      .map_err(|error: toml_edit::TomlError| error.to_string())?;
    for deprecation in schema::present_deprecations(&doc) {
      if let Some(message) = deprecation.spec.deprecation {
        crate::warn!("{} in {}: {}", deprecation.path, path.display(), message);
      }
    }
    let config = toml_edit::de::from_document(doc).map_err(|error| error.to_string())?;
    Ok((config, bytes))
  }

  pub(crate) fn load_path_with_bytes(path: &Path) -> RailResult<(Self, Vec<u8>)> {
    Self::parse_path(path).map_err(|message| {
      RailError::Config(ConfigError::ParseError {
        path: path.to_path_buf(),
        message,
      })
    })
  }

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
      if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
          let file_name = entry.file_name();
          let file_name_str = file_name.to_string_lossy();

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

  /// Load config from rail.toml (searches multiple locations).
  ///
  /// Searches: `rail.toml`, `.rail.toml`, `.cargo/rail.toml`, `.config/rail.toml`
  ///
  /// # Errors
  ///
  /// Returns [`ConfigError::NotFound`] if no config file exists.
  ///
  /// Returns [`ConfigError::ParseError`] if the config file cannot be read or parsed.
  pub fn load(path: &Path) -> RailResult<Self> {
    match Self::try_load(path) {
      ConfigLoadResult::Loaded(config) => Ok(*config),
      ConfigLoadResult::ParseError { path, message } => {
        Err(RailError::Config(ConfigError::ParseError { path, message }))
      }
      ConfigLoadResult::NotFound => Err(RailError::Config(ConfigError::NotFound {
        workspace_root: path.to_path_buf(),
      })),
    }
  }

  /// Try to load config, returning a result that distinguishes between
  /// "not found" and "parse error". This is used by WorkspaceContext to
  /// properly report parse errors instead of silently falling back to defaults.
  pub fn try_load(path: &Path) -> ConfigLoadResult {
    let config_path = match Self::find_config_path(path) {
      Some(p) => p,
      None => return ConfigLoadResult::NotFound,
    };

    match Self::parse_path(&config_path) {
      Ok((config, _)) => ConfigLoadResult::Loaded(Box::new(config)),
      Err(message) => ConfigLoadResult::ParseError {
        path: config_path,
        message,
      },
    }
  }

  /// Get all crates that have split configuration
  pub fn get_split_crates(&self) -> Vec<(&str, &CrateSplitConfig)> {
    self
      .crates
      .iter()
      .filter_map(|(name, config)| config.split.as_ref().map(|split| (name.as_str(), split)))
      .collect()
  }

  /// Build all SplitConfigs from unified crate config
  pub fn build_split_configs(&self) -> Vec<SplitConfig> {
    self
      .crates
      .iter()
      .filter_map(|(name, config)| {
        config.split.as_ref().map(|split_cfg| {
          split::build_split_config(
            name.clone(),
            split_cfg,
            config.release.as_ref().map(|r| r.publish),
            config.changelog.as_ref(),
          )
        })
      })
      .collect()
  }
}
