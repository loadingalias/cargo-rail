//! Preview or remove explicitly selected Cargo-Rail state.

use crate::backup::BackupManager;
use crate::commands::TextJsonOutputFormat;
use crate::config::{RailConfig, UnifyConfig};
use crate::error::{GitError, RailError, RailResult};
use crate::git::SystemGit;
use crate::progress;
use crate::release::state::{ReleaseState, ReleaseStatus, state_dir, validate_state_path};
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
            || RailConfig::find_config_path(&workspace_root),
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
    /// Select one terminal release journal.
    pub release_journal: Option<String>,
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
    release_journals: Vec<String>,
}

impl CleanArtifacts {
    fn new() -> Self {
        Self {
            cache_files: Vec::new(),
            report_files: Vec::new(),
            backups: Vec::new(),
            release_journals: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.cache_files.is_empty()
            && self.report_files.is_empty()
            && self.backups.is_empty()
            && self.release_journals.is_empty()
    }

    fn total_count(&self) -> usize {
        self.cache_files.len() + self.report_files.len() + self.backups.len() + self.release_journals.len()
    }
}

/// Run the clean command
pub fn run_clean(ctx: &CleanContext, options: CleanOptions) -> RailResult<()> {
    let json = options.format.is_json();
    if !options.all
        && !options.cache
        && !options.prune_backups
        && !options.all_backups
        && !options.reports
        && options.release_journal.is_none()
    {
        return Err(RailError::with_help(
            "clean requires an explicit artifact selector",
            "select --cache, --reports, --prune-backups, --all-backups, --release-journal ID_OR_PATH, or --all",
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

    // Collect artifacts to clean
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
    let exact_release_journal = options
        .release_journal
        .as_deref()
        .map(|selector| select_terminal_release_journal(ctx, selector))
        .transpose()?;
    if let Some(path) = &exact_release_journal {
        artifacts.release_journals.push(path.display().to_string());
    } else if options.all {
        collect_release_journal_artifacts(ctx, &mut artifacts)?;
    }
    let summary = summarize_artifacts(ctx, &artifacts, cache_status.as_ref())?;
    // Validate the complete workspace-owned cache scope before deleting any of it.
    if !options.check && clean_cache {
        crate::cache::status(ctx.workspace_root(), true, false)?;
    }

    // Check mode: preview what would be cleaned
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
                "release_journals": artifacts.release_journals,
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

    // Execute cleaning
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
    if let Some(path) = exact_release_journal {
        cleaned.release_journals = vec![clean_exact_release_journal(ctx, &path)?];
    } else if options.all {
        cleaned.release_journals = clean_release_journals(ctx, &artifacts.release_journals)?;
    }

    // Output results
    if json {
        let payload = serde_json::json!({
          "command": "clean",
          "cleaned": {
            "cache": cleaned.cache_files,
            "reports": cleaned.report_files,
            "backups": cleaned.backups,
            "release_journals": cleaned.release_journals,
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
    release_journals: u64,
}

impl CleanByteSummary {
    fn total_bytes(&self) -> u64 {
        self.cache
            .saturating_add(self.reports)
            .saturating_add(self.backups)
            .saturating_add(self.release_journals)
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
    let release_journals = sum_paths(artifacts.release_journals.iter().map(Path::new))?;
    Ok(CleanByteSummary {
        cache,
        reports,
        backups,
        release_journals,
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
    if options.all || options.release_journal.is_some() {
        println!(
            "  release journals: {} file(s), {}",
            artifacts.release_journals.len(),
            crate::commands::cache::human_bytes(bytes.release_journals)
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
        for path in &artifacts.release_journals {
            println!("    {path}");
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
        if let Some(journal) = &options.release_journal {
            command.push_str(" --release-journal ");
            command.push_str(&shell_quote(journal));
        }
    }
    command
}

fn shell_quote(value: &str) -> String {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
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

fn collect_release_journal_artifacts(ctx: &CleanContext, artifacts: &mut CleanArtifacts) -> RailResult<()> {
    let directory = state_dir(ctx.workspace_root());
    if !directory.exists() {
        return Ok(());
    }
    let mut paths = fs::read_dir(&directory)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "json"))
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        validate_terminal_release_journal(ctx, &path)?;
        artifacts.release_journals.push(path.display().to_string());
    }
    Ok(())
}

fn select_terminal_release_journal(ctx: &CleanContext, selector: &str) -> RailResult<PathBuf> {
    if selector.trim().is_empty() {
        return Err(RailError::message("release journal selector must not be empty"));
    }
    let selector_path = Path::new(selector);
    let path = if selector_path.components().count() == 1 && selector_path.extension().is_none() {
        if !selector
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(RailError::message(format!(
                "release transaction ID '{selector}' contains unsupported characters"
            )));
        }
        state_dir(ctx.workspace_root()).join(format!("{selector}.json"))
    } else if selector_path.is_absolute() {
        selector_path.to_path_buf()
    } else {
        ctx.workspace_root().join(selector_path)
    };
    let path = validate_terminal_release_journal(ctx, &path)?;
    if selector_path.components().count() == 1
        && selector_path.extension().is_none()
        && ReleaseState::load(&path)?.transaction_id != selector
    {
        return Err(RailError::message(format!(
            "release journal '{}' does not belong to transaction '{selector}'",
            path.display()
        )));
    }
    Ok(path)
}

fn validate_terminal_release_journal(ctx: &CleanContext, path: &Path) -> RailResult<PathBuf> {
    let path = validate_state_path(ctx.workspace_root(), path)?;
    let state = ReleaseState::load(&path).map_err(|error| {
        RailError::with_help(
            format!("release journal '{}' is ambiguous: {error}", path.display()),
            "inspect it with 'cargo rail release status'; clean will not remove ambiguous state",
        )
    })?;
    if state.status == ReleaseStatus::Active {
        return Err(RailError::with_help(
            format!(
                "clean refused active release transaction '{}' in phase {}",
                state.transaction_id,
                state.phase.as_str()
            ),
            format!("next action: cargo rail release status {}", path.display()),
        ));
    }
    Ok(path)
}

fn clean_exact_release_journal(ctx: &CleanContext, path: &Path) -> RailResult<String> {
    let path = validate_terminal_release_journal(ctx, path)?;
    fs::remove_file(&path).map_err(|error| {
        RailError::with_help(
            format!("failed to remove release journal '{}': {error}", path.display()),
            "check file permissions and retry",
        )
    })?;
    Ok(path.display().to_string())
}

fn clean_release_journals(ctx: &CleanContext, paths: &[String]) -> RailResult<Vec<String>> {
    let mut cleaned = Vec::with_capacity(paths.len());
    for path in paths {
        cleaned.push(clean_exact_release_journal(ctx, Path::new(path))?);
    }
    if !cleaned.is_empty() {
        progress!("removed {} terminal release journal(s)", cleaned.len());
    }
    Ok(cleaned)
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

    // Get list of backups that will be cleaned before cleaning
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
