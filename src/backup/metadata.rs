//! Backup metadata tracking
//!
//! Stores information about what was backed up, when, and why.

use crate::config::RailConfig;
use crate::error::{RailError, RailResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Metadata describing a backup
///
/// Stored as `metadata.json` in each backup directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupMetadata {
  /// When the backup was created (ISO 8601 format)
  pub timestamp: String,

  /// Command that created this backup (e.g., "cargo rail unify apply")
  pub command: String,

  /// Files that were backed up (relative to workspace root)
  pub files_modified: Vec<PathBuf>,

  /// Snapshot of rail.toml config at time of backup (optional)
  #[serde(skip_serializing_if = "Option::is_none")]
  pub config_snapshot: Option<RailConfig>,

  /// Optional description of what this backup contains
  #[serde(skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
}

impl BackupMetadata {
  /// Create new backup metadata
  pub fn new(command: impl Into<String>) -> Self {
    Self {
      timestamp: chrono::Local::now().to_rfc3339(),
      command: command.into(),
      files_modified: Vec::new(),
      config_snapshot: None,
      description: None,
    }
  }

  /// Add a file to the backup
  pub fn add_file(&mut self, path: impl Into<PathBuf>) {
    self.files_modified.push(path.into());
  }

  /// Set the config snapshot
  pub fn with_config(mut self, config: RailConfig) -> Self {
    self.config_snapshot = Some(config);
    self
  }

  /// Set a description
  pub fn with_description(mut self, description: impl Into<String>) -> Self {
    self.description = Some(description.into());
    self
  }

  /// Load metadata from a backup directory
  pub fn load(backup_dir: &Path) -> RailResult<Self> {
    let metadata_path = backup_dir.join("metadata.json");
    let content = fs::read_to_string(&metadata_path).map_err(|e| {
      RailError::message(format!(
        "Failed to read backup metadata from {}: {}",
        metadata_path.display(),
        e
      ))
    })?;

    serde_json::from_str(&content).map_err(|e| {
      RailError::message(format!(
        "Failed to parse backup metadata from {}: {}",
        metadata_path.display(),
        e
      ))
    })
  }

  /// Save metadata to a backup directory
  pub fn save(&self, backup_dir: &Path) -> RailResult<()> {
    let metadata_path = backup_dir.join("metadata.json");
    let content = serde_json::to_string_pretty(self)
      .map_err(|e| RailError::message(format!("Failed to serialize backup metadata: {}", e)))?;

    fs::write(&metadata_path, content).map_err(|e| {
      RailError::message(format!(
        "Failed to write backup metadata to {}: {}",
        metadata_path.display(),
        e
      ))
    })?;

    Ok(())
  }
}

/// A record of an existing backup (for listing)
#[derive(Debug, Clone)]
pub struct BackupRecord {
  /// Backup identifier
  pub id: String,

  /// Backup metadata
  pub metadata: BackupMetadata,

  /// Full path to backup directory
  pub path: PathBuf,
}

impl BackupRecord {
  /// Create a new backup record
  pub fn new(id: String, metadata: BackupMetadata, path: PathBuf) -> Self {
    Self { id, metadata, path }
  }

  /// Get human-readable timestamp
  pub fn timestamp_display(&self) -> String {
    // Parse RFC3339 and format for display
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&self.metadata.timestamp) {
      dt.format("%Y-%m-%d %H:%M:%S").to_string()
    } else {
      self.metadata.timestamp.clone()
    }
  }

  /// Get file count
  pub fn file_count(&self) -> usize {
    self.metadata.files_modified.len()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

  #[test]
  fn test_backup_metadata_creation() {
    let mut metadata = BackupMetadata::new("cargo rail unify apply");
    assert_eq!(metadata.command, "cargo rail unify apply");
    assert!(metadata.files_modified.is_empty());
    assert!(metadata.config_snapshot.is_none());

    metadata.add_file("Cargo.toml");
    metadata.add_file("crates/foo/Cargo.toml");
    assert_eq!(metadata.files_modified.len(), 2);
  }

  #[test]
  fn test_backup_metadata_with_description() {
    let metadata = BackupMetadata::new("test command").with_description("Test backup");
    assert_eq!(metadata.description, Some("Test backup".to_string()));
  }

  #[test]
  fn test_backup_metadata_save_load() -> RailResult<()> {
    let temp_dir = TempDir::new().unwrap();
    let backup_dir = temp_dir.path();

    // Create and save metadata
    let mut original = BackupMetadata::new("cargo rail unify apply");
    original.add_file("Cargo.toml");
    original.add_file("crates/foo/Cargo.toml");
    original = original.with_description("Test backup");

    original.save(backup_dir)?;

    // Load it back
    let loaded = BackupMetadata::load(backup_dir)?;

    assert_eq!(loaded.command, original.command);
    assert_eq!(loaded.files_modified, original.files_modified);
    assert_eq!(loaded.description, original.description);

    Ok(())
  }

  #[test]
  fn test_backup_record_timestamp_display() {
    let metadata = BackupMetadata {
      timestamp: "2024-01-15T14:30:22+00:00".to_string(),
      command: "test".to_string(),
      files_modified: vec![],
      config_snapshot: None,
      description: None,
    };

    let record = BackupRecord::new("2024-01-15-143022".to_string(), metadata, PathBuf::from("/tmp/backup"));

    let display = record.timestamp_display();
    assert!(display.contains("2024-01-15"));
    assert!(display.contains("14:30:22"));
  }
}
