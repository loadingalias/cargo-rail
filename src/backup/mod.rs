//! Backup and restore functionality for cargo-rail operations
//!
//! Provides automatic backup/restore of workspace files to enable safe
//! undo operations. Backups are stored in `target/.cargo-rail/backups/`
//! to keep them out of version control and the main workspace tree.
//!
//! # Architecture
//!
//! - Each backup is stored in a timestamped directory
//! - `metadata.json` tracks what was backed up and why
//! - Original file structure is preserved for easy restoration
//! - Automatic cleanup keeps only recent backups
//!
//! # Usage
//!
//! ```rust,ignore
//! use cargo_rail::backup::{BackupManager, BackupMetadata};
//!
//! let manager = BackupManager::new(workspace_root);
//!
//! // Create backup before modifications
//! let metadata = BackupMetadata::new("cargo rail unify apply");
//! let backup_id = manager.create_backup(&files_to_backup, metadata)?;
//!
//! // Restore if needed
//! manager.restore_backup(&backup_id)?;
//!
//! // List available backups
//! let backups = manager.list_backups()?;
//! ```

use std::path::{Path, PathBuf};

mod manager;
mod metadata;

pub use manager::BackupManager;
pub use metadata::{BackupMetadata, BackupRecord};

/// Backup identifier (timestamp-based)
pub type BackupId = String;

/// Creates a backup ID from current timestamp
pub fn create_backup_id() -> BackupId {
  chrono::Local::now().format("%Y-%m-%d-%H%M%S").to_string()
}

/// Get the backup root directory for a workspace
///
/// Returns `<workspace_root>/target/.cargo-rail/backups/`
pub fn get_backup_root(workspace_root: &Path) -> PathBuf {
  workspace_root.join("target").join(".cargo-rail").join("backups")
}

/// Get the path to a specific backup directory
pub fn get_backup_dir(workspace_root: &Path, backup_id: &str) -> PathBuf {
  get_backup_root(workspace_root).join(backup_id)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_backup_id_format() {
    let id = create_backup_id();
    // Should be in format YYYY-MM-DD-HHMMSS
    assert_eq!(id.len(), 17); // 2024-01-15-143022
    assert_eq!(id.chars().nth(4), Some('-'));
    assert_eq!(id.chars().nth(7), Some('-'));
    assert_eq!(id.chars().nth(10), Some('-'));
  }

  #[test]
  fn test_backup_root_path() {
    let workspace = PathBuf::from("/workspace");
    let backup_root = get_backup_root(&workspace);
    assert_eq!(backup_root, PathBuf::from("/workspace/target/.cargo-rail/backups"));
  }

  #[test]
  fn test_backup_dir_path() {
    let workspace = PathBuf::from("/workspace");
    let backup_dir = get_backup_dir(&workspace, "2024-01-15-143022");
    assert_eq!(
      backup_dir,
      PathBuf::from("/workspace/target/.cargo-rail/backups/2024-01-15-143022")
    );
  }
}
