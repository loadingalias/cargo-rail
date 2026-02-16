//! `cargo rail clean` - Remove generated artifacts (cache, backups, reports).

use crate::backup::BackupManager;
use crate::commands::OutputFormat;
use crate::config::UnifyConfig;
use crate::error::{RailError, RailResult};
use crate::progress;
use crate::workspace::WorkspaceContext;
use std::fs;

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
pub fn run_clean(
  ctx: &WorkspaceContext,
  cache: bool,
  backups: bool,
  reports: bool,
  check: bool,
  format: OutputFormat,
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

  // Check mode: preview what would be cleaned
  if check {
    if json {
      let output = serde_json::json!({
        "command": "clean",
        "check": true,
        "would_clean": {
          "cache": artifacts.cache_files,
          "reports": artifacts.report_files,
          "backups": artifacts.backups,
        },
        "total": artifacts.total_count(),
        "has_changes": !artifacts.is_empty()
      });
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

  if clean_cache && let Some(path) = clean_metadata_cache(ctx)? {
    cleaned.cache_files.push(path);
  }

  if clean_reports {
    cleaned.report_files = clean_generated_reports(ctx)?;
  }

  if prune_backups || delete_all_backups {
    cleaned.backups = clean_backups_handler(ctx, delete_all_backups)?;
  }

  // Output results
  if json {
    let output = serde_json::json!({
      "command": "clean",
      "cleaned": {
        "cache": cleaned.cache_files,
        "reports": cleaned.report_files,
        "backups": cleaned.backups,
      },
      "total": cleaned.total_count()
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
  } else if cleaned.is_empty() {
    println!("nothing to clean");
  } else {
    println!("clean complete");
  }

  Ok(())
}

fn collect_cache_artifacts(ctx: &WorkspaceContext, artifacts: &mut CleanArtifacts) {
  let cache_path = ctx
    .workspace_root
    .join("target")
    .join("cargo-rail")
    .join("metadata.json");

  if cache_path.exists() {
    artifacts.cache_files.push(cache_path.display().to_string());
  }
}

fn collect_report_artifacts(ctx: &WorkspaceContext, artifacts: &mut CleanArtifacts) {
  let report_dir = ctx.workspace_root.join("target").join("cargo-rail");
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
      .config
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

fn clean_metadata_cache(ctx: &WorkspaceContext) -> RailResult<Option<String>> {
  let cache_path = ctx
    .workspace_root
    .join("target")
    .join("cargo-rail")
    .join("metadata.json");

  if cache_path.exists() {
    progress!("removing cache...");
    fs::remove_file(&cache_path).map_err(|e| {
      RailError::with_help(
        format!("failed to remove {}: {}", cache_path.display(), e),
        "check file permissions or if the file is in use",
      )
    })?;
    Ok(Some(cache_path.display().to_string()))
  } else {
    Ok(None)
  }
}

fn clean_generated_reports(ctx: &WorkspaceContext) -> RailResult<Vec<String>> {
  let report_dir = ctx.workspace_root.join("target").join("cargo-rail");
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
      .config
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
