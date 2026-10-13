//! Preview or remove explicitly selected Cargo-Rail state.

use crate::backup::BackupManager;
use crate::commands::TextJsonOutputFormat;
use crate::config::{RailConfig, UnifyConfig};
use crate::error::{GitError, RailError, RailResult};
use crate::git::SystemGit;
use crate::progress;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Minimal captured authority required by `clean`.
///
/// Cleanup deliberately does not load Cargo metadata or build a dependency
/// graph. It captures only the repository boundary, optional cleanup policy,
/// and the workspace-owned artifact root.
#[derive(Debug)]
pub struct CleanContext {
    workspace_root: PathBuf,
    config: Option<Arc<RailConfig>>,
    _git: Option<SystemGit>,
    state_root: PathBuf,
    resolved_state_root: PathBuf,
}

impl CleanContext {
    /// Capture cleanup authority without performing Cargo discovery.
    pub fn capture(workspace_root: &Path, config_override: Option<&Path>) -> RailResult<Self> {
        let workspace_root = crate::utils::canonicalize_existing(workspace_root)?;
        if !workspace_root.is_dir() {
            return Err(RailError::message(format!(
                "clean workspace root '{}' is not a directory",
                workspace_root.display()
            )));
        }
        let git = match SystemGit::open(&workspace_root) {
            Ok(git) => Some(git),
            Err(RailError::Git(GitError::RepoNotFound { .. })) => None,
            Err(error) => return Err(error),
        };
        let config_path = config_override.map_or_else(
            || RailConfig::find_config_path(&crate::workspace::discovery_root(&workspace_root)),
            |path| {
                Some(if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    workspace_root.join(path)
                })
            },
        );
        let config = config_path
            .map(|path| RailConfig::load_path_with_bytes(&path).map(|(config, _)| Arc::new(config)))
            .transpose()?;
        if let Some(config) = &config {
            config.validate(&workspace_root, None)?;
        }

        let state_root = crate::workspace::cargo_rail_state_root(&workspace_root);
        let resolved_state_root = crate::utils::canonicalize_allow_missing(&state_root)?;
        if !resolved_state_root.starts_with(&workspace_root) {
            return Err(RailError::with_help(
                format!(
                    "clean artifact root '{}' resolves outside workspace '{}'",
                    state_root.display(),
                    workspace_root.display()
                ),
                "replace linked artifact directories with real directories inside the workspace",
            ));
        }

        Ok(Self {
            workspace_root,
            config,
            _git: git,
            state_root,
            resolved_state_root,
        })
    }

    fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    fn config(&self) -> Option<&RailConfig> {
        self.config.as_deref()
    }

    fn state_root(&self) -> &Path {
        &self.state_root
    }

    fn revalidate_artifact_root(&self) -> RailResult<()> {
        let resolved = crate::utils::canonicalize_allow_missing(&self.state_root)?;
        if resolved != self.resolved_state_root || !resolved.starts_with(&self.workspace_root) {
            return Err(RailError::with_help(
                format!(
                    "clean artifact root '{}' changed after capture",
                    self.state_root.display()
                ),
                "restore the workspace-owned artifact directory and retry",
            ));
        }
        Ok(())
    }
}

/// Explicit cleanup authority for one invocation.
#[derive(Debug)]
pub struct CleanOptions {
    /// Select every eligible artifact class.
    pub all: bool,
    /// Select workspace cache state.
    pub cache: bool,
    /// Select backups beyond configured retention.
    pub prune_backups: bool,
    /// Select every backup.
    pub all_backups: bool,
    /// Select generated reports.
    pub reports: bool,
    /// Preview without mutation.
    pub check: bool,
    /// Output transport for the cleanup report.
    pub format: TextJsonOutputFormat,
}

/// Artifacts collected during clean
struct CleanArtifacts {
    cache_files: Vec<String>,
    report_files: Vec<String>,
    backups: Vec<String>,
}

impl CleanArtifacts {
    fn new() -> Self {
        Self {
            cache_files: Vec::new(),
            report_files: Vec::new(),
            backups: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.cache_files.is_empty() && self.report_files.is_empty() && self.backups.is_empty()
    }

    fn total_count(&self) -> usize {
        self.cache_files.len() + self.report_files.len() + self.backups.len()
    }
}

/// Run the clean command
pub fn run_clean(ctx: &CleanContext, options: CleanOptions) -> RailResult<()> {
    let json = options.format.is_json();
    if !options.all && !options.cache && !options.prune_backups && !options.all_backups && !options.reports {
        return Err(RailError::with_help(
            "clean requires an explicit artifact selector",
            "select --cache, --reports, --prune-backups, --all-backups, or --all",
        ));
    }

    let clean_cache = options.cache || options.all;
    let clean_reports = options.reports || options.all;
    let prune_backups = options.prune_backups;
    let delete_all_backups = options.all_backups || options.all;

    ctx.revalidate_artifact_root()?;

    let cache_status = clean_cache
        .then(|| crate::cache::status(ctx.workspace_root(), true, false))
        .transpose()?;

    let mut artifacts = CleanArtifacts::new();

    if let Some(status) = &cache_status {
        collect_cache_artifacts(status, &mut artifacts);
    }

    if clean_reports {
        collect_report_artifacts(ctx, &mut artifacts);
    }

    if prune_backups || delete_all_backups {
        collect_backup_artifacts(ctx, delete_all_backups, &mut artifacts)?;
    }
    let summary = summarize_artifacts(ctx, &artifacts, cache_status.as_ref())?;
    // Validate the complete workspace-owned cache scope before deleting any of it.
    if !options.check && clean_cache {
        crate::cache::status(ctx.workspace_root(), true, false)?;
    }

    if options.check {
        if json {
            let has_changes = !artifacts.is_empty();
            let payload = serde_json::json!({
              "command": "clean",
              "check": true,
              "would_clean": {
                "cache": artifacts.cache_files,
                "reports": artifacts.report_files,
                "backups": artifacts.backups,
              },
              "total": artifacts.total_count(),
              "would_reclaim_bytes": summary.total_bytes(),
              "class_bytes": summary,
              "has_changes": has_changes,
              "cache_status": cache_status,
            });
            let output = crate::output::machine_json_envelope(
                "clean",
                "check",
                if has_changes { "pending_changes" } else { "success" },
                if has_changes { 1 } else { 0 },
                payload,
            );
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            render_clean_text("Would remove", &options, &artifacts, &summary);
            if !artifacts.is_empty() {
                println!("Next: {}", clean_apply_command(&options));
            }
        }

        if !artifacts.is_empty() {
            return Err(RailError::CheckHasPendingChanges);
        }
        return Ok(());
    }

    ctx.revalidate_artifact_root()?;
    let mut cleaned = CleanArtifacts::new();

    if clean_cache {
        cleaned.cache_files = clean_cache_files(ctx)?;
    }

    if clean_reports {
        cleaned.report_files = clean_generated_reports(ctx)?;
    }

    if prune_backups || delete_all_backups {
        cleaned.backups = clean_backups_handler(ctx, delete_all_backups)?;
    }

    if json {
        let payload = serde_json::json!({
          "command": "clean",
          "cleaned": {
            "cache": cleaned.cache_files,
            "reports": cleaned.report_files,
            "backups": cleaned.backups,
          },
          "total": cleaned.total_count()
          ,"reclaimed_bytes": summary.total_bytes()
          ,"class_bytes": summary
        });
        let output = crate::output::machine_json_envelope("clean", "apply", "success", 0, payload);
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if cleaned.is_empty() {
        println!("Nothing to clean.");
    } else {
        render_clean_text("Removed", &options, &cleaned, &summary);
    }

    Ok(())
}

#[derive(Debug, serde::Serialize)]
struct CleanByteSummary {
    cache: u64,
    reports: u64,
    backups: u64,
}

impl CleanByteSummary {
    fn total_bytes(&self) -> u64 {
        self.cache.saturating_add(self.reports).saturating_add(self.backups)
    }
}

fn summarize_artifacts(
    ctx: &CleanContext,
    artifacts: &CleanArtifacts,
    cache_status: Option<&crate::cache::CacheStatus>,
) -> RailResult<CleanByteSummary> {
    let cache = cache_status
        .and_then(|status| status.workspace.as_ref())
        .map_or(0, |workspace| workspace.bytes);
    let reports = sum_paths(artifacts.report_files.iter().map(Path::new))?;
    let backup_root = crate::backup::get_backup_root(ctx.workspace_root());
    let backups = sum_paths(artifacts.backups.iter().map(|id| backup_root.join(id)))?;
    Ok(CleanByteSummary {
        cache,
        reports,
        backups,
    })
}

fn sum_paths<P>(paths: impl IntoIterator<Item = P>) -> RailResult<u64>
where
    P: AsRef<Path>,
{
    paths.into_iter().try_fold(0_u64, |total, path| {
        total
            .checked_add(measure_path(path.as_ref())?)
            .ok_or_else(|| RailError::message("clean byte count overflow"))
    })
}

fn measure_path(path: &Path) -> RailResult<u64> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }
    fs::read_dir(path)?.try_fold(0_u64, |total, entry| {
        let bytes = measure_path(&entry?.path())?;
        total
            .checked_add(bytes)
            .ok_or_else(|| RailError::message("clean byte count overflow"))
    })
}

fn render_clean_text(heading: &str, options: &CleanOptions, artifacts: &CleanArtifacts, bytes: &CleanByteSummary) {
    if artifacts.is_empty() {
        println!("Nothing to clean.");
        return;
    }
    println!("{heading}:");
    if options.all || options.cache {
        println!(
            "  workspace cache: {} path(s), {}",
            artifacts.cache_files.len(),
            crate::commands::cache::human_bytes(bytes.cache)
        );
    }
    if options.all || options.reports {
        println!(
            "  reports: {} file(s), {}",
            artifacts.report_files.len(),
            crate::commands::cache::human_bytes(bytes.reports)
        );
    }
    if options.all || options.prune_backups || options.all_backups {
        println!(
            "  backups: {} set(s), {}",
            artifacts.backups.len(),
            crate::commands::cache::human_bytes(bytes.backups)
        );
    }
    if crate::output::is_verbose() {
        for path in &artifacts.cache_files {
            println!("    {path}");
        }
        for path in &artifacts.report_files {
            println!("    {path}");
        }
        for id in &artifacts.backups {
            println!("    backup {id}");
        }
    }
}

fn clean_apply_command(options: &CleanOptions) -> String {
    let mut command = String::from("cargo rail clean");
    if options.all {
        command.push_str(" --all");
    } else {
        if options.cache {
            command.push_str(" --cache");
        }
        if options.reports {
            command.push_str(" --reports");
        }
        if options.prune_backups {
            command.push_str(" --prune-backups");
        }
        if options.all_backups {
            command.push_str(" --all-backups");
        }
    }
    command
}

fn collect_cache_artifacts(status: &crate::cache::CacheStatus, artifacts: &mut CleanArtifacts) {
    if let Some(workspace) = &status.workspace {
        artifacts.cache_files.extend(
            workspace
                .artifacts
                .iter()
                .filter(|artifact| artifact.kind != "workspace_cache_lock")
                .map(|artifact| artifact.path.clone()),
        );
    }
}

fn collect_report_artifacts(ctx: &CleanContext, artifacts: &mut CleanArtifacts) {
    let report_dir = ctx.state_root();
    if report_dir.exists()
        && let Ok(entries) = fs::read_dir(report_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
                artifacts.report_files.push(path.display().to_string());
            }
        }
    }
}

fn collect_backup_artifacts(ctx: &CleanContext, delete_all: bool, artifacts: &mut CleanArtifacts) -> RailResult<()> {
    let backup_manager = BackupManager::new(ctx.workspace_root());
    if !backup_manager.has_backups() {
        return Ok(());
    }

    let backup_list = backup_manager.list_backups()?;

    if delete_all {
        for backup in &backup_list {
            artifacts.backups.push(backup.id.clone());
        }
    } else {
        let max_backups = ctx
            .config()
            .map(|c| c.unify.max_backups)
            .unwrap_or_else(|| UnifyConfig::default().max_backups);

        if backup_list.len() > max_backups {
            for backup in backup_list.iter().skip(max_backups) {
                artifacts.backups.push(backup.id.clone());
            }
        }
    }

    Ok(())
}

fn clean_cache_files(ctx: &CleanContext) -> RailResult<Vec<String>> {
    progress!("removing validated cache state...");
    let removal = crate::cache::remove_workspace(ctx.workspace_root())?;
    Ok(removal.paths)
}

fn clean_generated_reports(ctx: &CleanContext) -> RailResult<Vec<String>> {
    let report_dir = ctx.state_root();
    let mut cleaned = Vec::new();

    if report_dir.exists() {
        progress!("removing reports...");
        for entry in fs::read_dir(report_dir).map_err(|e| {
            RailError::with_help(
                format!("failed to read {}: {}", report_dir.display(), e),
                "check directory permissions",
            )
        })? {
            let entry = entry.map_err(|e| {
                RailError::with_help(
                    format!("failed to read directory entry: {}", e),
                    "check directory permissions",
                )
            })?;
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
                fs::remove_file(&path).map_err(|e| {
                    RailError::with_help(
                        format!("failed to remove {}: {}", path.display(), e),
                        "check file permissions or if the file is in use",
                    )
                })?;
                cleaned.push(path.display().to_string());
            }
        }
    }

    Ok(cleaned)
}

fn clean_backups_handler(ctx: &CleanContext, delete_all: bool) -> RailResult<Vec<String>> {
    let backup_manager = BackupManager::new(ctx.workspace_root());

    if !backup_manager.has_backups() {
        return Ok(Vec::new());
    }

    let backup_list = backup_manager.list_backups()?;
    let mut cleaned = Vec::with_capacity(backup_list.len());

    if delete_all {
        progress!("removing all backups...");
        for backup in &backup_list {
            cleaned.push(backup.id.clone());
        }
        let count = backup_manager.cleanup_old_backups(0)?;
        progress!("  removed {} backups", count);
    } else {
        let max_backups = ctx
            .config()
            .map(|c| c.unify.max_backups)
            .unwrap_or_else(|| UnifyConfig::default().max_backups);

        progress!("pruning backups (keeping {})...", max_backups);

        if backup_list.len() > max_backups {
            for backup in backup_list.iter().skip(max_backups) {
                cleaned.push(backup.id.clone());
            }
        }

        let count = backup_manager.cleanup_old_backups(max_backups)?;
        if count > 0 {
            progress!("  removed {} old backups", count);
        } else {
            progress!("  no backups to prune");
        }
    }

    Ok(cleaned)
}
