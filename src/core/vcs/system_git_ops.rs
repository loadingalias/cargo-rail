//! Additional operations for SystemGit (commit walking, remotes, etc.)

use super::system_git::SystemGit;
use super::CommitInfo;
use crate::core::error::{GitError, RailError, RailResult, ResultExt};
use std::path::{Path, PathBuf};
use std::process::Command;

impl SystemGit {
    /// Get commits touching specific paths
    ///
    /// Uses `git rev-list` with path filtering for efficient traversal.
    pub fn get_commits_touching_path(&self, paths: &[PathBuf], since: Option<&str>) -> RailResult<Vec<String>> {
        let mut cmd = self.git_cmd();
        cmd.args(["rev-list", "--no-merges", "--reverse"]);

        if let Some(since_sha) = since {
            cmd.arg(format!("{}..HEAD", since_sha));
        } else {
            cmd.arg("HEAD");
        }

        if !paths.is_empty() {
            cmd.arg("--");
            for path in paths {
                cmd.arg(path);
            }
        }

        let output = cmd.output().context("Failed to run git rev-list")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git rev-list".to_string(),
                stderr: stderr.to_string(),
            }));
        }

        let commits = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        Ok(commits)
    }

    /// Get commit metadata for a single SHA
    ///
    /// Uses `git log -1 --format` for efficient single-commit lookup.
    pub fn get_commit(&self, sha: &str) -> RailResult<CommitInfo> {
        // Format: %H (hash) %an (author name) %ae (author email) %at (author time)
        //         %cn (committer name) %ce (committer email) %ct (committer time)
        //         %P (parent hashes) %B (body)
        let format = "%H%n%an%n%ae%n%at%n%cn%n%ce%n%ct%n%P%n%B";

        let output = self
            .git_cmd()
            .args(["log", "-1", &format!("--format={}", format), sha])
            .output()
            .context("Failed to get commit info")?;

        if !output.status.success() {
            return Err(RailError::Git(GitError::CommitNotFound {
                sha: sha.to_string(),
            }));
        }

        parse_commit_output(&output.stdout)
    }

    /// Get all commits in chronological order (oldest first)
    pub fn get_all_commits_chronological(&self) -> RailResult<Vec<CommitInfo>> {
        let shas = self.get_commits_touching_path(&[], None)?;

        // Get all commits in parallel chunks
        use rayon::prelude::*;

        let commits: Result<Vec<_>, _> = shas
            .par_iter()
            .map(|sha| self.get_commit(sha))
            .collect();

        commits
    }

    /// List all files at a specific commit under a path
    pub fn list_files_at_commit(&self, commit_sha: &str, path: &Path) -> RailResult<Vec<PathBuf>> {
        let spec = if path.as_os_str().is_empty() {
            commit_sha.to_string()
        } else {
            format!("{}:{}", commit_sha, path.display())
        };

        let output = self
            .git_cmd()
            .args(["ls-tree", "-r", "--name-only", &spec])
            .output()
            .context("Failed to list files")?;

        if !output.status.success() {
            return Ok(vec![]);
        }

        let files = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(PathBuf::from)
            .collect();

        Ok(files)
    }

    /// Collect all files from a tree recursively
    pub fn collect_tree_files(&self, commit_sha: &str, path: &Path) -> RailResult<Vec<(PathBuf, Vec<u8>)>> {
        let files = self.list_files_at_commit(commit_sha, path)?;

        let mut results = Vec::with_capacity(files.len());
        for file in files {
            let full_path = path.join(&file);
            let content = self.read_file_at_commit(commit_sha, &full_path)?;
            results.push((file, content));
        }

        Ok(results)
    }

    /// Add a remote repository
    pub fn add_remote(&self, name: &str, url: &str) -> RailResult<()> {
        let output = self
            .git_cmd()
            .args(["remote", "add", name, url])
            .output()
            .context("Failed to add remote")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("already exists") {
                return Ok(()); // Remote exists, not an error
            }
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git remote add".to_string(),
                stderr: stderr.to_string(),
            }));
        }

        Ok(())
    }

    /// List all remotes
    pub fn list_remotes(&self) -> RailResult<Vec<(String, String)>> {
        let output = self
            .git_cmd()
            .args(["remote", "-v"])
            .output()
            .context("Failed to list remotes")?;

        if !output.status.success() {
            return Ok(vec![]);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut remotes = Vec::new();

        for line in stdout.lines() {
            // Format: "origin  git@github.com:user/repo.git (fetch)"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && line.contains("(fetch)") {
                remotes.push((parts[0].to_string(), parts[1].to_string()));
            }
        }

        Ok(remotes)
    }

    /// Push to remote
    pub fn push_to_remote(&self, remote_name: &str, branch: &str) -> RailResult<()> {
        println!("   Pushing to remote '{}'...", remote_name);

        let output = self
            .git_cmd()
            .args(["push", "-u", remote_name, branch])
            .output()
            .context("Failed to push")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RailError::Git(GitError::PushFailed {
                remote: remote_name.to_string(),
                branch: branch.to_string(),
                reason: stderr.to_string(),
            }));
        }

        println!("   ✅ Pushed to {}/{}", remote_name, branch);
        Ok(())
    }

    /// Fetch from remote
    pub fn fetch_from_remote(&self, remote_name: &str) -> RailResult<()> {
        println!("   Fetching from remote '{}'...", remote_name);

        let output = self
            .git_cmd()
            .args(["fetch", remote_name])
            .output()
            .context("Failed to fetch")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git fetch".to_string(),
                stderr: stderr.to_string(),
            }));
        }

        println!("   ✅ Fetched from {}", remote_name);
        Ok(())
    }

    /// Check if remote exists
    pub fn has_remote(&self, name: &str) -> RailResult<bool> {
        let remotes = self.list_remotes()?;
        Ok(remotes.iter().any(|(n, _)| n == name))
    }

    /// Get remote URL
    pub fn get_remote_url(&self, name: &str) -> RailResult<Option<String>> {
        let remotes = self.list_remotes()?;
        Ok(remotes.iter().find(|(n, _)| n == name).map(|(_, url)| url.clone()))
    }

    /// Create a branch
    pub fn create_branch(&self, branch_name: &str) -> RailResult<()> {
        let output = self
            .git_cmd()
            .args(["branch", branch_name])
            .output()
            .context("Failed to create branch")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git branch".to_string(),
                stderr: stderr.to_string(),
            }));
        }

        Ok(())
    }

    /// Checkout a branch
    pub fn checkout_branch(&self, branch_name: &str) -> RailResult<()> {
        let output = self
            .git_cmd()
            .args(["checkout", branch_name])
            .output()
            .context("Failed to checkout branch")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git checkout".to_string(),
                stderr: stderr.to_string(),
            }));
        }

        Ok(())
    }

    /// Create and checkout a branch
    pub fn create_and_checkout_branch(&self, branch_name: &str) -> RailResult<()> {
        self.create_branch(branch_name)?;
        self.checkout_branch(branch_name)?;
        Ok(())
    }
}

/// Parse git log output into CommitInfo
///
/// Format is %H%n%an%n%ae%n%at%n%cn%n%ce%n%ct%n%P%n%B
/// Which gives us: hash, author name, author email, author time,
///                 committer name, committer email, committer time,
///                 parent hashes, body
fn parse_commit_output(data: &[u8]) -> RailResult<CommitInfo> {
    let output = String::from_utf8_lossy(data);
    let mut lines = output.lines();

    let sha = lines.next().ok_or_else(|| RailError::message("Missing commit SHA"))?.to_string();
    let author = lines.next().ok_or_else(|| RailError::message("Missing author name"))?.to_string();
    let author_email = lines.next().ok_or_else(|| RailError::message("Missing author email"))?.to_string();
    let timestamp = lines
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| RailError::message("Missing/invalid author timestamp"))?;
    let committer = lines.next().ok_or_else(|| RailError::message("Missing committer name"))?.to_string();
    let committer_email = lines.next().ok_or_else(|| RailError::message("Missing committer email"))?.to_string();
    let _committer_timestamp = lines.next(); // We don't use this
    let parents_line = lines.next().unwrap_or("");
    let parent_shas = if parents_line.is_empty() {
        vec![]
    } else {
        parents_line.split_whitespace().map(|s| s.to_string()).collect()
    };

    // Rest is commit message
    let message: Vec<String> = lines.map(|s| s.to_string()).collect();
    let message = message.join("\n").trim().to_string();

    Ok(CommitInfo {
        sha,
        author,
        author_email,
        committer,
        committer_email,
        message,
        timestamp,
        parent_shas,
    })
}
