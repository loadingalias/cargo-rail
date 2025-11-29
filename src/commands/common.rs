//! Common utilities and builders for command implementations

use crate::config::{RailConfig, SplitConfig as ConfigSplitConfig};
use crate::error::{ConfigError, RailError, RailResult};
use crate::split::SplitConfig;
use crate::sync::SyncConfig;
use crate::workspace::WorkspaceContext;
use std::path::PathBuf;
use std::str::FromStr;

/// Standard output format for most commands
///
/// Use this for commands that just need text/json output.
/// For commands with more formats (like `affected`), define a specialized enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
  /// Human-readable text output (default)
  #[default]
  Text,
  /// Machine-readable JSON output
  Json,
}

impl OutputFormat {
  /// Check if this format is JSON
  pub fn is_json(&self) -> bool {
    matches!(self, Self::Json)
  }
}

impl FromStr for OutputFormat {
  type Err = RailError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s.to_lowercase().as_str() {
      "text" => Ok(Self::Text),
      "json" => Ok(Self::Json),
      _ => Err(RailError::message(format!(
        "Unknown format '{}'. Valid formats: text, json",
        s
      ))),
    }
  }
}

impl std::fmt::Display for OutputFormat {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Text => write!(f, "text"),
      Self::Json => write!(f, "json"),
    }
  }
}

/// Check if a format string indicates JSON output mode
///
/// Returns true for any format that produces structured output (json, jsonl, github, github-matrix).
/// Used for early JSON mode detection to suppress progress messages.
pub fn is_json_output(format: &str) -> bool {
  let f = format.to_lowercase();
  matches!(f.as_str(), "json" | "jsonl" | "json-lines" | "github" | "github-matrix")
}

/// Builder for split/sync configurations
///
/// Centralizes the logic for selecting crates (by name, --all, etc.)
/// and building engine-specific configs. Eliminates duplication between
/// split.rs and sync.rs command handlers.
pub struct SplitSyncConfigBuilder<'a> {
  ctx: &'a WorkspaceContext,
  config: &'a RailConfig,
  split_configs: Vec<ConfigSplitConfig>,
  remote_override: Option<String>,
}

impl<'a> SplitSyncConfigBuilder<'a> {
  /// Create a new builder from workspace context
  pub fn new(ctx: &'a WorkspaceContext) -> RailResult<Self> {
    let config = ctx.require_config()?.as_ref();
    Ok(Self {
      ctx,
      config,
      split_configs: Vec::new(),
      remote_override: None,
    })
  }

  /// Select a single crate by name
  pub fn with_crate(mut self, crate_name: &str) -> RailResult<Self> {
    // Use the helper method to get all splits from unified config
    let all_splits = self.config.build_split_configs();
    let split_config = all_splits.iter().find(|s| s.name == crate_name).ok_or_else(|| {
      RailError::Config(ConfigError::CrateNotFound {
        name: crate_name.to_string(),
      })
    })?;

    self.split_configs = vec![split_config.clone()];
    Ok(self)
  }

  /// Select all configured crates
  pub fn with_all_crates(mut self) -> Self {
    self.split_configs = self.config.build_split_configs();
    self
  }

  /// Select crates based on optional name or --all flag
  pub fn with_crate_or_all(self, crate_name: Option<String>, all: bool) -> RailResult<Self> {
    if all {
      Ok(self.with_all_crates())
    } else if let Some(name) = crate_name {
      self.with_crate(&name)
    } else {
      Err(RailError::with_help(
        "Must specify a crate name or use --all",
        "Try: cargo rail <command> --all OR cargo rail <command> <crate-name>",
      ))
    }
  }

  /// Override remote URL for all selected crates
  pub fn with_remote_override(mut self, remote: Option<String>) -> Self {
    self.remote_override = remote;
    self
  }

  /// Validate all selected configurations
  pub fn validate(self) -> RailResult<Self> {
    for split_config in &self.split_configs {
      split_config.validate()?;
    }
    Ok(self)
  }

  /// Check if all remotes are local paths (testing mode)
  pub fn all_local(&self) -> bool {
    self.split_configs.iter().all(|s| {
      self
        .remote_override
        .as_ref()
        .map(|r| crate::utils::is_local_path(r))
        .unwrap_or_else(|| s.is_local_testing())
    })
  }

  /// Get the number of selected crates
  pub fn count(&self) -> usize {
    self.split_configs.len()
  }

  /// Build SplitConfig instances for the split engine
  pub fn build_split_configs(self) -> RailResult<Vec<SplitConfig>> {
    let mut configs = Vec::new();

    for split_config in &self.split_configs {
      let crate_paths = split_config.get_paths().into_iter().cloned().collect::<Vec<_>>();

      // Apply remote override if provided
      let remote = self
        .remote_override
        .clone()
        .unwrap_or_else(|| split_config.remote.clone());

      let target_repo_path = if crate::utils::is_local_path(&remote) {
        PathBuf::from(&remote)
      } else {
        split_config.target_repo_path(self.ctx.workspace_root())
      };

      configs.push(SplitConfig {
        crate_name: split_config.name.clone(),
        crate_paths,
        mode: split_config.mode.clone(),
        workspace_mode: split_config.workspace_mode.clone(),
        target_repo_path,
        branch: split_config.branch.clone(),
        remote_url: Some(remote),
        include: split_config.include.clone(),
        exclude: split_config.exclude.clone(),
      });
    }

    Ok(configs)
  }

  /// Build (SyncConfig, target_exists) tuples for the sync engine
  pub fn build_sync_configs(self) -> RailResult<Vec<(SyncConfig, bool)>> {
    let mut configs = Vec::new();

    for split_config in &self.split_configs {
      let crate_paths = split_config.get_paths().into_iter().cloned().collect::<Vec<_>>();

      // Apply remote override if provided
      let remote = self
        .remote_override
        .clone()
        .unwrap_or_else(|| split_config.remote.clone());

      let target_repo_path = if crate::utils::is_local_path(&remote) {
        PathBuf::from(&remote)
      } else {
        split_config.target_repo_path(self.ctx.workspace_root())
      };

      let target_exists = target_repo_path.exists();

      configs.push((
        SyncConfig {
          crate_name: split_config.name.clone(),
          crate_paths,
          mode: split_config.mode.clone(),
          target_repo_path,
          branch: split_config.branch.clone(),
          remote_url: remote,
        },
        target_exists,
      ));
    }

    Ok(configs)
  }
}
