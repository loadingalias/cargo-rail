use crate::backup::BackupManager;
use crate::config::UnifyConfig;
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;
use std::fs;

/// Run the clean command
pub fn run_clean(ctx: &WorkspaceContext, cache: bool, backups: bool, reports: bool) -> RailResult<()> {
  let mut cleaned_any = false;

  // If no flags provided, clean everything (cache, reports, and ALL backups)
  let clean_all = !cache && !backups && !reports;

  let clean_cache = cache || clean_all;
  let clean_reports = reports || clean_all;
  // If specific backup flag provided, prune. If cleaning all, delete all.
  let prune_backups = backups && !clean_all;
  let delete_all_backups = clean_all;

  if clean_cache {
    clean_metadata_cache(ctx)?;
    cleaned_any = true;
  }

  if clean_reports {
    clean_generated_reports(ctx)?;
    cleaned_any = true;
  }

  if prune_backups || delete_all_backups {
    clean_backups_handler(ctx, delete_all_backups)?;
    cleaned_any = true;
  }

  if cleaned_any {
    println!("✨ Clean complete");
  } else {
    println!("Nothing to clean");
  }

  Ok(())
}

fn clean_metadata_cache(ctx: &WorkspaceContext) -> RailResult<()> {
  let rail_target = ctx
    .workspace_root
    .join("target")
    .join("cargo-rail")
    .join("metadata.json");
  // Also clean old path if it exists
  let old_rail_target = ctx.workspace_root.join("target").join("rail");

  if rail_target.exists() {
    println!("🗑️  Cleaning metadata cache...");
    fs::remove_file(&rail_target).map_err(|e| {
      RailError::message(format!(
        "Failed to remove metadata cache at {}: {}",
        rail_target.display(),
        e
      ))
    })?;
  }

  if old_rail_target.exists() {
    println!("🗑️  Cleaning legacy metadata cache...");
    fs::remove_dir_all(&old_rail_target).map_err(|e| {
      RailError::message(format!(
        "Failed to remove legacy metadata cache at {}: {}",
        old_rail_target.display(),
        e
      ))
    })?;
  }

  Ok(())
}

fn clean_generated_reports(ctx: &WorkspaceContext) -> RailResult<()> {
  let report_dir = ctx.workspace_root.join("target").join("cargo-rail");

  // We need to be careful not to delete the whole directory if it contains backups or metadata
  // But wait, now everything is in target/cargo-rail/
  // So we should only delete *.md files or specific report files?
  // Or maybe we just clean everything EXCEPT backups and metadata.json if we are only cleaning reports?
  // Actually, reports are likely just markdown files in that dir.

  if report_dir.exists() {
    println!("🗑️  Cleaning generated reports...");
    for entry in fs::read_dir(&report_dir).map_err(|e| RailError::message(format!("Failed to read dir: {}", e)))? {
      let entry = entry.map_err(|e| RailError::message(format!("Failed to read entry: {}", e)))?;
      let path = entry.path();
      if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
        fs::remove_file(&path)
          .map_err(|e| RailError::message(format!("Failed to remove report {}: {}", path.display(), e)))?;
      }
    }
  }
  Ok(())
}

fn clean_backups_handler(ctx: &WorkspaceContext, delete_all: bool) -> RailResult<()> {
  let backup_manager = BackupManager::new(&ctx.workspace_root);

  if !backup_manager.has_backups() {
    return Ok(());
  }

  if delete_all {
    println!("🗑️  Removing ALL backups...");
    // BackupManager doesn't have a "delete all" method exposed directly other than cleanup_old_backups(0)
    // which we verified does exactly that.
    let count = backup_manager.cleanup_old_backups(0)?;
    println!("   Removed {} backups", count);
  } else {
    // Prune to max_backups
    let max_backups = ctx
      .config
      .as_ref()
      .map(|c| c.unify.max_backups)
      .unwrap_or_else(|| UnifyConfig::default().max_backups);
    println!("🧹 Pruning backups (keeping latest {})...", max_backups);
    let count = backup_manager.cleanup_old_backups(max_backups)?;
    if count > 0 {
      println!("   Removed {} old backups", count);
    } else {
      println!("   No backups to prune");
    }
  }

  Ok(())
}
