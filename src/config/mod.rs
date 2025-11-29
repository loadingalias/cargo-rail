//! Configuration for cargo-rail
//!
//! This module provides all configuration types for cargo-rail:
//! - `RailConfig` - Main configuration struct loaded from rail.toml
//! - `UnifyConfig` - Dependency unification settings
//! - `ReleaseConfig` - Release management settings
//! - `SplitConfig` - Crate splitting/syncing settings
//! - `ChangeDetectionConfig` - Change detection settings
//!
//! Configuration is searched in order: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml

mod change_detection;
mod release;
mod split;
mod unify;

// Re-export all public types
pub use change_detection::ChangeDetectionConfig;
pub use release::{ChangelogConfig, ChangelogRelativeTo, CrateReleaseConfig, ReleaseConfig};
pub use split::{CratePath, CrateSplitConfig, CrateSyncConfig, SplitConfig, SplitMode, WorkspaceMode};
pub use unify::{ExactPinHandling, TransitiveFeatureHost, UnifyConfig};

use crate::error::{ConfigError, RailError, RailResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Configuration for cargo-rail
/// Searched in order: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RailConfig {
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
  pub crates: HashMap<String, CrateConfig>,
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
  /// Sync configuration for this crate (placeholder for future use)
  pub sync: Option<CrateSyncConfig>,
}

/// Configuration for TOML formatting (placeholder for future options)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FormattingConfig {
  // Reserved for future use
  // Potential options: indentation style, inline table preferences, etc.
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

  /// Load config from rail.toml (searches multiple locations)
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

    let content = match fs::read_to_string(&config_path) {
      Ok(c) => c,
      Err(e) => {
        return ConfigLoadResult::ParseError {
          path: config_path,
          message: format!("failed to read file: {}", e),
        };
      }
    };

    match toml_edit::de::from_str(&content) {
      Ok(config) => ConfigLoadResult::Loaded(Box::new(config)),
      Err(e) => ConfigLoadResult::ParseError {
        path: config_path,
        message: e.to_string(),
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
