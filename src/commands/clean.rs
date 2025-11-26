use crate::backup::BackupManager;
use crate::config::UnifyConfig;
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;
use std::fs;

/// Run the clean command
pub fn run_clean(ctx: &WorkspaceContext, cache: bool, backups: bool, reports: bool, check: bool) -> RailResult<()> {
  // If no flags provided, clean everything (cache, reports, and ALL backups)
  let clean_all = !cache && !backups && !reports;

  let clean_cache = cache || clean_all;
  let clean_reports = reports || clean_all;
  // If specific backup flag provided, prune. If cleaning all, delete all.
  let prune_backups = backups && !clean_all;
  let delete_all_backups = clean_all;

  // Dry-run mode: show what would be cleaned
  if check {
    println!("🔍 DRY-RUN MODE - Showing what would be cleaned:\n");
    let mut would_clean = false;

    if clean_cache {
      let cache_path = ctx
        .workspace_root
        .join("target")
        .join("cargo-rail")
        .join("metadata.json");
      let old_cache = ctx.workspace_root.join("target").join("rail");

      if cache_path.exists() {
        println!("  📄 {}", cache_path.display());
        would_clean = true;
      }
      if old_cache.exists() {
        println!("  📁 {} (legacy)", old_cache.display());
        would_clean = true;
      }
    }

    if clean_reports {
      let report_dir = ctx.workspace_root.join("target").join("cargo-rail");
      if report_dir.exists() {
        for entry in fs::read_dir(&report_dir).ok().into_iter().flatten().flatten() {
          let path = entry.path();
          if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            println!("  📄 {}", path.display());
            would_clean = true;
          }
        }
      }
    }

    if prune_backups || delete_all_backups {
      let backup_manager = BackupManager::new(&ctx.workspace_root);
      if backup_manager.has_backups() {
        let backup_list = backup_manager.list_backups()?;
        if delete_all_backups {
          for backup in &backup_list {
            println!("  📦 backup: {}", backup.id);
            would_clean = true;
          }
        } else {
          let max_backups = ctx
            .config
            .as_ref()
            .map(|c| c.unify.max_backups)
            .unwrap_or_else(|| UnifyConfig::default().max_backups);
          if backup_list.len() > max_backups {
            for backup in backup_list.iter().skip(max_backups) {
              println!("  📦 backup (prune): {}", backup.id);
              would_clean = true;
            }
          }
        }
      }
    }

    if would_clean {
      println!("\n✋ To execute cleanup, run: cargo rail clean");
    } else {
      println!("Nothing to clean");
    }
    return Ok(());
  }

  // Execute cleanup
  let mut cleaned_any = false;

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
