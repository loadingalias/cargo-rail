#![allow(dead_code)]

use crate::core::error::{ConfigError, RailError, RailResult, ResultExt};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Configuration for cargo-rail
/// Searched in order: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RailConfig {
  pub workspace: WorkspaceConfig,
  #[serde(default)]
  pub security: SecurityConfig,
  pub splits: Vec<SplitConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
  pub root: PathBuf,
}

/// Security configuration for mono↔remote syncing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
  /// SSH key path (default: ~/.ssh/id_ed25519 or ~/.ssh/id_rsa)
  #[serde(default)]
  pub ssh_key_path: Option<PathBuf>,

  /// Require SSH signing key for commits (optional, default: false)
  #[serde(default)]
  pub require_signed_commits: bool,

  /// SSH signing key path (default: same as ssh_key_path)
  #[serde(default)]
  pub signing_key_path: Option<PathBuf>,

  /// PR branch pattern for remote→mono syncs (default: "rail/sync/{crate}/{timestamp}")
  #[serde(default = "default_pr_branch_pattern")]
  pub pr_branch_pattern: String,

  /// Protected branches that cannot be directly committed to (default: ["main", "master"])
  #[serde(default = "default_protected_branches")]
  pub protected_branches: Vec<String>,
}

fn default_pr_branch_pattern() -> String {
  "rail/sync/{crate}/{timestamp}".to_string()
}

fn default_protected_branches() -> Vec<String> {
  vec!["main".to_string(), "master".to_string()]
}

impl Default for SecurityConfig {
  fn default() -> Self {
    Self {
      ssh_key_path: None,
      require_signed_commits: false,
      signing_key_path: None,
      pr_branch_pattern: default_pr_branch_pattern(),
      protected_branches: default_protected_branches(),
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitConfig {
  pub name: String,
  pub remote: String,
  pub branch: String,
  pub mode: SplitMode,
  #[serde(default)]
  pub paths: Vec<CratePath>,
  #[serde(default)]
  pub include: Vec<String>,
  #[serde(default)]
  pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CratePath {
  #[serde(rename = "crate")]
  pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitMode {
  Single,
  Combined,
}

impl RailConfig {
  /// Find config file in search order: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml
  fn find_config_path(path: &Path) -> Option<PathBuf> {
    let candidates = vec![
      path.join("rail.toml"),
      path.join(".rail.toml"),
      path.join(".cargo").join("rail.toml"),
      path.join(".config").join("rail.toml"),
    ];

    candidates.into_iter().find(|p| p.exists())
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

  /// Save config to rail.toml (default location)
  pub fn save(&self, path: &Path) -> RailResult<()> {
    let config_path = path.join("rail.toml");
    let content = toml_edit::ser::to_string_pretty(self).context("Failed to serialize config to TOML")?;
    fs::write(&config_path, content).with_context(|| format!("Failed to write config to {}", config_path.display()))?;
    Ok(())
  }

  /// Check if config exists at the given path
  pub fn exists(path: &Path) -> bool {
    Self::find_config_path(path).is_some()
  }

  /// Create a new empty config
  pub fn new(workspace_root: PathBuf) -> Self {
    Self {
      workspace: WorkspaceConfig { root: workspace_root },
      security: SecurityConfig::default(),
      splits: Vec::new(),
    }
  }
}

impl SplitConfig {
  /// Get the path(s) for this split configuration
  pub fn get_paths(&self) -> Vec<&PathBuf> {
    self.paths.iter().map(|cp| &cp.path).collect()
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
