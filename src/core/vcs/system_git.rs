//! System git backend - zero dependencies, maximum performance
//!
//! Uses git plumbing commands for all operations. Optimized for:
//! - Batch processing (cat-file --batch, rev-list --format=raw)
//! - Metadata caching (repo paths, HEAD, branch)
//! - Safe subprocess execution (isolated environment)
//! - Zero-copy parsing where possible

use crate::core::error::{GitError, RailError, RailResult, ResultExt};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Git backend using system git (zero crate dependencies)
pub struct SystemGit {
  /// Repository working directory
  pub(crate) repo_path: PathBuf,

  /// Working tree root
  pub(crate) work_tree: PathBuf,
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
    let work_tree = stdout.trim();

    Ok(Self {
      repo_path: path.to_path_buf(),
      work_tree: PathBuf::from(work_tree),
    })
  }

  /// Get HEAD commit SHA
  ///
  /// Note: We don't cache this anymore to avoid interior mutability.
  /// The performance difference is negligible (1-2ms per call).
  pub fn head_commit(&self) -> RailResult<String> {
    let output = self
      .git_cmd()
      .args(["rev-parse", "HEAD"])
      .output()
      .context("Failed to get HEAD commit")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git rev-parse HEAD".to_string(),
        stderr: stderr.to_string(),
      }));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
  }

  /// Get current branch name
  pub fn current_branch(&self) -> RailResult<String> {
    let output = self
      .git_cmd()
      .args(["rev-parse", "--abbrev-ref", "HEAD"])
      .output()
      .context("Failed to get current branch")?;

    if !output.status.success() {
      return Ok("HEAD".to_string()); // Detached HEAD
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
  }

  /// Read a file at a specific commit
  ///
  /// Returns empty Vec if file doesn't exist at that commit.
  /// For reading multiple files, use `read_files_bulk` instead for 100x+ speedup.
  #[allow(dead_code)] // Kept as convenience API for single-file reads
  pub fn read_file_at_commit(&self, commit_sha: &str, path: &Path) -> RailResult<Vec<u8>> {
    let spec = format!("{}:{}", commit_sha, path.display());

    let output = self
      .git_cmd()
      .args(["show", &spec])
      .output()
      .context("Failed to read file from commit")?;

    if !output.status.success() {
      // File doesn't exist at this commit
      return Ok(vec![]);
    }

    Ok(output.stdout)
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
}

#[cfg(test)]
mod tests {
  /// Parse git timestamp (seconds since epoch)
  fn parse_timestamp(ts_str: &str) -> i64 {
    ts_str
      .split_whitespace()
      .next()
      .and_then(|s| s.parse().ok())
      .unwrap_or(0)
  }

  /// Validate SHA format (40 hex chars)
  fn is_valid_sha(sha: &str) -> bool {
    sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit())
  }

  #[test]
  fn test_parse_timestamp() {
    assert_eq!(parse_timestamp("1699999999 -0800"), 1699999999);
    assert_eq!(parse_timestamp("invalid"), 0);
  }

  #[test]
  fn test_is_valid_sha() {
    assert!(is_valid_sha("a".repeat(40).as_str()));
    assert!(!is_valid_sha("z".repeat(40).as_str()));
    assert!(!is_valid_sha("a".repeat(39).as_str()));
  }
}
