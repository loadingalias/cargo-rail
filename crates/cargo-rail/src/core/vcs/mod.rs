#![allow(dead_code)]

pub mod git;

use anyhow::Result;
use std::path::{Path, PathBuf};

/// VCS abstraction trait for swappable version control backends
pub trait Vcs {
  /// Open a repository at the given path
  fn open(path: &Path) -> Result<Self>
  where
    Self: Sized;

  /// Get the repository root path
  fn root(&self) -> &Path;

  /// Get the current HEAD commit SHA
  fn head_commit(&self) -> Result<String>;

  /// Get commit history for a specific path
  fn commit_history(&self, path: &Path, limit: Option<usize>) -> Result<Vec<CommitInfo>>;

  /// Check if a path is tracked by the repository
  fn is_tracked(&self, path: &Path) -> Result<bool>;

  /// Get the list of files at a specific commit for a path
  fn list_files_at_commit(&self, commit_sha: &str, path: &Path) -> Result<Vec<PathBuf>>;

  /// Get file contents at a specific commit
  fn read_file_at_commit(&self, commit_sha: &str, path: &Path) -> Result<Vec<u8>>;
}

/// Information about a commit
#[derive(Debug, Clone)]
pub struct CommitInfo {
  pub sha: String,
  pub author: String,
  pub author_email: String,
  pub committer: String,
  pub committer_email: String,
  pub message: String,
  pub timestamp: i64,
  pub parent_shas: Vec<String>,
}

impl CommitInfo {
  /// Get the first line of the commit message
  pub fn summary(&self) -> &str {
    self.message.lines().next().unwrap_or("")
  }
}
