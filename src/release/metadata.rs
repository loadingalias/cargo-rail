//! Release metadata tracking and manipulation
//!
//! Maintains the invariant: every released thing has name, version, last_sha anchor

use crate::core::config::{RailConfig, ReleaseConfig};
use crate::core::error::RailResult;
use chrono::{DateTime, Utc};
use std::path::Path;

/// Release metadata tracker
///
/// Provides operations to read, update, and persist release metadata
/// stored in rail.toml `[[releases]]` sections.
pub struct ReleaseTracker {
  config: RailConfig,
  workspace_root: std::path::PathBuf,
}

impl ReleaseTracker {
  /// Load release tracker from workspace
  pub fn load(workspace_root: &Path) -> RailResult<Self> {
    let config = RailConfig::load(workspace_root)?;
    Ok(Self {
      config,
      workspace_root: workspace_root.to_path_buf(),
    })
  }

  /// Get all configured releases
  pub fn releases(&self) -> &[ReleaseConfig] {
    &self.config.releases
  }

  /// Find a release by name
  pub fn find_release(&self, name: &str) -> Option<&ReleaseConfig> {
    self.config.releases.iter().find(|r| r.name == name)
  }

  /// Find a release by crate path
  #[allow(dead_code)] // TODO(Pillar 4): Use for auto-detecting releases from crate changes
  pub fn find_release_by_crate(&self, crate_path: &Path) -> Option<&ReleaseConfig> {
    self.config.releases.iter().find(|r| r.crate_path == crate_path)
  }

  /// Update release metadata after a successful release
  ///
  /// This maintains the invariant: every released thing has version + last_sha anchor
  pub fn update_release(&mut self, name: &str, version: &str, sha: &str) -> RailResult<()> {
    let release = self
      .config
      .releases
      .iter_mut()
      .find(|r| r.name == name)
      .ok_or_else(|| crate::core::error::RailError::message(format!("Release '{}' not found in config", name)))?;

    release.last_version = Some(version.to_string());
    release.last_sha = Some(sha.to_string());
    release.last_date = Some(Utc::now().to_rfc3339());

    Ok(())
  }

  /// Save updated configuration back to rail.toml
  pub fn save(&self) -> RailResult<()> {
    self.config.save(&self.workspace_root)
  }

  /// Get releases that have never been released (first time)
  #[allow(dead_code)] // TODO(Pillar 4): Use for release recommendations
  pub fn unreleased(&self) -> Vec<&ReleaseConfig> {
    self.config.releases.iter().filter(|r| r.is_first_release()).collect()
  }

  /// Get releases that have been released before
  #[allow(dead_code)] // TODO(Pillar 4): Use for release history analysis
  pub fn released(&self) -> Vec<&ReleaseConfig> {
    self.config.releases.iter().filter(|r| !r.is_first_release()).collect()
  }
}

/// Release metadata snapshot
#[allow(dead_code)] // TODO(Pillar 4): Use for JSON output in commands
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReleaseMetadata {
  pub name: String,
  pub crate_path: std::path::PathBuf,
  pub current_version: String,
  pub last_sha: Option<String>,
  pub last_date: Option<DateTime<Utc>>,
  pub has_split: bool,
  pub split_name: Option<String>,
  pub is_first_release: bool,
}

impl ReleaseMetadata {
  /// Create metadata snapshot from config
  #[allow(dead_code)] // TODO(Pillar 4): Use for detailed release info
  pub fn from_config(release: &ReleaseConfig) -> Self {
    let last_date = release
      .last_date
      .as_ref()
      .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
      .map(|d| d.with_timezone(&Utc));

    Self {
      name: release.name.clone(),
      crate_path: release.crate_path.clone(),
      current_version: release.last_version.clone().unwrap_or_else(|| "0.0.0".to_string()),
      last_sha: release.last_sha.clone(),
      last_date,
      has_split: release.has_split(),
      split_name: release.split.clone(),
      is_first_release: release.is_first_release(),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_release_metadata_first_release() {
    let config = ReleaseConfig {
      name: "test-crate".to_string(),
      crate_path: "crates/test".into(),
      split: None,
      last_version: None,
      last_sha: None,
      last_date: None,
    };

    assert!(config.is_first_release());
    assert!(!config.has_split());
  }

  #[test]
  fn test_release_metadata_with_history() {
    let config = ReleaseConfig {
      name: "test-crate".to_string(),
      crate_path: "crates/test".into(),
      split: Some("test".to_string()),
      last_version: Some("0.1.0".to_string()),
      last_sha: Some("abc123".to_string()),
      last_date: Some("2025-01-15T10:00:00Z".to_string()),
    };

    assert!(!config.is_first_release());
    assert!(config.has_split());
    assert_eq!(config.current_version().to_string(), "0.1.0");
  }
}
