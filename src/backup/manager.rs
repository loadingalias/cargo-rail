//! Backup manager - handles backup creation, restoration, and listing

use super::metadata::{BackupMetadata, BackupRecord};
use super::{BackupId, create_backup_id, get_backup_root};
use crate::error::{RailError, RailResult};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Manages backups for a workspace
#[derive(Debug)]
pub struct BackupManager {
    workspace_root: PathBuf,
    backup_root: PathBuf,
}

impl BackupManager {
    /// Create a new backup manager for a workspace
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        let workspace_root = if workspace_root.is_absolute() {
            workspace_root
        } else {
            std::env::current_dir()
                .map(|current| current.join(&workspace_root))
                .unwrap_or(workspace_root)
        };
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

        let workspace_root = canonical_directory(&self.workspace_root, "workspace root")?;
        let backup_root_path = get_backup_root(&workspace_root);
        let backup_root = prepare_contained_directory(&workspace_root, &backup_root_path, "backup root")?;
        let sources = files
            .iter()
            .map(|file| {
                validate_relative_entry(file, "backup file")?;
                let source = contained_path(&workspace_root, file, "backup source")?;
                match fs::symlink_metadata(&source) {
                    Ok(file_type) if file_type.file_type().is_symlink() => Err(RailError::message(format!(
                        "backup source '{}' must not be a symlink",
                        file.display()
                    ))),
                    Ok(file_type) if file_type.is_file() => Ok(Some((file.clone(), source))),
                    Ok(_) => Err(RailError::message(format!(
                        "backup source '{}' is not a regular file",
                        file.display()
                    ))),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(error) => Err(RailError::message(format!(
                        "failed to inspect backup source '{}': {}",
                        file.display(),
                        error
                    ))),
                }
            })
            .collect::<RailResult<Vec<_>>>()?;

        let backup_id = create_backup_id();
        validate_backup_id(&backup_id)?;
        let backup_dir = backup_root.join(&backup_id);

        fs::create_dir(&backup_dir)
            .map_err(|e| RailError::message(format!("failed to create {}: {}", backup_dir.display(), e)))?;
        let backup_dir = canonical_directory(&backup_dir, "backup directory")?;

        for source in sources {
            let Some((file, source)) = source else {
                continue;
            };
            let dest = contained_path(&backup_dir, &file, "backup destination")?;

            if let Some(parent) = dest.parent()
                && parent != backup_dir
            {
                prepare_contained_directory(&backup_dir, parent, "backup destination directory")?;
            }

            revalidate_regular_file(&workspace_root, &source, "backup source")?;
            revalidate_destination(&backup_dir, &dest, "backup destination")?;
            fs::copy(&source, &dest)
                .map_err(|e| RailError::message(format!("failed to backup {}: {}", source.display(), e)))?;

            metadata.add_file(file);
        }

        metadata.save(&backup_dir)?;

        // Cleanup old backups (max_backups is guaranteed > 0 here)
        let _deleted = self.cleanup_old_backups(max_backups)?;

        Ok(backup_id)
    }

    /// Restore a backup
    pub fn restore_backup(&self, backup_id: &str) -> RailResult<()> {
        validate_backup_id(backup_id)?;
        let workspace_root = canonical_directory(&self.workspace_root, "workspace root")?;
        let backup_root = canonical_directory(&self.backup_root, "backup root")?;
        if !backup_root.starts_with(&workspace_root) || backup_root == workspace_root {
            return Err(RailError::message(format!(
                "backup root '{}' is outside '{}'",
                self.backup_root.display(),
                workspace_root.display()
            )));
        }
        let backup_dir = validate_backup_directory(&backup_root, &backup_root.join(backup_id))?;

        let metadata = BackupMetadata::load(&backup_dir)?;

        crate::status!("restoring backup: {}", metadata.timestamp);
        crate::status!("  {} files", metadata.files_modified.len());

        for file in &metadata.files_modified {
            validate_relative_entry(file, "backup metadata file")?;
            let source = contained_path(&backup_dir, file, "backup source")?;
            let destination = contained_path(&workspace_root, file, "restore destination")?;
            revalidate_regular_file(&backup_dir, &source, "backup source")?;

            if let Some(parent) = destination.parent()
                && parent != workspace_root
            {
                prepare_contained_directory(&workspace_root, parent, "restore destination directory")?;
            }

            revalidate_destination(&workspace_root, &destination, "restore destination")?;
            fs::copy(&source, &destination)
                .map_err(|e| RailError::message(format!("failed to restore {}: {}", file.display(), e)))?;

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
        let workspace_root = canonical_directory(&self.workspace_root, "workspace root")?;
        let backup_root = canonical_directory(&self.backup_root, "backup root")?;
        if !backup_root.starts_with(&workspace_root) || backup_root == workspace_root {
            return Err(RailError::message(format!(
                "backup root '{}' is outside '{}'",
                self.backup_root.display(),
                workspace_root.display()
            )));
        }

        let mut backups = Vec::new();

        let entries = fs::read_dir(&backup_root)
            .map_err(|e| RailError::message(format!("failed to read {}: {}", backup_root.display(), e)))?;

        for entry in entries {
            let entry = entry.map_err(|e| RailError::message(format!("failed to read entry: {}", e)))?;

            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|e| RailError::message(format!("failed to inspect {}: {}", path.display(), e)))?;
            if !file_type.is_dir() || file_type.is_symlink() {
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

        let backup_root = canonical_directory(&self.backup_root, "backup root")?;
        for backup in to_delete {
            let backup_path = validate_backup_directory(&backup_root, &backup.path)?;
            fs::remove_dir_all(&backup_path)
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

fn validate_relative_entry(path: &Path, description: &str) -> RailResult<()> {
    let mut components = path.components();
    let Some(Component::Normal(_)) = components.next() else {
        return Err(RailError::message(format!(
            "{description} '{}' must be a non-empty relative path",
            path.display()
        )));
    };
    if components.any(|component| !matches!(component, Component::Normal(_))) {
        return Err(RailError::message(format!(
            "{description} '{}' must not contain '.', '..', a root, or a platform prefix",
            path.display()
        )));
    }
    Ok(())
}

fn validate_backup_id(backup_id: &str) -> RailResult<()> {
    let path = Path::new(backup_id);
    validate_relative_entry(path, "backup id")?;
    if path.components().count() != 1 {
        return Err(RailError::message(format!(
            "backup id '{}' must identify one backup directory",
            backup_id
        )));
    }
    Ok(())
}

fn canonical_directory(path: &Path, description: &str) -> RailResult<PathBuf> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        RailError::message(format!("failed to inspect {description} '{}': {error}", path.display()))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(RailError::message(format!(
            "{description} '{}' must be a real directory",
            path.display()
        )));
    }
    crate::utils::canonicalize_existing(path)
        .map_err(|error| RailError::message(format!("failed to resolve {description} '{}': {error}", path.display())))
}

fn contained_path(root: &Path, path: &Path, description: &str) -> RailResult<PathBuf> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let resolved = crate::utils::canonicalize_allow_missing(&candidate).map_err(|error| {
        RailError::message(format!("failed to resolve {description} '{}': {error}", path.display()))
    })?;
    if !resolved.starts_with(root) || resolved == root {
        return Err(RailError::message(format!(
            "{description} '{}' is outside '{}'",
            path.display(),
            root.display()
        )));
    }
    Ok(candidate)
}

fn prepare_contained_directory(root: &Path, path: &Path, description: &str) -> RailResult<PathBuf> {
    let resolved = contained_path(root, path, description)?;
    fs::create_dir_all(&resolved)
        .map_err(|error| RailError::message(format!("failed to create {description} '{}': {error}", path.display())))?;
    let canonical = canonical_directory(&resolved, description)?;
    if !canonical.starts_with(root) || canonical == root {
        return Err(RailError::message(format!(
            "{description} '{}' is outside '{}'",
            path.display(),
            root.display()
        )));
    }
    Ok(canonical)
}

fn validate_backup_directory(backup_root: &Path, path: &Path) -> RailResult<PathBuf> {
    let canonical = canonical_directory(path, "backup directory").map_err(|error| {
        if !path.exists() {
            RailError::message(format!(
                "backup '{}' not found",
                path.file_name().unwrap_or_default().to_string_lossy()
            ))
        } else {
            error
        }
    })?;
    if canonical.parent() != Some(backup_root) {
        return Err(RailError::message(format!(
            "backup directory '{}' is outside '{}'",
            path.display(),
            backup_root.display()
        )));
    }
    Ok(canonical)
}

fn revalidate_regular_file(root: &Path, path: &Path, description: &str) -> RailResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        RailError::message(format!("failed to inspect {description} '{}': {error}", path.display()))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(RailError::message(format!(
            "{description} '{}' must be a regular file",
            path.display()
        )));
    }
    let canonical = crate::utils::canonicalize_existing(path).map_err(|error| {
        RailError::message(format!("failed to resolve {description} '{}': {error}", path.display()))
    })?;
    if !canonical.starts_with(root) || canonical == root {
        return Err(RailError::message(format!(
            "{description} '{}' is outside '{}'",
            path.display(),
            root.display()
        )));
    }
    Ok(())
}

fn revalidate_destination(root: &Path, path: &Path, description: &str) -> RailResult<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(RailError::message(format!(
                "{description} '{}' must be a regular file or absent",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(RailError::message(format!(
                "failed to inspect {description} '{}': {error}",
                path.display()
            )));
        }
    }
    let resolved = contained_path(root, path, description)?;
    let parent = resolved
        .parent()
        .ok_or_else(|| RailError::message(format!("{description} '{}' has no parent", path.display())))?;
    let canonical_parent = canonical_directory(parent, description)?;
    if !canonical_parent.starts_with(root) {
        return Err(RailError::message(format!(
            "{description} '{}' is outside '{}'",
            path.display(),
            root.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::get_backup_dir;
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
    fn test_create_and_restore_backup() {
        let result: RailResult<()> = (|| {
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
        })();
        result.unwrap();
    }

    #[test]
    fn test_list_backups() {
        let result: RailResult<()> = (|| {
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

            std::thread::sleep(std::time::Duration::from_millis(10));

            // Create another backup
            let metadata2 = BackupMetadata::new("test 2");
            manager.create_backup(&files, metadata2, 10)?;

            // Should have 2 backups
            let backups = manager.list_backups()?;
            assert_eq!(backups.len(), 2);

            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn test_cleanup_old_backups() {
        let result: RailResult<()> = (|| {
            let workspace = create_test_workspace();
            let manager = BackupManager::new(workspace.path());

            let files = vec![PathBuf::from("Cargo.toml")];
            for i in 1..=5 {
                let metadata = BackupMetadata::new(format!("test {}", i));
                manager.create_backup(&files, metadata, 100)?;
                std::thread::sleep(std::time::Duration::from_millis(10));
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
        })();
        result.unwrap();
    }

    #[test]
    fn test_get_latest_backup() {
        let result: RailResult<()> = (|| {
            let workspace = create_test_workspace();
            let manager = BackupManager::new(workspace.path());

            // Initially no backups
            assert!(manager.get_latest_backup()?.is_none());

            let files = vec![PathBuf::from("Cargo.toml")];
            manager.create_backup(&files, BackupMetadata::new("first"), 10)?;
            std::thread::sleep(std::time::Duration::from_millis(10));
            manager.create_backup(&files, BackupMetadata::new("second"), 10)?;

            // Latest should be "second"
            let latest = manager.get_latest_backup()?.unwrap();
            assert_eq!(latest.metadata.command, "second");

            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn test_max_backups_zero_disables_backup() {
        let result: RailResult<()> = (|| {
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
        })();
        result.unwrap();
    }

    #[test]
    fn backup_paths_must_be_workspace_relative() {
        let workspace = create_test_workspace();
        let manager = BackupManager::new(workspace.path());

        for path in [PathBuf::from("../Cargo.toml"), workspace.path().join("Cargo.toml")] {
            let error = manager
                .create_backup(&[path], BackupMetadata::new("test"), 10)
                .unwrap_err();
            assert!(error.to_string().contains("must be a non-empty relative path"));
        }
    }

    #[test]
    fn restore_rejects_uncontained_metadata_paths() {
        let result: RailResult<()> = (|| {
            let workspace = create_test_workspace();
            let manager = BackupManager::new(workspace.path());
            let backup_id = manager.create_backup(&[PathBuf::from("Cargo.toml")], BackupMetadata::new("test"), 10)?;
            let backup_dir = get_backup_dir(workspace.path(), &backup_id);
            let mut metadata = BackupMetadata::load(&backup_dir)?;
            metadata.files_modified = vec![PathBuf::from("../outside")];
            metadata.save(&backup_dir)?;

            let error = manager.restore_backup(&backup_id).unwrap_err();
            assert!(error.to_string().contains("must be a non-empty relative path"));
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn restore_rejects_uncontained_backup_ids() {
        let workspace = create_test_workspace();
        let manager = BackupManager::new(workspace.path());

        for backup_id in ["../outside", "/tmp/outside"] {
            let error = manager.restore_backup(backup_id).unwrap_err();
            assert!(error.to_string().contains("backup id"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn backup_and_restore_reject_symlink_targets() {
        let result: RailResult<()> = (|| {
            use std::os::unix::fs::symlink;

            let workspace = create_test_workspace();
            let manager = BackupManager::new(workspace.path());
            let outside = TempDir::new().unwrap();
            let outside_file = outside.path().join("outside");
            fs::write(&outside_file, "outside")?;

            fs::remove_file(workspace.path().join("Cargo.toml"))?;
            symlink(&outside_file, workspace.path().join("Cargo.toml"))?;
            let error = manager
                .create_backup(&[PathBuf::from("Cargo.toml")], BackupMetadata::new("test"), 10)
                .unwrap_err();
            assert!(error.to_string().contains("outside"));

            fs::remove_file(workspace.path().join("Cargo.toml"))?;
            fs::write(workspace.path().join("Cargo.toml"), "original")?;
            let backup_id = manager.create_backup(&[PathBuf::from("Cargo.toml")], BackupMetadata::new("test"), 10)?;
            fs::remove_file(workspace.path().join("Cargo.toml"))?;
            symlink(&outside_file, workspace.path().join("Cargo.toml"))?;

            let error = manager.restore_backup(&backup_id).unwrap_err();
            assert!(error.to_string().contains("outside"));
            assert_eq!(fs::read_to_string(&outside_file)?, "outside");
            Ok(())
        })();
        result.unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn backup_root_must_remain_in_workspace() {
        let result: RailResult<()> = (|| {
            use std::os::unix::fs::symlink;

            let workspace = create_test_workspace();
            let manager = BackupManager::new(workspace.path());
            let outside = TempDir::new().unwrap();
            fs::create_dir_all(workspace.path().join("target/cargo-rail"))?;
            symlink(outside.path(), &manager.backup_root)?;

            let error = manager
                .create_backup(&[PathBuf::from("Cargo.toml")], BackupMetadata::new("test"), 10)
                .unwrap_err();
            assert!(error.to_string().contains("outside"));
            Ok(())
        })();
        result.unwrap();
    }
}
