//! Common utilities and builders for command implementations

use crate::config::{RailConfig, SplitConfig as ConfigSplitConfig};
use crate::error::{ConfigError, RailError, RailResult};
use crate::split::SplitParams;
use crate::sync::SyncConfig;
use crate::workspace::WorkspaceContext;
use clap::ValueEnum;
use std::path::PathBuf;

/// Render a deterministic preview for a potentially large list.
///
/// Keeps small lists intact and truncates large lists with a `+N more` suffix.
pub(crate) fn format_preview_list<T: AsRef<str>>(items: &[T], preview_limit: usize) -> String {
  if items.is_empty() {
    return "none".to_string();
  }

  let preview_limit = preview_limit.max(1);
  let preview = items
    .iter()
    .take(preview_limit)
    .map(AsRef::as_ref)
    .collect::<Vec<_>>()
    .join(", ");

  if items.len() <= preview_limit {
    preview
  } else {
    format!("{preview}, ... +{} more", items.len() - preview_limit)
  }
}

/// Output format for commands that support only human text and JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum TextJsonOutputFormat {
  /// Human-readable text output (default)
  #[default]
  Text,
  /// Machine-readable JSON output
  Json,
}

impl TextJsonOutputFormat {
  /// Check if this format is JSON.
  pub fn is_json(&self) -> bool {
    matches!(self, Self::Json)
  }

  /// Check if this format is structured output.
  pub fn is_json_like(&self) -> bool {
    self.is_json()
  }
}

/// Output format for `cargo rail split run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum SplitOutputFormat {
  /// Human-readable text output (default)
  #[default]
  Text,
  /// Machine-readable JSON output
  Json,
  /// Names only, one per line
  #[value(name = "names-only", alias = "names")]
  NamesOnly,
  /// JSON Lines format (one object per line)
  #[value(name = "jsonl", alias = "json-lines")]
  JsonLines,
}

impl SplitOutputFormat {
  /// Check if this format is JSON.
  pub fn is_json(&self) -> bool {
    matches!(self, Self::Json)
  }

  /// Check if this format is structured output.
  pub fn is_json_like(&self) -> bool {
    matches!(self, Self::Json | Self::JsonLines)
  }
}

/// Output format for `cargo rail change`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ChangeOutputFormat {
  /// Human-readable text output (default)
  #[default]
  Text,
  /// Machine-readable JSON output
  Json,
  /// Names only, one per line
  #[value(name = "names-only", alias = "names")]
  NamesOnly,
}

impl ChangeOutputFormat {
  /// Check if this format is JSON.
  pub fn is_json(&self) -> bool {
    matches!(self, Self::Json)
  }

  /// Check if this format is JSON-like structured output.
  pub fn is_json_like(&self) -> bool {
    self.is_json()
  }
}

/// Output format for `cargo rail unify`.
///
/// `unify` currently supports only text and JSON output. Unlike planner/executor surfaces,
/// it does not produce list-like formats (cargo-args, github, jsonl, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum UnifyOutputFormat {
  /// Human-readable text output (default)
  #[default]
  Text,
  /// Machine-readable JSON output
  Json,
}

impl UnifyOutputFormat {
  /// Check if this format is JSON
  pub fn is_json(&self) -> bool {
    matches!(self, Self::Json)
  }

  /// Check if this format is a JSON-like structured format
  pub fn is_json_like(&self) -> bool {
    self.is_json()
  }
}

/// Output format for `cargo rail plan`.
///
/// `plan` is the primary planning surface and intentionally supports a minimal
/// format set: human text, full JSON contract, and GitHub Actions key/value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum PlanOutputFormat {
  /// Human-readable text output (default)
  #[default]
  Text,
  /// Machine-readable JSON output
  Json,
  /// GitHub Actions output format for $GITHUB_OUTPUT
  #[value(name = "github")]
  GitHub,
  /// GitHub Actions output with embedded planner contract for debugging
  #[value(name = "github-debug")]
  GitHubDebug,
}

impl PlanOutputFormat {
  /// Check if this format is JSON.
  pub fn is_json(&self) -> bool {
    matches!(self, Self::Json)
  }

  /// Check if this format is a JSON-like structured format.
  pub fn is_json_like(&self) -> bool {
    matches!(self, Self::Json | Self::GitHub | Self::GitHubDebug)
  }
}

/// Output format for a dry-run action graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionOutputFormat {
  /// Human-readable execution or preview output (default).
  #[default]
  Text,
  /// Versioned machine-readable action plan.
  Json,
  /// GitHub Actions key/value output containing the ordered action IDs.
  #[value(name = "github")]
  #[serde(rename = "github")]
  GitHub,
}

impl ActionOutputFormat {
  /// Whether this format emits structured machine output.
  pub const fn is_json_like(self) -> bool {
    !matches!(self, Self::Text)
  }
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

/// Enforce explicit confirmation for destructive operations in non-interactive contexts.
///
/// Allowed confirmation paths:
/// - interactive prompt is available (`prompt_possible`)
/// - explicit `--yes`
/// - `--plan <path>` (fingerprint-validated mutation plan)
pub fn enforce_safety_gate(
  operation: &str,
  yes: bool,
  plan_path: Option<&std::path::Path>,
  prompt_possible: bool,
) -> RailResult<()> {
  if yes || plan_path.is_some() || prompt_possible {
    return Ok(());
  }

  Err(RailError::with_help(
    format!("{} requires explicit confirmation in non-interactive mode", operation),
    "use --yes to confirm explicitly, or pass --plan <PATH> from a prior --check JSON plan",
  ))
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
        "must specify a crate name or use --all",
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

  /// Build SplitParams instances for the split engine
  pub fn build_split_configs(self) -> RailResult<Vec<SplitParams>> {
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
      let path_capabilities = crate::split::SplitPathCapabilities::new(
        self.ctx.workspace_root(),
        &self.ctx.git()?.git().worktree_root,
        &crate_paths,
        &target_repo_path,
      )?;
      let target_repo_path = path_capabilities.target_root().to_path_buf();

      configs.push(SplitParams {
        crate_name: split_config.name.clone(),
        crate_paths,
        mode: split_config.mode.clone(),
        workspace_mode: split_config.workspace_mode.clone(),
        target_repo_path,
        branch: split_config.branch.clone(),
        remote_url: Some(remote),
        include: split_config.include.clone(),
        exclude: split_config.exclude.clone(),
        path_capabilities,
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
      let path_capabilities = crate::split::SplitPathCapabilities::new(
        self.ctx.workspace_root(),
        &self.ctx.git()?.git().worktree_root,
        &crate_paths,
        &target_repo_path,
      )?;
      let target_repo_path = path_capabilities.target_root().to_path_buf();

      let target_exists = target_repo_path.exists();

      configs.push((
        SyncConfig {
          crate_name: split_config.name.clone(),
          crate_paths,
          mode: split_config.mode.clone(),
          workspace_mode: split_config.workspace_mode.clone(),
          target_repo_path,
          branch: split_config.branch.clone(),
          remote_url: remote,
          path_capabilities,
        },
        target_exists,
      ));
    }

    Ok(configs)
  }
}

#[cfg(test)]
mod tests {
  use super::format_preview_list;

  #[test]
  fn preview_list_keeps_short_lists() {
    let items = vec!["rail-a".to_string(), "rail-b".to_string(), "rail-c".to_string()];
    assert_eq!(format_preview_list(&items, 5), "rail-a, rail-b, rail-c");
  }

  #[test]
  fn preview_list_truncates_large_lists() {
    let items = vec![
      "rail-a".to_string(),
      "rail-b".to_string(),
      "rail-c".to_string(),
      "rail-d".to_string(),
    ];
    assert_eq!(format_preview_list(&items, 2), "rail-a, rail-b, ... +2 more");
  }
}
