//! Auxiliary file handling for split repositories.
//!
//! Copies workspace-level config files (rust-toolchain.toml, rustfmt.toml, .cargo/config.toml)
//! and project files (README, LICENSE) into split repositories with appropriate fallback logic.

use crate::error::{RailResult, ResultExt};
use crate::split::SplitPathCapabilities;
use std::fs;
use std::path::{Path, PathBuf};

/// Handler for auxiliary files (rust-toolchain.toml, rustfmt.toml, .cargo/config.toml)
pub struct AuxiliaryFiles {
  files: Vec<AuxiliaryFile>,
}

#[derive(Debug, Clone)]
struct AuxiliaryFile {
  /// Relative path from workspace root
  source_path: PathBuf,
  /// Where to place it in split repo (relative to repo root)
  target_path: PathBuf,
}

/// Handler for project files (README, LICENSE) with crate-first, workspace-fallback logic
pub struct ProjectFiles {
  files: Vec<AuxiliaryFile>,
}

impl AuxiliaryFiles {
  /// Discover auxiliary files in workspace that should be copied to split repos
  pub fn discover(workspace_root: &Path) -> RailResult<Self> {
    let mut files = Vec::new();

    // Common auxiliary files to look for (workspace-level configs)
    let candidates = vec![
      ("rust-toolchain.toml", "rust-toolchain.toml"),
      ("rust-toolchain", "rust-toolchain"),
      ("rustfmt.toml", "rustfmt.toml"),
      (".rustfmt.toml", ".rustfmt.toml"),
      (".cargo/config.toml", ".cargo/config.toml"),
      (".cargo/config", ".cargo/config"),
      ("deny.toml", "deny.toml"),
      (".editorconfig", ".editorconfig"),
    ];

    for (source_rel, target_rel) in candidates {
      let source_path = workspace_root.join(source_rel);
      if source_path.exists() && source_path.is_file() {
        files.push(AuxiliaryFile {
          source_path,
          target_path: PathBuf::from(target_rel),
        });
      }
    }

    Ok(Self { files })
  }

  /// Copy discovered auxiliary files to split repo
  pub fn copy_to_split(&self, paths: &SplitPathCapabilities) -> RailResult<()> {
    if self.files.is_empty() {
      return Ok(());
    }

    for file in &self.files {
      let source_path = paths.authorize_source(&file.source_path)?;
      let target_path = paths.authorize_target(&file.target_path)?;

      // Create parent directories if needed
      if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)
          .with_context(|| format!("Failed to create directory for {}", target_path.display()))?;
      }

      // Copy the file
      fs::copy(&source_path, &target_path)
        .with_context(|| format!("Failed to copy {} to {}", source_path.display(), target_path.display()))?;
    }

    Ok(())
  }

  /// Get count of discovered files
  pub fn count(&self) -> usize {
    self.files.len()
  }

  /// Check if any files were discovered
  pub fn is_empty(&self) -> bool {
    self.files.is_empty()
  }
}

impl ProjectFiles {
  /// Discover project files with crate-first, workspace-fallback logic
  pub fn discover(workspace_root: &Path, crate_paths: &[PathBuf]) -> RailResult<Self> {
    let mut files = Vec::new();

    // Project files to look for (check crate dir first, then workspace root)
    let candidates = vec!["README.md", "LICENSE", "LICENSE-MIT", "LICENSE-APACHE"];

    for filename in candidates {
      // Check each crate directory first (in config order), then workspace root.
      let crate_file = crate_paths
        .iter()
        .map(|crate_path| workspace_root.join(crate_path).join(filename))
        .find(|path| path.exists() && path.is_file());
      let workspace_file = workspace_root.join(filename);

      let source_path = if let Some(crate_file) = crate_file {
        crate_file
      } else if workspace_file.exists() && workspace_file.is_file() {
        workspace_file
      } else {
        continue; // File doesn't exist in either location
      };

      files.push(AuxiliaryFile {
        source_path,
        target_path: PathBuf::from(filename),
      });
    }

    Ok(Self { files })
  }

  /// Copy discovered project files to split repo
  pub fn copy_to_split(&self, paths: &SplitPathCapabilities) -> RailResult<()> {
    if self.files.is_empty() {
      return Ok(());
    }

    for file in &self.files {
      let source_path = paths.authorize_source(&file.source_path)?;
      let target_path = paths.authorize_target(&file.target_path)?;

      if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)
          .with_context(|| format!("Failed to create directory for {}", target_path.display()))?;
      }

      // Copy the file
      fs::copy(&source_path, &target_path)
        .with_context(|| format!("Failed to copy {} to {}", source_path.display(), target_path.display()))?;
    }

    Ok(())
  }

  /// Get count of discovered files
  pub fn count(&self) -> usize {
    self.files.len()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;
  use tempfile::TempDir;

  #[test]
  fn test_discover_finds_rust_toolchain() {
    let temp = TempDir::new().unwrap();
    let workspace_root = temp.path();

    // Create a rust-toolchain.toml
    fs::write(
      workspace_root.join("rust-toolchain.toml"),
      "[toolchain]\nchannel = \"stable\"\n",
    )
    .unwrap();

    let aux_files = AuxiliaryFiles::discover(workspace_root).unwrap();
    assert_eq!(aux_files.count(), 1);
    assert!(!aux_files.is_empty());
  }

  #[test]
  fn test_discover_finds_multiple_files() {
    let temp = TempDir::new().unwrap();
    let workspace_root = temp.path();

    // Create multiple auxiliary files
    fs::write(workspace_root.join("rust-toolchain.toml"), "channel = \"stable\"").unwrap();
    fs::write(workspace_root.join("rustfmt.toml"), "max_width = 100").unwrap();
    fs::create_dir_all(workspace_root.join(".cargo")).unwrap();
    fs::write(workspace_root.join(".cargo/config.toml"), "[build]\nrustflags = []").unwrap();

    let aux_files = AuxiliaryFiles::discover(workspace_root).unwrap();
    assert_eq!(aux_files.count(), 3);
  }

  #[test]
  fn test_copy_to_split() {
    let temp = TempDir::new().unwrap();
    let workspace_root = temp.path().join("workspace");
    let split_root = temp.path().join("split");

    fs::create_dir(&workspace_root).unwrap();
    fs::create_dir(&split_root).unwrap();

    // Create source file
    fs::write(workspace_root.join("rust-toolchain.toml"), "channel = \"stable\"").unwrap();

    let aux_files = AuxiliaryFiles::discover(&workspace_root).unwrap();
    let paths =
      SplitPathCapabilities::new(&workspace_root, &workspace_root, &[PathBuf::from(".")], &split_root).unwrap();
    aux_files.copy_to_split(&paths).unwrap();

    // Verify file was copied
    assert!(split_root.join("rust-toolchain.toml").exists());
    let content = fs::read_to_string(split_root.join("rust-toolchain.toml")).unwrap();
    assert_eq!(content, "channel = \"stable\"");
  }

  #[test]
  fn test_copy_creates_directories() {
    let temp = TempDir::new().unwrap();
    let workspace_root = temp.path().join("workspace");
    let split_root = temp.path().join("split");

    fs::create_dir(&workspace_root).unwrap();
    fs::create_dir(&split_root).unwrap();

    // Create .cargo/config.toml
    fs::create_dir_all(workspace_root.join(".cargo")).unwrap();
    fs::write(workspace_root.join(".cargo/config.toml"), "[build]\nrustflags = []").unwrap();

    let aux_files = AuxiliaryFiles::discover(&workspace_root).unwrap();
    let paths =
      SplitPathCapabilities::new(&workspace_root, &workspace_root, &[PathBuf::from(".")], &split_root).unwrap();
    aux_files.copy_to_split(&paths).unwrap();

    // Verify directory and file were created
    assert!(split_root.join(".cargo").exists());
    assert!(split_root.join(".cargo/config.toml").exists());
  }
}
