//! Common utilities and builders for command implementations

use crate::config::{RailConfig, SplitConfig as ConfigSplitConfig};
use crate::error::{ConfigError, RailError, RailResult};
use crate::git::mappings::{HistorySide, MappingStore, repository_identity};
use crate::split::{ReleaseBoundary, SplitOwnership, SplitParams};
use crate::sync::SyncConfig;
use crate::workspace::WorkspaceContext;
use clap::ValueEnum;
use glob::Pattern;
use std::collections::BTreeMap;
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

pub(crate) fn split_mapping_count(
    workspace_root: &std::path::Path,
    crate_name: &str,
    target_repo_path: &std::path::Path,
) -> RailResult<usize> {
    if !target_repo_path.join(".git").exists() {
        return Ok(0);
    }
    let source_identity = repository_identity(workspace_root)?;
    let target_identity = repository_identity(target_repo_path)?;
    let mut store = MappingStore::new(crate_name.to_string());
    store.load_history(workspace_root, HistorySide::Source, &target_identity)?;
    store.load_history(target_repo_path, HistorySide::Target, &source_identity)?;
    store.load_legacy_notes(workspace_root)?;
    store.load_legacy_notes(target_repo_path)?;
    Ok(store.count())
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

/// Output format for `cargo rail surface`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum SurfaceOutputFormat {
    /// Human-readable text output (default)
    #[default]
    Text,
    /// Machine-readable JSON output
    Json,
    /// GitHub Actions key/value output
    #[value(name = "github")]
    GitHub,
}

impl SurfaceOutputFormat {
    /// Check if this format is JSON.
    pub fn is_json(&self) -> bool {
        matches!(self, Self::Json)
    }

    /// Check if this format is structured output.
    pub fn is_json_like(&self) -> bool {
        matches!(self, Self::Json | Self::GitHub)
    }
}

/// Builder for split/sync configurations
///
/// Centralizes the logic for selecting crates (by name, --all, etc.)
/// and building engine-specific configs. Eliminates duplication between
/// split.rs and sync.rs command handlers.
#[derive(Debug)]
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
    fn resolve_ownership(&self, split_config: &ConfigSplitConfig) -> RailResult<(Vec<PathBuf>, SplitOwnership)> {
        let snapshot = self.ctx.snapshot()?;
        let graph = snapshot.base_resolution().graph();
        let mut members = split_config.members.clone();
        members.sort();
        let original_len = members.len();
        members.dedup();
        if members.len() != original_len {
            return Err(RailError::Config(ConfigError::InvalidField {
                field: format!("crates.{}.split.members", split_config.name),
                reason: "Cargo member names must be unique".to_string(),
            }));
        }

        let mut crate_paths = Vec::with_capacity(members.len());
        for member in &members {
            let package = graph.workspace_package_by_name(member)?;
            let snapshot_package = snapshot
                .packages()
                .iter()
                .find(|candidate| candidate.id() == &package.id && candidate.is_workspace_member())
                .ok_or_else(|| {
                    RailError::message(format!(
                        "workspace snapshot has no exact package identity for split member '{}'",
                        member
                    ))
                })?;
            let root = snapshot_package.package_root().ok_or_else(|| {
                RailError::message(format!(
                    "split member '{}' has no source root in the workspace snapshot",
                    member
                ))
            })?;
            crate_paths.push(root.to_path_buf());
        }

        let dependency_closure = graph.workspace_dependency_closure(&members)?;
        let selected = members
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        let mut release_boundaries = snapshot
            .config()
            .into_iter()
            .flat_map(|config| &config.release.version_groups)
            .filter(|(_, boundary_members)| boundary_members.iter().any(|member| selected.contains(member.as_str())))
            .map(|(name, boundary_members)| {
                let mut boundary_members = boundary_members.clone();
                boundary_members.sort();
                ReleaseBoundary {
                    name: name.clone(),
                    members: boundary_members,
                }
            })
            .collect::<Vec<_>>();
        release_boundaries.sort_by(|left, right| left.name.cmp(&right.name));

        Ok((
            crate_paths,
            SplitOwnership {
                snapshot_id: snapshot.id().to_string(),
                members,
                dependency_closure,
                release_boundaries,
            },
        ))
    }

    fn resolve_asset_paths(
        &self,
        split_config: &ConfigSplitConfig,
        crate_paths: &[PathBuf],
    ) -> RailResult<Vec<PathBuf>> {
        let compile = |field: &str, values: &[String]| {
            values
                .iter()
                .map(|value| {
                    Pattern::new(value).map_err(|error| {
                        RailError::Config(ConfigError::InvalidField {
                            field: format!("crates.{}.split.{field}", split_config.name),
                            reason: format!("invalid glob '{value}': {error}"),
                        })
                    })
                })
                .collect::<RailResult<Vec<_>>>()
        };
        let includes = compile("include", &split_config.include)?;
        let excludes = compile("exclude", &split_config.exclude)?;
        let mut assets = self
            .ctx
            .snapshot()?
            .source()
            .tree()
            .entries()
            .iter()
            .filter(|entry| includes.iter().any(|pattern| pattern.matches(entry.path.as_str())))
            .filter(|entry| !excludes.iter().any(|pattern| pattern.matches(entry.path.as_str())))
            .map(|entry| entry.path.as_path().to_path_buf())
            .collect::<Vec<_>>();
        assets.sort();
        assets.dedup();

        if let Some(path) = assets
            .iter()
            .find(|path| crate_paths.iter().any(|crate_root| path.starts_with(crate_root)))
        {
            return Err(RailError::Config(ConfigError::InvalidField {
                field: format!("crates.{}.split.include", split_config.name),
                reason: format!(
                    "'{}' is Cargo-owned; member roots are included automatically",
                    path.display()
                ),
            }));
        }

        if split_config.mode == crate::config::SplitMode::Single {
            let crate_root = crate_paths
                .first()
                .ok_or_else(|| RailError::message("single split has no Cargo member root"))?;
            let mut targets = BTreeMap::<PathBuf, PathBuf>::new();
            for entry in self.ctx.snapshot()?.source().tree().entries() {
                let source = entry.path.as_path();
                let target = if let Ok(relative) = source.strip_prefix(crate_root) {
                    Some(relative.to_path_buf())
                } else if assets.iter().any(|asset| asset == source) {
                    Some(source.to_path_buf())
                } else {
                    None
                };
                let Some(target) = target else { continue };
                if let Some(previous) = targets.insert(target.clone(), source.to_path_buf())
                    && previous != source
                {
                    return Err(RailError::Config(ConfigError::InvalidField {
                        field: format!("crates.{}.split.include", split_config.name),
                        reason: format!(
                            "'{}' and '{}' both map to target path '{}'",
                            previous.display(),
                            source.display(),
                            target.display()
                        ),
                    }));
                }
            }
        }

        Ok(assets)
    }

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
            self.remote_override
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
            let (crate_paths, ownership) = self.resolve_ownership(split_config)?;
            let asset_paths = self.resolve_asset_paths(split_config, &crate_paths)?;

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
            )?
            .with_asset_paths(&asset_paths)?;
            let target_repo_path = path_capabilities.target_root().to_path_buf();

            configs.push(SplitParams {
                crate_name: split_config.name.clone(),
                crate_paths,
                asset_paths,
                ownership,
                mode: split_config.mode.clone(),
                workspace_mode: split_config.workspace_mode.clone(),
                target_repo_path,
                branch: split_config.branch.clone(),
                remote_url: Some(remote),
                path_capabilities,
            });
        }

        Ok(configs)
    }

    /// Build (SyncConfig, target_exists) tuples for the sync engine
    pub fn build_sync_configs(self) -> RailResult<Vec<(SyncConfig, bool)>> {
        let mut configs = Vec::new();

        for split_config in &self.split_configs {
            let (crate_paths, ownership) = self.resolve_ownership(split_config)?;
            let asset_paths = self.resolve_asset_paths(split_config, &crate_paths)?;

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
            )?
            .with_asset_paths(&asset_paths)?;
            let target_repo_path = path_capabilities.target_root().to_path_buf();

            let target_exists = target_repo_path.exists();

            configs.push((
                SyncConfig {
                    crate_name: split_config.name.clone(),
                    crate_paths,
                    asset_paths,
                    ownership,
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
