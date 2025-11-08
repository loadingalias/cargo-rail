#![allow(dead_code)]

use anyhow::{Context, Result};
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
  pub fn discover(workspace_root: &Path) -> Result<Self> {
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
  pub fn copy_to_split(&self, workspace_root: &Path, target_repo_root: &Path) -> Result<()> {
    for file in &self.files {
      let target_path = target_repo_root.join(&file.target_path);

      // Create parent directories if needed
      if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)
          .with_context(|| format!("Failed to create directory for {}", target_path.display()))?;
      }

      // Copy the file
      fs::copy(&file.source_path, &target_path).with_context(|| {
        format!(
          "Failed to copy {} to {}",
          file.source_path.display(),
          target_path.display()
        )
      })?;

      println!(
        "  📄 Copied {} to {}",
        file
          .source_path
          .strip_prefix(workspace_root)
          .unwrap_or(&file.source_path)
          .display(),
        file.target_path.display()
      );
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
  pub fn discover(workspace_root: &Path, crate_path: &Path) -> Result<Self> {
    let mut files = Vec::new();

    // Project files to look for (check crate dir first, then workspace root)
    let candidates = vec![
      "README.md",
      "LICENSE",
      "LICENSE-MIT",
      "LICENSE-APACHE",
    ];

    for filename in candidates {
      // Check crate directory first
      let crate_file = crate_path.join(filename);
      let workspace_file = workspace_root.join(filename);

      let source_path = if crate_file.exists() && crate_file.is_file() {
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
  pub fn copy_to_split(&self, workspace_root: &Path, target_repo_root: &Path) -> Result<()> {
    for file in &self.files {
      let target_path = target_repo_root.join(&file.target_path);

      // Copy the file
      fs::copy(&file.source_path, &target_path).with_context(|| {
        format!(
          "Failed to copy {} to {}",
          file.source_path.display(),
          target_path.display()
        )
      })?;

      println!(
        "  📄 Copied {} to {}",
        file
          .source_path
          .strip_prefix(workspace_root)
          .unwrap_or(&file.source_path)
          .display(),
        file.target_path.display()
      );
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
    aux_files.copy_to_split(&workspace_root, &split_root).unwrap();

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
    aux_files.copy_to_split(&workspace_root, &split_root).unwrap();

    // Verify directory and file were created
    assert!(split_root.join(".cargo").exists());
    assert!(split_root.join(".cargo/config.toml").exists());
  }
}
