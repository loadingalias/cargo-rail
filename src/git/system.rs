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

/// Normalize a path from git output to a platform-native path.
///
/// On Windows, git (via MSYS2) outputs paths like `/c/Users/...` which need to be
/// converted to native Windows paths like `C:\Users\...` for proper path operations.
///
/// This avoids using `canonicalize()` which adds the `\\?\` extended-length prefix
/// that can cause compatibility issues with other tools.
fn normalize_git_path(path: &str) -> PathBuf {
  #[cfg(windows)]
  {
    // MSYS-style path: /c/Users/... -> C:\Users\...
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b'/' && bytes[1].is_ascii_alphabetic() {
      let drive = (bytes[1] as char).to_ascii_uppercase();
      let rest = &path[2..]; // includes leading /
      let windows_path = format!("{}:{}", drive, rest.replace('/', "\\"));
      return PathBuf::from(windows_path);
    }
  }

  // For non-MSYS paths or non-Windows, use the path directly.
  // PathBuf handles forward slashes correctly on all platforms.
  PathBuf::from(path)
}

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
    let worktree_root = normalize_git_path(stdout.trim());

    Ok(Self {
      repo_path: path.to_path_buf(),
      worktree_root,
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
  /// - Whitelists only essential variables (PATH, HOME, auth-related vars)
  /// - Adds safe configuration overrides
  pub(crate) fn git_cmd(&self) -> Command {
    let mut cmd = Command::new("git");

    // Set working directory
    cmd.arg("-C").arg(&self.repo_path);

    // Isolated environment (don't trust global config)
    cmd.env_clear();
    // Always preserve PATH for process execution.
    if let Some(path) = std::env::var_os("PATH") {
      cmd.env("PATH", path);
    }
    // Preserve common home directory variables for git config/credentials.
    for key in ["HOME", "USERPROFILE", "HOMEDRIVE", "HOMEPATH"] {
      if let Some(val) = std::env::var_os(key) {
        cmd.env(key, val);
      }
    }
    // Preserve common auth-related variables so fetch/push work with SSH agents and askpass.
    for key in [
      "SSH_AUTH_SOCK",
      "SSH_ASKPASS",
      "DISPLAY",
      "GIT_ASKPASS",
      "GIT_SSH",
      "GIT_SSH_COMMAND",
      "GIT_TERMINAL_PROMPT",
    ] {
      if let Some(val) = std::env::var_os(key) {
        cmd.env(key, val);
      }
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
mod tests {
  use super::*;
  use std::ffi::OsStr;
  use std::sync::{Mutex, OnceLock};

  static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

  fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
  }

  fn command_has_env_value(cmd: &Command, key: &str, value: &str) -> bool {
    cmd
      .get_envs()
      .any(|(k, v)| k == OsStr::new(key) && v == Some(OsStr::new(value)))
  }

  #[test]
  fn test_git_cmd_preserves_ssh_auth_sock_when_set() {
    let _guard = lock_env();
    let key = "SSH_AUTH_SOCK";
    let prev = std::env::var_os(key);

    unsafe {
      std::env::set_var(key, "cargo-rail-test-sock");
    }
    let git = SystemGit::open(Path::new(".")).unwrap();
    let cmd = git.git_cmd();
    assert!(command_has_env_value(&cmd, key, "cargo-rail-test-sock"));

    match prev {
      Some(v) => unsafe {
        std::env::set_var(key, v);
      },
      None => unsafe {
        std::env::remove_var(key);
      },
    }
  }

  #[test]
  fn test_git_cmd_preserves_git_ssh_command_when_set() {
    let _guard = lock_env();
    let key = "GIT_SSH_COMMAND";
    let prev = std::env::var_os(key);

    unsafe {
      std::env::set_var(key, "ssh -o BatchMode=yes");
    }
    let git = SystemGit::open(Path::new(".")).unwrap();
    let cmd = git.git_cmd();
    assert!(command_has_env_value(&cmd, key, "ssh -o BatchMode=yes"));

    match prev {
      Some(v) => unsafe {
        std::env::set_var(key, v);
      },
      None => unsafe {
        std::env::remove_var(key);
      },
    }
  }
}
