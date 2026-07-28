//! Contained workspace-file backups for mutation recovery.
//!
//! Each timestamped directory under `target/cargo-rail/backups` preserves the
//! original relative file layout and records it in `metadata.json`.

use std::path::{Path, PathBuf};

mod manager;
mod metadata;

pub use manager::BackupManager;
pub use metadata::{BackupMetadata, BackupRecord};

/// Backup identifier (timestamp-based)
pub type BackupId = String;

/// Creates a backup ID from current timestamp.
#[doc(hidden)]
pub fn create_backup_id() -> BackupId {
  chrono::Local::now().format("%Y-%m-%d-%H%M%S-%3f").to_string()
}

/// Get the backup root directory for a workspace.
#[doc(hidden)]
pub fn get_backup_root(workspace_root: &Path) -> PathBuf {
  crate::workspace::cargo_rail_state_root(workspace_root).join("backups")
}

/// Get the path to a specific backup directory.
#[doc(hidden)]
pub fn get_backup_dir(workspace_root: &Path, backup_id: &str) -> PathBuf {
  get_backup_root(workspace_root).join(backup_id)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_backup_id_format() {
    let id = create_backup_id();
    // Should be in format YYYY-MM-DD-HHMMSS-mmm
    assert_eq!(id.len(), 21); // 2024-01-15-143022-123
    assert_eq!(id.chars().nth(4), Some('-'));
    assert_eq!(id.chars().nth(7), Some('-'));
    assert_eq!(id.chars().nth(10), Some('-'));
    assert_eq!(id.chars().nth(17), Some('-'));
  }

  #[test]
  fn test_backup_root_path() {
    let workspace = PathBuf::from("/workspace");
    let backup_root = get_backup_root(&workspace);
    assert_eq!(backup_root, PathBuf::from("/workspace/target/cargo-rail/backups"));
  }

  #[test]
  fn test_backup_dir_path() {
    let workspace = PathBuf::from("/workspace");
    let backup_dir = get_backup_dir(&workspace, "2024-01-15-143022");
    assert_eq!(
      backup_dir,
      PathBuf::from("/workspace/target/cargo-rail/backups/2024-01-15-143022")
    );
  }
}
