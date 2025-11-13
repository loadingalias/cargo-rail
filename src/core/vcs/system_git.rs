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

    /// Git directory (.git or bare repo)
    pub(crate) git_dir: PathBuf,

    /// Working tree root
    pub(crate) work_tree: PathBuf,

    /// Cached metadata
    pub(crate) cache: GitCache,
}

/// Cached git metadata (invalidated on mutation operations)
#[derive(Default)]
struct GitCache {
    /// HEAD commit SHA (cached)
    head: Option<String>,

    /// Current branch name (cached)
    current_branch: Option<String>,
}

impl GitCache {
    /// Invalidate all caches (call after mutations like commit, checkout)
    fn invalidate(&mut self) {
        self.head = None;
        self.current_branch = None;
    }
}

impl SystemGit {
    /// Open a git repository
    ///
    /// This performs ONE subprocess call to get all repo metadata.
    pub fn open(path: &Path) -> RailResult<Self> {
        // Get all repo metadata in one subprocess call
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "--git-dir", "--show-toplevel"])
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
        let mut lines = stdout.lines();

        let git_dir = lines
            .next()
            .ok_or_else(|| RailError::message("git rev-parse returned no output"))?;
        let work_tree = lines
            .next()
            .ok_or_else(|| RailError::message("git rev-parse missing work tree"))?;

        Ok(Self {
            repo_path: path.to_path_buf(),
            git_dir: PathBuf::from(git_dir),
            work_tree: PathBuf::from(work_tree),
            cache: GitCache::default(),
        })
    }

    /// Get repository root (working tree)
    pub fn root(&self) -> &Path {
        &self.work_tree
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

    /// Check if a file is tracked in the index
    pub fn is_tracked(&self, path: &Path) -> RailResult<bool> {
        let relative_path = path.strip_prefix(&self.work_tree).unwrap_or(path);

        let output = self
            .git_cmd()
            .args(["ls-files", "--", relative_path.to_str().unwrap_or("")])
            .output()
            .context("Failed to check if file is tracked")?;

        // If output is non-empty, file is tracked
        Ok(!output.stdout.is_empty())
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

/// Parse git timestamp (seconds since epoch)
fn parse_timestamp(ts_str: &str) -> i64 {
    ts_str.split_whitespace().next().and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// Validate SHA format (40 hex chars)
fn is_valid_sha(sha: &str) -> bool {
    sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

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
