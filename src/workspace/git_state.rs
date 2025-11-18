//! Git state and operations wrapper
//!
//! Thin wrapper around SystemGit providing workspace-level git operations.
//! Built once at workspace context initialization, passed by reference.

use crate::error::RailResult;
use crate::git::SystemGit;
use std::path::{Path, PathBuf};

/// Git state for the workspace
///
/// Provides git operations scoped to the workspace repository.
/// This is built once and shared across all commands via WorkspaceContext.
#[derive(Clone)]
pub struct GitState {
  /// Underlying git backend
  git: SystemGit,

  /// Cached repository root (git working tree)
  repo_root: PathBuf,
}

impl GitState {
  /// Open git repository at the given path
  pub fn open(path: &Path) -> RailResult<Self> {
    let git = SystemGit::open(path)?;
    let repo_root = git.work_tree.clone();

    Ok(Self { git, repo_root })
  }

  /// Get repository root path
  pub fn repo_root(&self) -> &Path {
    &self.repo_root
  }

  /// Access underlying SystemGit for advanced operations
  ///
  /// Use this when you need direct access to SystemGit methods.
  pub fn git(&self) -> &SystemGit {
    &self.git
  }

  /// Get current HEAD commit SHA
  ///
  /// Part of git backbone - will be used for sync/split operations
  #[allow(dead_code)]
  pub fn head_commit(&self) -> RailResult<String> {
    self.git.head_commit()
  }

  /// Get current branch name
  ///
  /// Part of git backbone - will be used for git operations
  #[allow(dead_code)]
  pub fn current_branch(&self) -> RailResult<String> {
    self.git.current_branch()
  }
}
