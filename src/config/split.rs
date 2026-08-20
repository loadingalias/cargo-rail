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
    /// Cargo workspace members included in this repository.
    ///
    /// A single split defaults to the owning `[crates.NAME]` key. Combined
    /// splits name every member explicitly.
    #[serde(default)]
    pub members: Vec<String>,
    /// Legacy path-based member selection accepted only by `config migrate`.
    #[serde(default, rename = "paths", skip_serializing_if = "Vec::is_empty")]
    pub legacy_paths: Vec<CratePath>,
    /// Additional non-Cargo files/directories to include.
    #[serde(default)]
    pub include: Vec<String>,
    /// Paths to remove from the explicit non-Cargo `include` set.
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
    /// Cargo workspace member names included in the split.
    pub members: Vec<String>,
    /// Legacy path-based member selection awaiting explicit migration.
    pub legacy_paths: Vec<CratePath>,
    /// Additional non-Cargo files/directories to include.
    #[serde(default)]
    pub include: Vec<String>,
    /// Paths to remove from the explicit non-Cargo `include` set.
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
        if !self.legacy_paths.is_empty() {
            return Err(RailError::with_help(
                format!("Split '{}' still selects Cargo members by path", self.name),
                "run 'cargo rail config migrate' to replace split.paths with member names",
            ));
        }

        if self.members.is_empty() {
            return Err(RailError::with_help(
                format!("Split '{}' must own at least one Cargo member", self.name),
                format!("Add members under [crates.{}.split]", self.name),
            ));
        }
        let mut unique_members = std::collections::HashSet::with_capacity(self.members.len());
        if let Some(member) = self
            .members
            .iter()
            .find(|member| !unique_members.insert(member.as_str()))
        {
            return Err(RailError::with_help(
                format!("Split '{}' selects Cargo member '{}' more than once", self.name, member),
                "list each split member exactly once",
            ));
        }
        if !self.exclude.is_empty() && self.include.is_empty() {
            return Err(RailError::with_help(
                format!("Split '{}' excludes assets without including any", self.name),
                "split.exclude only narrows explicit non-Cargo assets selected by split.include",
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
                if self.members.len() != 1 {
                    return Err(RailError::with_help(
                        format!(
                            "Single mode split '{}' must have exactly one member (found {})",
                            self.name,
                            self.members.len()
                        ),
                        "change mode to 'combined' or remove extra members",
                    ));
                }
            }
            SplitMode::Combined => {
                if self.members.len() < 2 {
                    return Err(RailError::with_help(
                        format!(
                            "Combined mode split '{}' should have multiple members (found {})",
                            self.name,
                            self.members.len()
                        ),
                        "change mode to 'single' or add more Cargo members",
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

/// Helper to build SplitConfig from crate name and CrateSplitConfig
pub fn build_split_config(
    name: String,
    split: &CrateSplitConfig,
    release_publish: Option<bool>,
    changelog: Option<&ChangelogConfig>,
) -> SplitConfig {
    let members = if split.members.is_empty() {
        vec![name.clone()]
    } else {
        split.members.clone()
    };
    SplitConfig {
        name,
        remote: split.remote.clone(),
        branch: split.branch.clone(),
        mode: split.mode.clone(),
        workspace_mode: split.workspace_mode.clone(),
        members,
        legacy_paths: split.legacy_paths.clone(),
        include: split.include.clone(),
        exclude: split.exclude.clone(),
        publish: release_publish.unwrap_or(true),
        changelog_path: changelog.and_then(|c| c.path.clone()),
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_config_validate_empty_members() {
        let config = SplitConfig {
            name: "test-crate".to_string(),
            remote: "git@github.com:user/test.git".to_string(),
            branch: "main".to_string(),
            mode: SplitMode::Single,
            workspace_mode: WorkspaceMode::default(),
            members: vec![],
            legacy_paths: vec![],
            include: vec![],
            exclude: vec![],
            publish: true,
            changelog_path: None,
        };

        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("at least one Cargo member"));
    }

    #[test]
    fn test_split_config_validate_empty_remote() {
        let config = SplitConfig {
            name: "test-crate".to_string(),
            remote: "".to_string(),
            branch: "main".to_string(),
            mode: SplitMode::Single,
            workspace_mode: WorkspaceMode::default(),
            members: vec!["test-crate".to_string()],
            legacy_paths: vec![],
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
    fn test_split_config_validate_single_mode_multiple_members() {
        let config = SplitConfig {
            name: "test-crate".to_string(),
            remote: "git@github.com:user/test.git".to_string(),
            branch: "main".to_string(),
            mode: SplitMode::Single,
            workspace_mode: WorkspaceMode::default(),
            members: vec!["a".to_string(), "b".to_string()],
            legacy_paths: vec![],
            include: vec![],
            exclude: vec![],
            publish: true,
            changelog_path: None,
        };

        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Single mode"));
        assert!(err_msg.contains("exactly one member"));
    }

    #[test]
    fn test_split_config_validate_combined_mode_single_member() {
        let config = SplitConfig {
            name: "test-crate".to_string(),
            remote: "git@github.com:user/test.git".to_string(),
            branch: "main".to_string(),
            mode: SplitMode::Combined,
            workspace_mode: WorkspaceMode::default(),
            members: vec!["a".to_string()],
            legacy_paths: vec![],
            include: vec![],
            exclude: vec![],
            publish: true,
            changelog_path: None,
        };

        let result = config.validate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Combined mode"));
        assert!(err_msg.contains("multiple members"));
    }

    #[test]
    fn test_split_config_validate_valid() {
        let config = SplitConfig {
            name: "test-crate".to_string(),
            remote: "git@github.com:user/test.git".to_string(),
            branch: "main".to_string(),
            mode: SplitMode::Single,
            workspace_mode: WorkspaceMode::default(),
            members: vec!["test-crate".to_string()],
            legacy_paths: vec![],
            include: vec![],
            exclude: vec![],
            publish: true,
            changelog_path: None,
        };

        config.validate().unwrap();
    }
}
