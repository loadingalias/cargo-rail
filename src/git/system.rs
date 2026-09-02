//! System git backend - zero dependencies, maximum performance
//!
//! Uses git plumbing commands for all operations. Optimized for:
//! - Batch processing (cat-file --batch, rev-list --format=raw)
//! - Metadata caching (repo paths, HEAD, branch)
//! - Safe subprocess execution (inherited caller environment with bounded repository state)
//! - Zero-copy parsing where possible

use super::{git_cmd_for_path, git_command, sanitize_git_environment};
use crate::error::{GitError, RailError, RailResult, ResultExt, git_command_diagnostics};
use crate::utils;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};

const OPEN_ROOT_CACHE_CAPACITY: usize = 64;

fn open_root_cache() -> &'static Mutex<std::collections::VecDeque<PathBuf>> {
    static CACHE: OnceLock<Mutex<std::collections::VecDeque<PathBuf>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(std::collections::VecDeque::new()))
}

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
            if let Ok(rest) = std::str::from_utf8(&bytes[2..]) {
                let windows_path = format!("{}:{}", drive, rest.replace('/', "\\"));
                return PathBuf::from(windows_path);
            }
        }
    }

    // For non-MSYS paths or non-Windows, use the path directly.
    // PathBuf handles forward slashes correctly on all platforms.
    PathBuf::from(path)
}

fn parse_nul_paths(bytes: &[u8]) -> Vec<PathBuf> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
        .map(|raw| PathBuf::from(String::from_utf8_lossy(raw).into_owned()))
        .collect()
}

fn path_from_git_bytes(raw: &[u8]) -> RailResult<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        Ok(PathBuf::from(std::ffi::OsString::from_vec(raw.to_vec())))
    }
    #[cfg(not(unix))]
    {
        Ok(PathBuf::from(String::from_utf8(raw.to_vec())?))
    }
}

fn field_after_spaces(record: &[u8], spaces: usize) -> Option<&[u8]> {
    let mut remaining = record;
    for _ in 0..spaces {
        let separator = remaining.iter().position(|byte| *byte == b' ')?;
        remaining = &remaining[separator + 1..];
    }
    Some(remaining)
}

fn parse_obstructing_status_paths(bytes: &[u8]) -> RailResult<Vec<PathBuf>> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(|record| {
            let path = match record.first().copied() {
                Some(b'1') => field_after_spaces(record, 8),
                Some(b'u') => field_after_spaces(record, 10),
                Some(b'?') | Some(b'!') if record.get(1) == Some(&b' ') => record.get(2..),
                Some(b'2') => {
                    return Err(RailError::message(
                        "git status reported a rename despite exact no-rename capture",
                    ));
                }
                _ => None,
            }
            .filter(|path| !path.is_empty())
            .ok_or_else(|| RailError::message("git status returned an invalid porcelain-v2 record"))?;
            path_from_git_bytes(path)
        })
        .collect()
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
    /// Author time-zone offset as Git renders it (`+HHMM` or `-HHMM`).
    pub author_timezone: String,
    /// Committer timestamp (seconds since Unix epoch).
    pub committer_timestamp: i64,
    /// Committer time-zone offset as Git renders it (`+HHMM` or `-HHMM`).
    pub committer_timezone: String,
    /// Parent commit SHAs
    pub parent_shas: Vec<String>,
}

/// Complete author and committer identity for a synthesized commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitMetadata {
    /// Author name.
    pub author: String,
    /// Author email address.
    pub author_email: String,
    /// Author timestamp.
    pub author_timestamp: i64,
    /// Author time-zone offset.
    pub author_timezone: String,
    /// Committer name.
    pub committer: String,
    /// Committer email address.
    pub committer_email: String,
    /// Committer timestamp.
    pub committer_timestamp: i64,
    /// Committer time-zone offset.
    pub committer_timezone: String,
}

impl CommitInfo {
    /// Copy the complete commit identity for deterministic synthesis.
    pub fn metadata(&self) -> CommitMetadata {
        CommitMetadata {
            author: self.author.clone(),
            author_email: self.author_email.clone(),
            author_timestamp: self.timestamp,
            author_timezone: self.author_timezone.clone(),
            committer: self.committer.clone(),
            committer_email: self.committer_email.clone(),
            committer_timestamp: self.committer_timestamp,
            committer_timezone: self.committer_timezone.clone(),
        }
    }
}

/// Git backend using system git (zero crate dependencies)
#[derive(Debug, Clone)]
pub struct SystemGit {
    /// Repository working directory
    pub(crate) repo_path: PathBuf,

    /// Root directory of the git working tree
    pub(crate) worktree_root: PathBuf,

    /// Git object format is immutable for the lifetime of a repository.
    object_format: std::sync::Arc<OnceLock<String>>,
}

impl SystemGit {
    /// Open a git repository.
    ///
    /// Performs ONE subprocess call to get the repository metadata.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::RepoNotFound`] if `path` is not inside a git repository.
    pub fn open(path: &Path) -> RailResult<Self> {
        let repo_path = utils::canonicalize_existing(path).unwrap_or_else(|_| path.to_path_buf());
        if let Ok(mut cache) = open_root_cache().lock()
            && let Some(index) = cache.iter().position(|root| root == &repo_path)
        {
            if std::fs::symlink_metadata(repo_path.join(".git")).is_ok() {
                let worktree_root = cache.remove(index).expect("cached root index");
                cache.push_back(worktree_root.clone());
                return Ok(Self {
                    repo_path,
                    worktree_root,
                    object_format: std::sync::Arc::new(OnceLock::new()),
                });
            }
            cache.remove(index);
        }
        // Get repo metadata in one subprocess call
        let output = git_cmd_for_path(&repo_path)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .context("Failed to execute git rev-parse")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("not a git repository") {
                return Err(RailError::Git(GitError::RepoNotFound { path: repo_path }));
            }
            return Err(RailError::message(format!("Failed to open git repository: {}", stderr)));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let reported_root = normalize_git_path(stdout.trim());
        let worktree_root = utils::canonicalize_existing(&reported_root).unwrap_or(reported_root);

        let git = Self {
            repo_path,
            worktree_root,
            object_format: std::sync::Arc::new(OnceLock::new()),
        };
        // Only exact roots are cached. A nested input can gain its own `.git`
        // directory later, so caching its parent discovery would be stale.
        // The root marker is checked on every hit; repository/ref identity is
        // never cached and remains owned by each operation's drift boundary.
        if git.repo_path == git.worktree_root
            && std::fs::symlink_metadata(git.worktree_root.join(".git")).is_ok()
            && let Ok(mut cache) = open_root_cache().lock()
        {
            if let Some(existing) = cache.iter().position(|root| root == &git.worktree_root) {
                cache.remove(existing);
            }
            if cache.len() == OPEN_ROOT_CACHE_CAPACITY {
                cache.pop_front();
            }
            cache.push_back(git.worktree_root.clone());
        }
        Ok(git)
    }

    /// Get HEAD commit SHA.
    pub fn head_commit(&self) -> RailResult<String> {
        self.run_git_stdout(&["rev-parse", "HEAD"])
    }

    /// Return the repository's immutable object format, shared by clones of
    /// this already-open backend.
    pub(crate) fn object_format(&self) -> RailResult<String> {
        if let Some(format) = self.object_format.get() {
            return Ok(format.clone());
        }
        let format = self.run_git_stdout(&["rev-parse", "--show-object-format"])?;
        drop(self.object_format.set(format.clone()));
        Ok(format)
    }

    /// Get current branch name
    pub fn current_branch(&self) -> RailResult<String> {
        let symbolic = self.run_git(&["symbolic-ref", "--short", "-q", "HEAD"]);
        match symbolic {
            Ok(output) => Ok(String::from_utf8(output.stdout)?.trim().to_string()),
            Err(_) => self.run_git_stdout(&["rev-parse", "--abbrev-ref", "HEAD"]),
        }
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
    /// Lists files with status prefixes (e.g., " M file.txt", "?? new.txt").
    /// Useful for displaying what's dirty when refusing to run on a dirty worktree.
    pub fn dirty_files(&self) -> RailResult<Vec<String>> {
        let output = self.run_git_stdout(&["status", "--porcelain"])?;
        Ok(output.lines().map(|s| s.to_string()).collect())
    }

    /// Return every tracked or untracked path whose worktree content differs from `HEAD`.
    ///
    /// Paths are repository-relative and sorted. Ignored files are intentionally excluded:
    /// they are outside Git's mutation boundary unless a command declares them explicitly.
    pub fn changed_paths(&self) -> RailResult<Vec<PathBuf>> {
        let mut paths = if self.head_commit().is_ok() {
            let staged = self.run_git_read_only(&["diff", "--cached", "--name-only", "-z", "HEAD"])?;
            let unstaged = self.run_git_read_only(&["diff-files", "--name-only", "-z"])?;
            let mut paths = parse_nul_paths(&staged.stdout);
            paths.extend(parse_nul_paths(&unstaged.stdout));
            paths
        } else {
            let staged = self.run_git_read_only(&["ls-files", "--cached", "-z"])?;
            parse_nul_paths(&staged.stdout)
        };
        let untracked = self.run_git_read_only(&["ls-files", "--others", "--exclude-standard", "-z"])?;
        paths.extend(parse_nul_paths(&untracked.stdout));
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    /// Return every tracked, untracked, or ignored worktree path that could be
    /// overwritten by a target history materialization. Git administrative
    /// storage is excluded by Git itself. One porcelain-status stream owns
    /// ordinary staged, unstaged, untracked, and ignored state. A second index
    /// stream independently inspects only assume-unchanged/skip-worktree paths,
    /// which status intentionally suppresses.
    pub(crate) fn obstructing_worktree_paths(&self) -> RailResult<Vec<PathBuf>> {
        let status = self.run_git_read_only(&[
            "status",
            "--porcelain=v2",
            "-z",
            "--no-renames",
            "--untracked-files=all",
            "--ignored=matching",
        ])?;
        let mut paths = parse_obstructing_status_paths(&status.stdout)?;
        let index = self.run_git_read_only(&["ls-files", "-v", "--stage", "-z"])?;
        for record in index
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
                return Err(RailError::message("git ls-files returned an invalid staged entry"));
            };
            let flag = *record
                .first()
                .ok_or_else(|| RailError::message("git ls-files returned an index entry without a state flag"))?;
            if record.get(1) != Some(&b' ') {
                return Err(RailError::message("git ls-files returned an invalid index state flag"));
            }
            let header = std::str::from_utf8(&record[2..tab])?;
            let mut fields = header.split_whitespace();
            let mode = fields.next().unwrap_or_default();
            let object_id = fields.next().unwrap_or_default();
            let stage = fields.next().unwrap_or_default();
            let path = path_from_git_bytes(&record[tab + 1..])?;
            if stage != "0" {
                paths.push(path);
                continue;
            }
            if flag == b'H' {
                continue;
            }
            let absolute = self.worktree_root.join(&path);
            let metadata = match std::fs::symlink_metadata(&absolute) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    paths.push(path);
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let actual_id = match mode {
                "100644" | "100755" if metadata.file_type().is_file() => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt as _;

                        let executable = metadata.permissions().mode() & 0o111 != 0;
                        if (mode == "100755") != executable {
                            paths.push(path.clone());
                            continue;
                        }
                    }
                    let bytes = std::fs::read(&absolute)?;
                    let git_path = path.to_string_lossy().replace('\\', "/");
                    self.hash_path_bytes(&git_path, &bytes)?
                }
                "120000" if metadata.file_type().is_symlink() => {
                    let target = std::fs::read_link(&absolute)?;
                    #[cfg(unix)]
                    let bytes = {
                        use std::os::unix::ffi::OsStrExt as _;
                        target.as_os_str().as_bytes().to_vec()
                    };
                    #[cfg(not(unix))]
                    let bytes = target.to_string_lossy().as_bytes().to_vec();
                    self.hash_bytes(&bytes)?
                }
                "160000" if metadata.file_type().is_dir() => continue,
                _ => {
                    paths.push(path);
                    continue;
                }
            };
            if actual_id != object_id {
                paths.push(path);
            }
        }
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    /// Hash bytes with the repository's configured Git object format.
    pub(crate) fn hash_bytes(&self, bytes: &[u8]) -> RailResult<String> {
        self.hash_bytes_with_path(None, bytes)
    }

    /// Hash worktree bytes after applying clean filters for a worktree-relative `path`.
    pub(crate) fn hash_path_bytes(&self, path: &str, bytes: &[u8]) -> RailResult<String> {
        self.hash_bytes_with_path(Some(path), bytes)
    }

    fn hash_bytes_with_path(&self, path: Option<&str>, bytes: &[u8]) -> RailResult<String> {
        use std::io::Write as _;

        crate::instrumentation::record_hash(bytes.len());
        let mut cmd = path.map_or_else(|| self.git_cmd(), |_| git_cmd_for_path(&self.worktree_root));
        cmd.arg("hash-object");
        let path_argument = path.map(|path| format!("--path={path}"));
        if let Some(path_argument) = &path_argument {
            cmd.arg(path_argument);
        }
        cmd.arg("--stdin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|error| RailError::message(format!("failed to start git hash-object: {}", error)))?;
        child
            .stdin
            .take()
            .ok_or_else(|| RailError::message("git hash-object stdin was unavailable"))?
            .write_all(bytes)
            .map_err(|error| RailError::message(format!("failed to write git hash input: {}", error)))?;
        let output = child
            .wait_with_output()
            .map_err(|error| RailError::message(format!("failed to read git hash-object output: {}", error)))?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: path.map_or_else(
                    || "git hash-object --stdin".to_string(),
                    |path| format!("git hash-object --path={path} --stdin"),
                ),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Create a Git command bounded to this repository.
    pub(crate) fn git_cmd(&self) -> Command {
        git_cmd_for_path(&self.repo_path)
    }

    /// Run a git command and return the output or error
    ///
    /// This helper eliminates 200+ lines of boilerplate by handling:
    /// - Command execution
    /// - Success checking
    /// - Error formatting
    ///
    /// Example: `git.run_git(&["status", "--short"])?`
    pub(crate) fn run_git(&self, args: &[&str]) -> RailResult<std::process::Output> {
        self.run_git_in(&self.repo_path, args)
    }

    /// Run a path-reporting Git command from the repository worktree root.
    pub(crate) fn run_git_at_worktree_root(&self, args: &[&str]) -> RailResult<std::process::Output> {
        self.run_git_in(&self.worktree_root, args)
    }

    fn run_git_read_only(&self, args: &[&str]) -> RailResult<std::process::Output> {
        let mut command = git_cmd_for_path(&self.repo_path);
        command
            .env("GIT_OPTIONAL_LOCKS", "0")
            .arg("--no-optional-locks")
            .args(args);
        let output = command
            .output()
            .with_context(|| format!("Failed to execute read-only git {}", args.join(" ")))?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: format!("git {}", args.join(" ")),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        Ok(output)
    }

    fn run_git_in(&self, path: &Path, args: &[&str]) -> RailResult<std::process::Output> {
        let mut cmd = git_cmd_for_path(path);
        cmd.args(args);

        let output = cmd
            .output()
            .with_context(|| format!("Failed to execute git {}", args.join(" ")))?;

        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: format!("git {}", args.join(" ")),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }

        Ok(output)
    }

    /// Run a user-facing Git command while preserving its output.
    ///
    /// Human output is streamed as it arrives and retained for errors. JSON and
    /// quiet modes retain the same structured capture without writing raw child
    /// output into the command's output stream.
    pub(crate) fn run_git_observable(&self, args: &[&str]) -> RailResult<Output> {
        self.run_git_observable_with_env(args, &[])
    }

    /// Run a user-facing Git command with operation-specific hook context.
    pub(crate) fn run_git_observable_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> RailResult<Output> {
        let mut cmd = self.git_cmd();
        cmd.args(args);
        for (key, value) in env {
            cmd.env(key, value);
        }

        let output =
            observable_output(&mut cmd).with_context(|| format!("Failed to execute git {}", args.join(" ")))?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: format!("git {}", args.join(" ")),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
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

    /// Check if a tag exists
    pub fn tag_exists(&self, tag_name: &str) -> RailResult<bool> {
        let ref_name = format!("refs/tags/{}", tag_name);
        Ok(self.run_git_check(&["rev-parse", "-q", "--verify", &ref_name]))
    }

    /// Get a git config value
    ///
    /// Returns `Ok(Some(value))` if the config key exists,
    /// `Ok(None)` if it doesn't exist, or `Err` for other failures.
    pub fn get_config(&self, key: &str) -> RailResult<Option<String>> {
        let mut cmd = self.git_cmd();
        cmd.args(["config", "--get", key]);

        match cmd.output() {
            Ok(output) if output.status.success() => {
                Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_string()))
            }
            Ok(output) if output.status.code() == Some(1) => {
                // Exit code 1 means key not found (not an error)
                Ok(None)
            }
            Ok(output) => Err(RailError::Git(GitError::CommandFailed {
                command: format!("git config --get {}", key),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            })),
            Err(e) => Err(RailError::message(format!("Failed to get git config {}: {}", key, e))),
        }
    }

    /// Set a git config value
    pub fn set_config(&self, key: &str, value: &str) -> RailResult<()> {
        self.run_git(&["config", key, value])?;
        Ok(())
    }

    /// Stage all changes (git add -A)
    pub fn stage_all(&self) -> RailResult<()> {
        self.run_git(&["add", "-A"])?;
        Ok(())
    }

    /// Stage exactly the supplied repository paths, including deletions.
    pub fn stage_paths(&self, paths: &[PathBuf]) -> RailResult<()> {
        if paths.is_empty() {
            return Ok(());
        }

        let mut cmd = self.git_cmd();
        cmd.args(["add", "-A", "--"]);
        for path in paths {
            cmd.arg(self.normalize_repo_path(path)?);
        }
        let output = cmd
            .output()
            .map_err(|e| RailError::message(format!("failed to stage planned paths: {}", e)))?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git add -A -- <planned-paths>".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        Ok(())
    }

    fn normalize_repo_path(&self, path: &Path) -> RailResult<PathBuf> {
        if !path.is_absolute() {
            return Ok(path.to_path_buf());
        }
        utils::path_relative_to(&self.worktree_root, path).map_err(|error| {
            RailError::message(format!(
                "path '{}' is outside git worktree '{}': {}",
                path.display(),
                self.worktree_root.display(),
                error
            ))
        })
    }

    /// Check if there are staged changes
    pub fn has_staged_changes(&self) -> RailResult<bool> {
        // git diff --cached --quiet returns 1 if there are changes
        Ok(!self.run_git_check(&["diff", "--cached", "--quiet"]))
    }

    /// Create a commit with the given message
    pub fn commit(&self, message: &str) -> RailResult<String> {
        self.commit_with_env(message, &[])
    }

    /// Create a commit with operation-specific hook context.
    pub(crate) fn commit_with_env(&self, message: &str, env: &[(&str, &str)]) -> RailResult<String> {
        self.run_git_observable_with_env(&["commit", "-m", message], env)?;
        // Return the new commit SHA
        self.run_git_stdout(&["rev-parse", "HEAD"])
    }

    /// Create a tag
    ///
    /// If `message` is Some, creates an annotated tag. Otherwise creates a lightweight tag.
    /// If `sign` is true, creates a signed tag (requires GPG/SSH key).
    /// If `sign` is false, explicitly disables signing to override user's git config.
    pub fn create_tag(&self, name: &str, message: Option<&str>, sign: bool) -> RailResult<()> {
        let mut cmd = self.git_cmd();

        if sign {
            cmd.args(["tag", "-s"]);
        } else {
            // Override user's tag.gpgsign=true config to ensure unsigned tag
            cmd.args(["-c", "tag.gpgsign=false", "tag", "-a"]);
        }

        if let Some(msg) = message {
            cmd.args(["-m", msg]);
        }

        cmd.arg(name);

        let output = observable_output(&mut cmd).context("Failed to run git tag")?;

        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: format!("git tag {}", name),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }

        Ok(())
    }

    /// Find the latest tag matching a pattern (sorted by version)
    ///
    /// Produces the tag name when found, or `None` when no tags match.
    /// Uses version sorting to find the highest version tag.
    pub fn find_latest_tag(&self, pattern: &str) -> RailResult<Option<String>> {
        let mut cmd = self.git_cmd();
        cmd.args(["tag", "--list", pattern, "--sort=-version:refname"]);

        match cmd.output() {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                Ok(stdout.lines().next().map(|s| s.to_string()))
            }
            Ok(_) => Ok(None), // No matching tags found
            Err(e) => Err(RailError::message(format!("Failed to find tag: {}", e))),
        }
    }

    /// Check if a remote URL has content (branches)
    pub fn ls_remote_has_content(&self, url: &str) -> RailResult<bool> {
        Ok(!self.run_git(&["ls-remote", "--heads", url])?.stdout.is_empty())
    }

    /// Resolve one exact remote branch without mutating local refs.
    pub(crate) fn remote_branch_head(&self, remote: &str, branch: &str) -> RailResult<Option<String>> {
        let reference = format!("refs/heads/{branch}");
        let output = self.run_git(&["ls-remote", "--heads", remote, &reference])?;
        let stdout = String::from_utf8(output.stdout)?;
        let mut lines = stdout.lines().filter(|line| !line.trim().is_empty());
        let Some(line) = lines.next() else {
            return Ok(None);
        };
        if lines.next().is_some() {
            return Err(RailError::message(format!(
                "remote branch '{}' resolved to multiple refs",
                branch
            )));
        }
        let mut fields = line.split_whitespace();
        let sha = fields
            .next()
            .ok_or_else(|| RailError::message("remote branch response has no object ID"))?;
        let observed_reference = fields
            .next()
            .ok_or_else(|| RailError::message("remote branch response has no ref name"))?;
        if fields.next().is_some() || observed_reference != reference {
            return Err(RailError::message("remote branch response is malformed"));
        }
        if !matches!(sha.len(), 40 | 64) || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(RailError::message("remote branch response has an invalid object ID"));
        }
        Ok(Some(sha.to_ascii_lowercase()))
    }

    /// Get formatted git log output
    ///
    /// Returns commit data in the specified format between the given refs.
    pub fn log_formatted(&self, format: &str, from: Option<&str>, to: &str) -> RailResult<String> {
        let format_arg = format!("--format={}", format);
        let range = if let Some(from_ref) = from {
            format!("{}..{}", from_ref, to)
        } else {
            to.to_string()
        };

        self.run_git_stdout(&["log", &format_arg, &range])
    }

    /// Check if signing is configured (GPG or SSH)
    pub fn has_signing_configured(&self) -> bool {
        self.get_config("user.signingkey").ok().flatten().is_some()
            || self.get_config("gpg.format").ok().flatten().is_some()
    }
}

pub(crate) fn observable_output(cmd: &mut Command) -> io::Result<Output> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Git stdout pipe was unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("Git stderr pipe was unavailable"))?;
    let stream = !crate::output::is_json_mode() && !crate::output::is_quiet();

    let (status, stdout, stderr) = std::thread::scope(|scope| {
        let stdout_task = scope.spawn(move || capture_pipe(stdout, io::stdout(), stream));
        let stderr_task = scope.spawn(move || capture_pipe(stderr, io::stderr(), stream));
        let status = child.wait();
        if status.is_err() {
            drop(child.kill());
            drop(child.wait());
        }
        let stdout = stdout_task
            .join()
            .map_err(|_| io::Error::other("Git stdout reader panicked"))??;
        let stderr = stderr_task
            .join()
            .map_err(|_| io::Error::other("Git stderr reader panicked"))??;
        Ok::<_, io::Error>((status?, stdout, stderr))
    })?;

    Ok(Output { status, stdout, stderr })
}

fn capture_pipe<R: Read, W: Write>(mut reader: R, mut writer: W, mut stream: bool) -> io::Result<Vec<u8>> {
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        captured.extend_from_slice(&buffer[..read]);
        if stream && (writer.write_all(&buffer[..read]).and_then(|()| writer.flush())).is_err() {
            stream = false;
        }
    }
    Ok(captured)
}

/// Initialize a new git repository at the given path
///
/// This is a standalone function since it creates a new repo (no existing SystemGit).
pub fn init_repo(path: &std::path::Path, initial_branch: &str) -> RailResult<()> {
    let mut command = git_command();
    sanitize_git_environment(&mut command);
    let output = command
        .arg("init")
        .arg("--initial-branch")
        .arg(initial_branch)
        .arg(path)
        .output()
        .context("Failed to run git init")?;

    if !output.status.success() {
        return Err(RailError::Git(GitError::CommandFailed {
            command: "git init".to_string(),
            stderr: git_command_diagnostics(&output.stdout, &output.stderr),
        }));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::fs;

    #[test]
    fn test_git_cmd_removes_repository_redirection_environment() {
        let git = SystemGit::open(Path::new(".")).unwrap();
        let cmd = git.git_cmd();
        for key in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY"] {
            assert!(
                cmd.get_envs()
                    .any(|(configured, value)| configured == OsStr::new(key) && value.is_none()),
                "{} must not override cargo-rail's repository boundary",
                key
            );
        }
        assert!(
            cmd.get_envs().any(|(configured, value)| {
                configured == OsStr::new("GIT_NO_LAZY_FETCH") && value == Some(OsStr::new("1"))
            }),
            "authority reads must not lazily hydrate promisor objects"
        );
    }

    #[test]
    fn test_stage_paths_never_stages_unplanned_files() {
        let temp = tempfile::TempDir::new().unwrap();
        init_repo(temp.path(), "main").unwrap();
        let git = SystemGit::open(temp.path()).unwrap();
        git.set_config("user.name", "Test User").unwrap();
        git.set_config("user.email", "test@example.com").unwrap();
        fs::write(temp.path().join("planned.txt"), "before\n").unwrap();
        fs::write(temp.path().join("unplanned.txt"), "before\n").unwrap();
        git.stage_all().unwrap();
        git.commit("initial").unwrap();

        fs::write(temp.path().join("planned.txt"), "after\n").unwrap();
        fs::write(temp.path().join("unplanned.txt"), "after\n").unwrap();
        git.stage_paths(&[PathBuf::from("planned.txt")]).unwrap();

        let staged = git.run_git_stdout(&["diff", "--cached", "--name-only"]).unwrap();
        assert_eq!(staged, "planned.txt");
        assert_eq!(
            git.changed_paths().unwrap(),
            vec![PathBuf::from("planned.txt"), PathBuf::from("unplanned.txt")]
        );
    }

    #[test]
    fn test_hash_bytes_uses_stable_git_object_ids() {
        let git = SystemGit::open(Path::new(".")).unwrap();
        let first = git.hash_bytes(b"approved inputs").unwrap();
        let repeated = git.hash_bytes(b"approved inputs").unwrap();
        let changed = git.hash_bytes(b"different inputs").unwrap();

        assert_eq!(first, repeated);
        assert_ne!(first, changed);
        assert!(first.len() >= 40);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn obstructing_paths_ignore_assume_unchanged_and_skip_worktree_hints() {
        let temp = tempfile::TempDir::new().unwrap();
        init_repo(temp.path(), "main").unwrap();
        let git = SystemGit::open(temp.path()).unwrap();
        git.set_config("user.name", "Test User").unwrap();
        git.set_config("user.email", "test@example.com").unwrap();
        fs::write(temp.path().join("tracked.txt"), "before\n").unwrap();
        git.stage_all().unwrap();
        git.commit("initial").unwrap();

        git.run_git(&["update-index", "--assume-unchanged", "tracked.txt"])
            .unwrap();
        fs::write(temp.path().join("tracked.txt"), "assume-unchanged edit\n").unwrap();
        assert_eq!(
            git.obstructing_worktree_paths().unwrap(),
            vec![PathBuf::from("tracked.txt")]
        );

        git.run_git(&["update-index", "--no-assume-unchanged", "tracked.txt"])
            .unwrap();
        git.run_git(&["checkout", "--", "tracked.txt"]).unwrap();
        git.run_git(&["update-index", "--skip-worktree", "tracked.txt"])
            .unwrap();
        fs::write(temp.path().join("tracked.txt"), "skip-worktree edit\n").unwrap();
        assert_eq!(
            git.obstructing_worktree_paths().unwrap(),
            vec![PathBuf::from("tracked.txt")]
        );
    }

    #[test]
    fn obstructing_paths_capture_staged_unstaged_untracked_and_ignored_state() {
        let temp = tempfile::TempDir::new().unwrap();
        init_repo(temp.path(), "main").unwrap();
        let git = SystemGit::open(temp.path()).unwrap();
        git.set_config("user.name", "Test User").unwrap();
        git.set_config("user.email", "test@example.com").unwrap();
        fs::write(temp.path().join("staged.txt"), "before\n").unwrap();
        fs::write(temp.path().join("unstaged.txt"), "before\n").unwrap();
        fs::write(temp.path().join(".gitignore"), "ignored.txt\n").unwrap();
        git.stage_all().unwrap();
        git.commit("initial").unwrap();

        fs::write(temp.path().join("staged.txt"), "after\n").unwrap();
        git.stage_paths(&[PathBuf::from("staged.txt")]).unwrap();
        fs::write(temp.path().join("unstaged.txt"), "after\n").unwrap();
        fs::write(temp.path().join("untracked.txt"), "new\n").unwrap();
        fs::write(temp.path().join("ignored.txt"), "ignored\n").unwrap();

        assert_eq!(
            git.obstructing_worktree_paths().unwrap(),
            vec![
                PathBuf::from("ignored.txt"),
                PathBuf::from("staged.txt"),
                PathBuf::from("unstaged.txt"),
                PathBuf::from("untracked.txt"),
            ]
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn obstructing_paths_preserve_non_utf8_git_names() {
        use std::os::unix::ffi::OsStringExt as _;

        let temp = tempfile::TempDir::new().unwrap();
        init_repo(temp.path(), "main").unwrap();
        let git = SystemGit::open(temp.path()).unwrap();
        let name = std::ffi::OsString::from_vec(b"non-utf8-\xff".to_vec());
        fs::write(temp.path().join(&name), "untracked\n").unwrap();

        assert_eq!(git.obstructing_worktree_paths().unwrap(), vec![PathBuf::from(name)]);
    }

    #[cfg(windows)]
    #[test]
    fn test_git_accepts_windows_canonical_paths_at_process_boundaries() {
        let temp = tempfile::TempDir::new().unwrap();
        init_repo(temp.path(), "main").unwrap();
        let git = SystemGit::open(temp.path()).unwrap();
        git.set_config("user.name", "Test User").unwrap();
        git.set_config("user.email", "test@example.com").unwrap();
        let file = temp.path().join("tracked.txt");
        fs::write(&file, "before\n").unwrap();
        git.stage_all().unwrap();
        git.commit("initial").unwrap();

        let verbatim_root = fs::canonicalize(temp.path()).unwrap();
        let verbatim_file = fs::canonicalize(&file).unwrap();
        let git = SystemGit::open(&verbatim_root).unwrap();
        assert_eq!(
            git.get_commits_touching_path(&verbatim_file, None, "HEAD")
                .unwrap()
                .len(),
            1
        );

        fs::write(&file, "after\n").unwrap();
        git.stage_paths(&[verbatim_file]).unwrap();
        assert_eq!(
            git.run_git_stdout(&["diff", "--cached", "--name-only"]).unwrap(),
            "tracked.txt"
        );
    }
}
