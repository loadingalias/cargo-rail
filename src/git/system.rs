//! System git backend - zero dependencies, maximum performance
//!
//! Uses git plumbing commands for all operations. Optimized for:
//! - Batch processing (cat-file --batch, rev-list --format=raw)
//! - Metadata caching (repo paths, HEAD, branch)
//! - Safe subprocess execution (isolated environment)
//! - Zero-copy parsing where possible

use crate::error::{GitError, RailError, RailResult, ResultExt};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Information about a git commit
#[derive(Debug, Clone)]
pub struct CommitInfo {
  /// Full SHA-1 hash of the commit
  pub sha: String,
  /// Author name
  pub author: String,
  /// Author email address
  pub author_email: String,
  /// Committer name
  pub committer: String,
  /// Committer email address
  pub committer_email: String,
  /// Commit message
  pub message: String,
  /// Commit timestamp (seconds since Unix epoch)
  pub timestamp: i64,
  /// Parent commit SHAs
  pub parent_shas: Vec<String>,
}

/// Git backend using system git (zero crate dependencies)
#[derive(Clone)]
pub struct SystemGit {
  /// Repository working directory
  pub(crate) repo_path: PathBuf,

  /// Root directory of the git working tree
  pub(crate) worktree_root: PathBuf,
}

impl SystemGit {
  /// Open a git repository
  ///
  /// This performs ONE subprocess call to get the repository metadata.
  pub fn open(path: &Path) -> RailResult<Self> {
    // Get repo metadata in one subprocess call
    let output = Command::new("git")
      .arg("-C")
      .arg(path)
      .args(["rev-parse", "--show-toplevel"])
      .output()
      .context("Failed to execute git rev-parse")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      if stderr.contains("not a git repository") {
        return Err(RailError::Git(GitError::RepoNotFound {
          path: path.to_path_buf(),
        }));
      }
      return Err(RailError::message(format!("Failed to open git repository: {}", stderr)));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let worktree_root = stdout.trim();

    Ok(Self {
      repo_path: path.to_path_buf(),
      worktree_root: PathBuf::from(worktree_root),
    })
  }

  /// Get HEAD commit SHA
  ///
  /// Note: We don't cache this anymore to avoid interior mutability.
  /// The performance difference is negligible (1-2ms per call).
  pub fn head_commit(&self) -> RailResult<String> {
    self.run_git_stdout(&["rev-parse", "HEAD"])
  }

  /// Get current branch name
  pub fn current_branch(&self) -> RailResult<String> {
    // Try to get branch name, fallback to "HEAD" if detached
    self
      .run_git_stdout(&["rev-parse", "--abbrev-ref", "HEAD"])
      .or(Ok("HEAD".to_string()))
  }

  /// Check if HEAD is detached (not on any branch)
  pub fn is_detached_head(&self) -> RailResult<bool> {
    let branch = self.run_git_stdout(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    Ok(branch == "HEAD")
  }

  /// Get the default branch name (main/master) via remote HEAD
  ///
  /// Tries to detect the default branch by:
  /// 1. Checking `refs/remotes/origin/HEAD` symbolic ref
  /// 2. Falling back to checking for common branch names (main, master)
  ///
  /// Returns `None` if no default branch can be determined.
  pub fn default_branch(&self) -> RailResult<Option<String>> {
    // Try: git symbolic-ref refs/remotes/origin/HEAD
    if let Ok(output) = self.run_git_stdout(&["symbolic-ref", "refs/remotes/origin/HEAD"]) {
      // Output: "refs/remotes/origin/main" -> extract "main"
      if let Some(branch) = output.strip_prefix("refs/remotes/origin/") {
        return Ok(Some(branch.to_string()));
      }
    }

    // Fallback: check for common defaults
    for name in &["main", "master"] {
      if self.run_git_check(&["rev-parse", "--verify", &format!("refs/heads/{}", name)]) {
        return Ok(Some((*name).to_string()));
      }
    }

    Ok(None)
  }

  /// Check if the worktree has uncommitted changes
  ///
  /// Returns `true` if there are staged or unstaged changes, including untracked files.
  /// This is useful for safety checks before destructive operations.
  pub fn is_dirty(&self) -> RailResult<bool> {
    let output = self.run_git_stdout(&["status", "--porcelain"])?;
    Ok(!output.is_empty())
  }

  /// Get list of dirty files in the worktree
  ///
  /// Returns the files with their status prefixes (e.g., " M file.txt", "?? new.txt").
  /// Useful for displaying what's dirty when refusing to run on a dirty worktree.
  pub fn dirty_files(&self) -> RailResult<Vec<String>> {
    let output = self.run_git_stdout(&["status", "--porcelain"])?;
    Ok(output.lines().map(|s| s.to_string()).collect())
  }

  /// Create a safe git command with isolated environment
  ///
  /// - Sets working directory to repo path
  /// - Clears environment variables
  /// - Whitelists only PATH and HOME
  /// - Adds safe configuration overrides
  pub(crate) fn git_cmd(&self) -> Command {
    let mut cmd = Command::new("git");

    // Set working directory
    cmd.arg("-C").arg(&self.repo_path);

    // Isolated environment (don't trust global config)
    cmd.env_clear();
    if let Ok(path) = std::env::var("PATH") {
      cmd.env("PATH", path);
    }
    if let Ok(home) = std::env::var("HOME") {
      cmd.env("HOME", home);
    }

    // Force safe behavior (override user config)
    cmd.arg("-c").arg("protocol.version=2");
    cmd.arg("-c").arg("advice.detachedHead=false");
    cmd.arg("-c").arg("core.quotePath=false"); // Don't escape non-ASCII

    cmd
  }

  /// Run a git command and return the output or error
  ///
  /// This helper eliminates 200+ lines of boilerplate by handling:
  /// - Command execution
  /// - Success checking
  /// - Error formatting
  ///
  /// # Example
  /// ```ignore
  /// let output = git.run_git(&["status", "--short"])?;
  /// ```
  pub(crate) fn run_git(&self, args: &[&str]) -> RailResult<std::process::Output> {
    let mut cmd = self.git_cmd();
    cmd.args(args);

    let output = cmd
      .output()
      .with_context(|| format!("Failed to execute git {}", args.join(" ")))?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::Git(GitError::CommandFailed {
        command: format!("git {}", args.join(" ")),
        stderr: stderr.to_string(),
      }));
    }

    Ok(output)
  }

  /// Run a git command and return stdout as a String
  ///
  /// Convenience wrapper around `run_git` that returns trimmed stdout.
  pub(crate) fn run_git_stdout(&self, args: &[&str]) -> RailResult<String> {
    let output = self.run_git(args)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
  }

  /// Run a git command, returning true if successful, false otherwise
  ///
  /// Used for operations that should silently fail (e.g., checking if remote exists).
  pub(crate) fn run_git_check(&self, args: &[&str]) -> bool {
    let mut cmd = self.git_cmd();
    cmd.args(args);

    if let Ok(output) = cmd.output() {
      output.status.success()
    } else {
      false
    }
  }

  /// Run a git command with a custom error builder
  ///
  /// This allows using specific GitError variants while still getting
  /// the boilerplate reduction benefits.
  ///
  /// # Example
  /// ```ignore
  /// git.run_git_with_error(&["push", "-u", "origin", "main"], |stderr| {
  ///   RailError::Git(GitError::PushFailed {
  ///     remote: "origin".to_string(),
  ///     branch: "main".to_string(),
  ///     reason: stderr.to_string(),
  ///   })
  /// })?;
  /// ```
  pub(crate) fn run_git_with_error<F>(&self, args: &[&str], error_fn: F) -> RailResult<std::process::Output>
  where
    F: FnOnce(&str) -> RailError,
  {
    let mut cmd = self.git_cmd();
    cmd.args(args);

    let output = cmd
      .output()
      .with_context(|| format!("Failed to execute git {}", args.join(" ")))?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(error_fn(&stderr));
    }

    Ok(output)
  }
}

#[cfg(test)]
mod tests {}
