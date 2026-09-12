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

    /// Command that created this backup (e.g., "cargo rail unify")
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

        crate::utils::write_file_atomic(&metadata_path, content.as_bytes())
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
        let mut metadata = BackupMetadata::new("cargo rail unify");
        assert_eq!(metadata.command, "cargo rail unify");
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
    fn metadata_save_uses_public_fields_and_omits_absent_options() {
        let dir = TempDir::new().unwrap();
        let metadata = BackupMetadata {
            timestamp: "2024-01-15T14:30:22+00:00".to_owned(),
            command: "cargo rail unify".to_owned(),
            files_modified: vec![PathBuf::from("Cargo.toml")],
            config_snapshot: None,
            description: None,
        };
        metadata.save(dir.path()).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join("metadata.json")).unwrap()).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "timestamp": "2024-01-15T14:30:22+00:00",
                "command": "cargo rail unify",
                "files_modified": ["Cargo.toml"]
            })
        );
    }

    #[test]
    fn metadata_load_reads_independently_authored_record() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("metadata.json"), r#"{"timestamp":"2024-01-15T14:30:22+00:00","command":"cargo rail unify","files_modified":["Cargo.toml","crates/foo/Cargo.toml"],"description":"Before unify"}"#).unwrap();
        let metadata = BackupMetadata::load(dir.path()).unwrap();
        assert_eq!(metadata.timestamp, "2024-01-15T14:30:22+00:00");
        assert_eq!(metadata.command, "cargo rail unify");
        assert_eq!(
            metadata.files_modified,
            [PathBuf::from("Cargo.toml"), PathBuf::from("crates/foo/Cargo.toml")]
        );
        assert_eq!(metadata.description.as_deref(), Some("Before unify"));
        assert!(metadata.config_snapshot.is_none());
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
        assert_eq!(display, "2024-01-15 14:30:22");
    }
}
