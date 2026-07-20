//! `cargo rail clean` - Remove generated artifacts (cache, backups, reports).

use crate::backup::BackupManager;
use crate::commands::TextJsonOutputFormat;
use crate::config::UnifyConfig;
use crate::error::{RailError, RailResult};
use crate::progress;
use crate::release::state::{ReleasePhase, ReleaseState, ReleaseStatus, StepStatus, state_dir};
use crate::workspace::WorkspaceContext;
use std::fs;

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
pub fn run_clean(
  ctx: &WorkspaceContext,
  cache: bool,
  backups: bool,
  reports: bool,
  check: bool,
  format: TextJsonOutputFormat,
) -> RailResult<()> {
  let json = format.is_json();

  if json {
    crate::output::set_json_mode(true);
  }

  let clean_all = !cache && !backups && !reports;

  let clean_cache = cache || clean_all;
  let clean_reports = reports || clean_all;
  let prune_backups = backups && !clean_all;
  let delete_all_backups = clean_all;

  // Collect artifacts to clean
  let mut artifacts = CleanArtifacts::new();

  if clean_cache {
    collect_cache_artifacts(ctx, &mut artifacts);
  }

  if clean_reports {
    collect_report_artifacts(ctx, &mut artifacts);
  }

  if prune_backups || delete_all_backups {
    collect_backup_artifacts(ctx, delete_all_backups, &mut artifacts)?;
  }
  if clean_all {
    collect_release_journal_artifacts(ctx, &mut artifacts)?;
  }

  // Check mode: preview what would be cleaned
  if check {
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
        "has_changes": has_changes
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
      println!("would clean:\n");
      for path in &artifacts.cache_files {
        println!("  {}", path);
      }
      for path in &artifacts.report_files {
        println!("  {}", path);
      }
      for backup_id in &artifacts.backups {
        println!("  backup: {}", backup_id);
      }
      for path in &artifacts.release_journals {
        println!("  release journal: {}", path);
      }

      if !artifacts.is_empty() {
        println!("\nChanges detected. Run without --check to apply.");
      } else {
        println!("nothing to clean");
      }
    }

    if !artifacts.is_empty() {
      return Err(RailError::CheckHasPendingChanges);
    }
    return Ok(());
  }

  // Execute cleaning
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
  if clean_all {
    cleaned.release_journals = clean_release_journals(&artifacts.release_journals)?;
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
    });
    let output = crate::output::machine_json_envelope("clean", "apply", "success", 0, payload);
    println!("{}", serde_json::to_string_pretty(&output)?);
  } else if cleaned.is_empty() {
    println!("nothing to clean");
  } else {
    println!("clean complete");
  }

  Ok(())
}

fn collect_cache_artifacts(ctx: &WorkspaceContext, artifacts: &mut CleanArtifacts) {
  let state_root = crate::workspace::cargo_rail_state_root(ctx.workspace_root());
  let cache_path = state_root.join("metadata.json");
  let compiler_cache_dir = state_root.join("cache");

  if cache_path.exists() {
    artifacts.cache_files.push(cache_path.display().to_string());
  }

  if compiler_cache_dir.exists() {
    artifacts.cache_files.push(compiler_cache_dir.display().to_string());
  }
}

fn collect_report_artifacts(ctx: &WorkspaceContext, artifacts: &mut CleanArtifacts) {
  let report_dir = crate::workspace::cargo_rail_state_root(ctx.workspace_root());
  if report_dir.exists()
    && let Ok(entries) = fs::read_dir(&report_dir)
  {
    for entry in entries.flatten() {
      let path = entry.path();
      if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
        artifacts.report_files.push(path.display().to_string());
      }
    }
  }
}

fn collect_backup_artifacts(
  ctx: &WorkspaceContext,
  delete_all: bool,
  artifacts: &mut CleanArtifacts,
) -> RailResult<()> {
  let backup_manager = BackupManager::new(&ctx.workspace_root);
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
      .as_ref()
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

fn collect_release_journal_artifacts(ctx: &WorkspaceContext, artifacts: &mut CleanArtifacts) -> RailResult<()> {
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
    let state = ReleaseState::load(&path).map_err(|error| {
      RailError::with_help(
        format!("release journal '{}' is ambiguous: {}", path.display(), error),
        "inspect and recover it with 'cargo rail release status'; clean will not remove ambiguous state",
      )
    })?;
    let superseded = state.status == ReleaseStatus::Active
      && state.phase == ReleasePhase::Planned
      && state
        .crates
        .iter()
        .all(|crate_state| crate_state.commit.status == StepStatus::Pending)
      && ctx.git()?.git().head_commit()? != state.initial_head;
    if state.status == ReleaseStatus::Active && !superseded {
      return Err(RailError::with_help(
        format!(
          "clean refused active release transaction '{}' in phase {}",
          state.transaction_id,
          state.phase.as_str()
        ),
        format!("resume or abort it first: cargo rail release status {}", path.display()),
      ));
    }
    artifacts.release_journals.push(path.display().to_string());
  }
  Ok(())
}

fn clean_release_journals(paths: &[String]) -> RailResult<Vec<String>> {
  let mut cleaned = Vec::with_capacity(paths.len());
  for path in paths {
    fs::remove_file(path).map_err(|error| {
      RailError::with_help(
        format!("failed to remove release journal '{}': {}", path, error),
        "check file permissions and retry",
      )
    })?;
    cleaned.push(path.clone());
  }
  if !cleaned.is_empty() {
    progress!("removed {} terminal release journal(s)", cleaned.len());
  }
  Ok(cleaned)
}

fn clean_cache_files(ctx: &WorkspaceContext) -> RailResult<Vec<String>> {
  let state_root = crate::workspace::cargo_rail_state_root(ctx.workspace_root());
  let cache_path = state_root.join("metadata.json");
  let compiler_cache_dir = state_root.join("cache");
  let mut cleaned_paths = Vec::new();

  if cache_path.exists() {
    progress!("removing cache...");
    fs::remove_file(&cache_path).map_err(|e| {
      RailError::with_help(
        format!("failed to remove {}: {}", cache_path.display(), e),
        "check file permissions or if the file is in use",
      )
    })?;
    cleaned_paths.push(cache_path.display().to_string());
  }

  if compiler_cache_dir.exists() {
    progress!("removing cache...");
    fs::remove_dir_all(&compiler_cache_dir).map_err(|e| {
      RailError::with_help(
        format!("failed to remove {}: {}", compiler_cache_dir.display(), e),
        "check directory permissions or if files are in use",
      )
    })?;
    cleaned_paths.push(compiler_cache_dir.display().to_string());
  }

  Ok(cleaned_paths)
}

fn clean_generated_reports(ctx: &WorkspaceContext) -> RailResult<Vec<String>> {
  let report_dir = crate::workspace::cargo_rail_state_root(ctx.workspace_root());
  let mut cleaned = Vec::new();

  if report_dir.exists() {
    progress!("removing reports...");
    for entry in fs::read_dir(&report_dir).map_err(|e| {
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

fn clean_backups_handler(ctx: &WorkspaceContext, delete_all: bool) -> RailResult<Vec<String>> {
  let backup_manager = BackupManager::new(&ctx.workspace_root);

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
      .as_ref()
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
