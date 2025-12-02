//! Split configuration - controls crate splitting and syncing behavior

use crate::config::release::ChangelogConfig;
use crate::config::unify::default_true;
use crate::error::{ConfigError, RailError, RailResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Split configuration for a crate (under [crates.X.split])
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

/// Full split configuration (flattened for command use)
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

impl SplitConfig {
  /// Get the path(s) for this split configuration
  pub fn get_paths(&self) -> Vec<&PathBuf> {
    self.paths.iter().map(|cp| &cp.path).collect()
  }

  /// Determine the target repository path for this split configuration
  ///
  /// For local paths (testing), returns the path as-is.
  /// For remote URLs, extracts the repo name and places it adjacent to workspace root.
  pub fn target_repo_path(&self, workspace_root: &std::path::Path) -> PathBuf {
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
        format!("Add paths in rail.toml under [crates.{}.split]", self.name),
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

/// Per-crate sync configuration.
///
/// **Note:** Reserved for future use. Currently has no effect.
/// Will hold sync-specific settings like conflict strategies, exclusion patterns, etc.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CrateSyncConfig {}

/// Helper to build SplitConfig from crate name and CrateSplitConfig
pub fn build_split_config(
  name: String,
  split: &CrateSplitConfig,
  release_publish: Option<bool>,
  changelog: Option<&ChangelogConfig>,
) -> SplitConfig {
  SplitConfig {
    name,
    remote: split.remote.clone(),
    branch: split.branch.clone(),
    mode: split.mode.clone(),
    workspace_mode: split.workspace_mode.clone(),
    paths: split.paths.clone(),
    include: split.include.clone(),
    exclude: split.exclude.clone(),
    publish: release_publish.unwrap_or(true),
    changelog_path: changelog.and_then(|c| c.path.clone()),
  }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

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
