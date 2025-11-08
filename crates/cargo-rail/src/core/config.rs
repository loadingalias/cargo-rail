#![allow(dead_code)]

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Configuration for cargo-rail, stored in .rail/config.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RailConfig {
  pub workspace: WorkspaceConfig,
  pub splits: Vec<SplitConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
  pub root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitConfig {
  pub name: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub path: Option<PathBuf>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub paths: Option<Vec<PathBuf>>,
  pub remote: String,
  pub branch: String,
  pub mode: SplitMode,
  #[serde(default)]
  pub include: Vec<String>,
  #[serde(default)]
  pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitMode {
  Single,
  Combined,
}

impl RailConfig {
  /// Load config from .rail/config.toml
  pub fn load(path: &Path) -> Result<Self> {
    let config_path = path.join(".rail").join("config.toml");
    let content = fs::read_to_string(&config_path)
      .with_context(|| format!("Failed to read config from {}", config_path.display()))?;
    let config: RailConfig =
      toml::from_str(&content).with_context(|| format!("Failed to parse config from {}", config_path.display()))?;
    Ok(config)
  }

  /// Save config to .rail/config.toml
  pub fn save(&self, path: &Path) -> Result<()> {
    let rail_dir = path.join(".rail");
    fs::create_dir_all(&rail_dir)
      .with_context(|| format!("Failed to create .rail directory at {}", rail_dir.display()))?;

    let config_path = rail_dir.join("config.toml");
    let content = toml::to_string_pretty(self).context("Failed to serialize config to TOML")?;
    fs::write(&config_path, content).with_context(|| format!("Failed to write config to {}", config_path.display()))?;
    Ok(())
  }

  /// Check if config exists at the given path
  pub fn exists(path: &Path) -> bool {
    path.join(".rail").join("config.toml").exists()
  }

  /// Create a new empty config
  pub fn new(workspace_root: PathBuf) -> Self {
    Self {
      workspace: WorkspaceConfig { root: workspace_root },
      splits: Vec::new(),
    }
  }
}

impl SplitConfig {
  /// Get the path(s) for this split configuration
  pub fn get_paths(&self) -> Vec<&PathBuf> {
    if let Some(ref path) = self.path {
      vec![path]
    } else if let Some(ref paths) = self.paths {
      paths.iter().collect()
    } else {
      Vec::new()
    }
  }

  /// Validate the split configuration
  pub fn validate(&self) -> Result<()> {
    match self.mode {
      SplitMode::Single => {
        if self.path.is_none() {
          anyhow::bail!("Single mode split '{}' must have 'path' field", self.name);
        }
        if self.paths.is_some() {
          anyhow::bail!("Single mode split '{}' cannot have 'paths' field", self.name);
        }
      }
      SplitMode::Combined => {
        if self.paths.is_none() {
          anyhow::bail!("Combined mode split '{}' must have 'paths' field", self.name);
        }
        if self.path.is_some() {
          anyhow::bail!("Combined mode split '{}' cannot have 'path' field", self.name);
        }
        if let Some(ref paths) = self.paths
          && paths.is_empty()
        {
          anyhow::bail!("Combined mode split '{}' must have at least one path", self.name);
        }
      }
    }
    Ok(())
  }
}
