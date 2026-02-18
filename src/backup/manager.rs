//! Backup manager - handles backup creation, restoration, and listing

use super::metadata::{BackupMetadata, BackupRecord};
use super::{BackupId, create_backup_id, get_backup_dir, get_backup_root};
use crate::error::{RailError, RailResult};
use std::fs;
use std::path::PathBuf;

/// Manages backups for a workspace
pub struct BackupManager {
  workspace_root: PathBuf,
  backup_root: PathBuf,
}

impl BackupManager {
  /// Create a new backup manager for a workspace
  pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
    let workspace_root = workspace_root.into();
    let backup_root = get_backup_root(&workspace_root);
    Self {
      workspace_root,
      backup_root,
    }
  }

  /// Create a backup of specified files
  ///
  /// Backs up files relative to the workspace root. When `max_backups` is `0`,
  /// backup creation is skipped and a placeholder id is returned.
  pub fn create_backup(
    &self,
    files: &[PathBuf],
    mut metadata: BackupMetadata,
    max_backups: usize,
  ) -> RailResult<BackupId> {
    // max_backups = 0 means no backups
    if max_backups == 0 {
      return Ok("none".to_string());
    }

    let backup_id = create_backup_id();
    let backup_dir = get_backup_dir(&self.workspace_root, &backup_id);

    fs::create_dir_all(&backup_dir)
      .map_err(|e| RailError::message(format!("failed to create {}: {}", backup_dir.display(), e)))?;

    for file in files {
      let src = self.workspace_root.join(file);
      let dest = backup_dir.join(file);

      if !src.exists() {
        continue;
      }

      if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
          .map_err(|e| RailError::message(format!("failed to create {}: {}", parent.display(), e)))?;
      }

      fs::copy(&src, &dest).map_err(|e| RailError::message(format!("failed to backup {}: {}", src.display(), e)))?;

      metadata.add_file(file.clone());
    }

    metadata.save(&backup_dir)?;

    // Cleanup old backups (max_backups is guaranteed > 0 here)
    let _deleted = self.cleanup_old_backups(max_backups)?;

    Ok(backup_id)
  }

  /// Restore a backup
  pub fn restore_backup(&self, backup_id: &str) -> RailResult<()> {
    let backup_dir = get_backup_dir(&self.workspace_root, backup_id);

    if !backup_dir.exists() {
      return Err(RailError::message(format!("backup '{}' not found", backup_id)));
    }

    let metadata = BackupMetadata::load(&backup_dir)?;

    crate::status!("restoring backup: {}", metadata.timestamp);
    crate::status!("  {} files", metadata.files_modified.len());

    for file in &metadata.files_modified {
      let src = backup_dir.join(file);
      let dest = self.workspace_root.join(file);

      if !src.exists() {
        crate::status!("  skipped (missing): {}", file.display());
        continue;
      }

      if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
          .map_err(|e| RailError::message(format!("failed to create {}: {}", parent.display(), e)))?;
      }

      fs::copy(&src, &dest).map_err(|e| RailError::message(format!("failed to restore {}: {}", file.display(), e)))?;

      crate::status!("  restored: {}", file.display());
    }

    println!("backup restored");

    Ok(())
  }

  /// List all backups (newest first)
  pub fn list_backups(&self) -> RailResult<Vec<BackupRecord>> {
    if !self.backup_root.exists() {
      return Ok(Vec::new());
    }

    let mut backups = Vec::new();

    let entries = fs::read_dir(&self.backup_root)
      .map_err(|e| RailError::message(format!("failed to read {}: {}", self.backup_root.display(), e)))?;

    for entry in entries {
      let entry = entry.map_err(|e| RailError::message(format!("failed to read entry: {}", e)))?;

      let path = entry.path();
      if !path.is_dir() {
        continue;
      }

      let backup_id = match path.file_name() {
        Some(name) => name.to_string_lossy().to_string(),
        None => continue,
      };

      match BackupMetadata::load(&path) {
        Ok(metadata) => {
          backups.push(BackupRecord::new(backup_id, metadata, path));
        }
        Err(e) => {
          crate::warn!("skipping corrupted backup '{}': {}", backup_id, e);
          continue;
        }
      }
    }

    backups.sort_by(|a, b| b.metadata.timestamp.cmp(&a.metadata.timestamp));

    Ok(backups)
  }

  /// Get the most recent backup
  pub fn get_latest_backup(&self) -> RailResult<Option<BackupRecord>> {
    let backups = self.list_backups()?;
    Ok(backups.into_iter().next())
  }

  /// Delete old backups, keeping only the most recent N
  pub fn cleanup_old_backups(&self, keep_count: usize) -> RailResult<usize> {
    let backups = self.list_backups()?;

    if backups.len() <= keep_count {
      return Ok(0);
    }

    let to_delete = &backups[keep_count..];
    let deleted_count = to_delete.len();

    for backup in to_delete {
      fs::remove_dir_all(&backup.path)
        .map_err(|e| RailError::message(format!("failed to delete {}: {}", backup.id, e)))?;
    }

    Ok(deleted_count)
  }

  /// Check if any backups exist
  pub fn has_backups(&self) -> bool {
    self.backup_root.exists()
      && self
        .backup_root
        .read_dir()
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

  fn create_test_workspace() -> TempDir {
    let temp = TempDir::new().unwrap();
    let workspace = temp.path();

    // Create test files
    fs::write(workspace.join("Cargo.toml"), "# Root Cargo.toml").unwrap();
    fs::create_dir_all(workspace.join("crates/foo")).unwrap();
    fs::write(workspace.join("crates/foo/Cargo.toml"), "# Foo Cargo.toml").unwrap();

    temp
  }

  #[test]
  fn test_backup_manager_creation() {
    let workspace = create_test_workspace();
    let manager = BackupManager::new(workspace.path());

    assert_eq!(manager.workspace_root, workspace.path());
    assert!(manager.backup_root.ends_with("target/cargo-rail/backups"));
  }

  #[test]
  fn test_create_and_restore_backup() -> RailResult<()> {
    let workspace = create_test_workspace();
    let manager = BackupManager::new(workspace.path());

    // Files to backup
    let files = vec![PathBuf::from("Cargo.toml"), PathBuf::from("crates/foo/Cargo.toml")];

    // Create backup (use max_backups=10 to keep multiple backups)
    let metadata = BackupMetadata::new("test command");
    let backup_id = manager.create_backup(&files, metadata, 10)?;

    // Verify backup was created
    let backup_dir = get_backup_dir(workspace.path(), &backup_id);
    assert!(backup_dir.exists());
    assert!(backup_dir.join("Cargo.toml").exists());
    assert!(backup_dir.join("crates/foo/Cargo.toml").exists());
    assert!(backup_dir.join("metadata.json").exists());

    // Modify original files
    fs::write(workspace.path().join("Cargo.toml"), "# Modified").unwrap();

    // Restore backup
    manager.restore_backup(&backup_id)?;

    // Verify restoration
    let content = fs::read_to_string(workspace.path().join("Cargo.toml")).unwrap();
    assert_eq!(content, "# Root Cargo.toml");

    Ok(())
  }

  #[test]
  fn test_list_backups() -> RailResult<()> {
    let workspace = create_test_workspace();
    let manager = BackupManager::new(workspace.path());

    // Initially no backups
    assert!(!manager.has_backups());
    let backups = manager.list_backups()?;
    assert_eq!(backups.len(), 0);

    // Create a backup (use max_backups=10 to keep multiple backups)
    let files = vec![PathBuf::from("Cargo.toml")];
    let metadata = BackupMetadata::new("test 1");
    manager.create_backup(&files, metadata, 10)?;

    // Should now have 1 backup
    assert!(manager.has_backups());
    let backups = manager.list_backups()?;
    assert_eq!(backups.len(), 1);
    assert_eq!(backups[0].metadata.command, "test 1");

    // Sleep to ensure different timestamp
    std::thread::sleep(std::time::Duration::from_secs(1));

    // Create another backup
    let metadata2 = BackupMetadata::new("test 2");
    manager.create_backup(&files, metadata2, 10)?;

    // Should have 2 backups
    let backups = manager.list_backups()?;
    assert_eq!(backups.len(), 2);

    Ok(())
  }

  #[test]
  fn test_cleanup_old_backups() -> RailResult<()> {
    let workspace = create_test_workspace();
    let manager = BackupManager::new(workspace.path());

    // Create 5 backups with sufficient delay for unique timestamps
    // Use max_backups=100 so all 5 are kept initially
    let files = vec![PathBuf::from("Cargo.toml")];
    for i in 1..=5 {
      let metadata = BackupMetadata::new(format!("test {}", i));
      manager.create_backup(&files, metadata, 100)?;
      // Sleep 1 second to ensure different timestamps (format is YYYY-MM-DD-HHMMSS)
      std::thread::sleep(std::time::Duration::from_secs(1));
    }

    // Verify we have 5 backups
    let backups = manager.list_backups()?;
    assert_eq!(backups.len(), 5);

    // Keep only 3 most recent
    let deleted = manager.cleanup_old_backups(3)?;
    assert_eq!(deleted, 2);

    // Should now have 3 backups
    let backups = manager.list_backups()?;
    assert_eq!(backups.len(), 3);

    Ok(())
  }

  #[test]
  fn test_get_latest_backup() -> RailResult<()> {
    let workspace = create_test_workspace();
    let manager = BackupManager::new(workspace.path());

    // Initially no backups
    assert!(manager.get_latest_backup()?.is_none());

    // Create backups with sufficient delay (use max_backups=10 to keep them)
    let files = vec![PathBuf::from("Cargo.toml")];
    manager.create_backup(&files, BackupMetadata::new("first"), 10)?;
    std::thread::sleep(std::time::Duration::from_secs(1));
    manager.create_backup(&files, BackupMetadata::new("second"), 10)?;

    // Latest should be "second"
    let latest = manager.get_latest_backup()?.unwrap();
    assert_eq!(latest.metadata.command, "second");

    Ok(())
  }

  #[test]
  fn test_max_backups_zero_disables_backup() -> RailResult<()> {
    let workspace = create_test_workspace();
    let manager = BackupManager::new(workspace.path());

    let files = vec![PathBuf::from("Cargo.toml")];

    // With max_backups = 0, no backup should be created
    let backup_id = manager.create_backup(&files, BackupMetadata::new("test"), 0)?;
    assert_eq!(backup_id, "none");

    // Should have no backups
    assert!(!manager.has_backups());
    let backups = manager.list_backups()?;
    assert_eq!(backups.len(), 0);

    Ok(())
  }
}
