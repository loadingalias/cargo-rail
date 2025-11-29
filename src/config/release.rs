//! Release configuration - controls release management behavior

use crate::config::unify::default_true;
use crate::error::ConfigError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

  /// What changelog paths are relative to (default: "crate")
  /// - "crate": Paths are relative to each crate's directory
  /// - "workspace": Paths are relative to workspace root
  #[serde(default)]
  pub changelog_relative_to: ChangelogRelativeTo,

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
      changelog_relative_to: ChangelogRelativeTo::default(),
      skip_changelog_for: Vec::new(),
      require_changelog_entries: false,
    }
  }
}

impl ReleaseConfig {
  /// Validate the release configuration
  pub fn validate(&self, workspace_members: &[String]) -> Result<Vec<String>, ConfigError> {
    let mut warnings = Vec::new();

    // Validate tag_format
    if self.tag_format.trim().is_empty() {
      return Err(ConfigError::InvalidField {
        field: "release.tag_format".to_string(),
        reason: "tag_format cannot be empty".to_string(),
      });
    }

    // Check for recommended placeholders in monorepo context
    let is_monorepo = workspace_members.len() > 1;
    if is_monorepo && !self.tag_format.contains("{crate}") {
      warnings.push(
        "release.tag_format does not contain {crate} placeholder. \
                In monorepos, this may cause tag collisions between crates."
          .to_string(),
      );
    }

    if !self.tag_format.contains("{version}") && !self.tag_format.contains("{prefix}") {
      warnings.push(
        "release.tag_format does not contain {version} or {prefix} placeholder. \
                Tags may not be identifiable."
          .to_string(),
      );
    }

    // Validate skip_changelog_for - check that all crate names exist
    for crate_name in &self.skip_changelog_for {
      if !workspace_members.contains(crate_name) {
        warnings.push(format!(
          "release.skip_changelog_for contains unknown crate '{}'. \
                    Available crates: {}",
          crate_name,
          workspace_members.join(", ")
        ));
      }
    }

    Ok(warnings)
  }
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
  /// Path to changelog file
  /// Relative to crate directory (default) or workspace root depending on `relative_to`
  pub path: Option<PathBuf>,
  /// Exclude this crate from changelog generation?
  #[serde(default)]
  pub skip: bool,
}

/// What the changelog_path is relative to
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangelogRelativeTo {
  /// Relative to each crate's directory (default)
  /// With this, `changelog_path = "CHANGELOG.md"` creates `crates/foo/CHANGELOG.md`
  #[default]
  Crate,
  /// Relative to workspace root
  /// With this, `changelog_path = "CHANGELOG.md"` creates `./CHANGELOG.md`
  Workspace,
}

// ============================================================================
// Default Functions
// ============================================================================

fn default_tag_prefix() -> String {
  "v".to_string()
}

fn default_tag_format() -> String {
  // Use {prefix} placeholder so tag_prefix is respected
  // With default tag_prefix="v", this produces: crate-name-v1.0.0
  "{crate}-{prefix}{version}".to_string()
}

fn default_publish_delay() -> u64 {
  5
}

fn default_changelog_path() -> String {
  "CHANGELOG.md".to_string()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_changelog_relative_to_default() {
    let config = ReleaseConfig::default();
    assert_eq!(config.changelog_relative_to, ChangelogRelativeTo::Crate);
  }

  #[test]
  fn test_changelog_relative_to_parsing() {
    // Test "crate" value
    let toml = r#"changelog_relative_to = "crate""#;
    let config: ReleaseConfig = toml_edit::de::from_str(toml).unwrap();
    assert_eq!(config.changelog_relative_to, ChangelogRelativeTo::Crate);

    // Test "workspace" value
    let toml = r#"changelog_relative_to = "workspace""#;
    let config: ReleaseConfig = toml_edit::de::from_str(toml).unwrap();
    assert_eq!(config.changelog_relative_to, ChangelogRelativeTo::Workspace);
  }

  #[test]
  fn test_changelog_relative_to_full_config() {
    let toml = r#"
      changelog_path = "docs/CHANGELOG.md"
      changelog_relative_to = "workspace"
    "#;
    let config: ReleaseConfig = toml_edit::de::from_str(toml).unwrap();
    assert_eq!(config.changelog_path, "docs/CHANGELOG.md");
    assert_eq!(config.changelog_relative_to, ChangelogRelativeTo::Workspace);
  }

  #[test]
  fn test_changelog_relative_to_defaults_to_crate() {
    // When not specified, should default to "crate"
    let toml = r#"
      changelog_path = "CHANGELOG.md"
    "#;
    let config: ReleaseConfig = toml_edit::de::from_str(toml).unwrap();
    assert_eq!(config.changelog_relative_to, ChangelogRelativeTo::Crate);
  }
}
