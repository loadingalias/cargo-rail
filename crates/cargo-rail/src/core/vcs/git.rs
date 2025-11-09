#![allow(dead_code)]

use super::{CommitInfo, Vcs};
use anyhow::{Context, Result};
use gix::Repository;
use std::path::{Path, PathBuf};

/// Git implementation using gix (gitoxide)
pub struct GitBackend {
  repo: Repository,
  root: PathBuf,
}

impl Vcs for GitBackend {
  fn open(path: &Path) -> Result<Self>
  where
    Self: Sized,
  {
    let repo = gix::open(path).with_context(|| format!("Failed to open git repository at {}", path.display()))?;
    let root = repo
      .workdir()
      .context("Repository has no working directory")?
      .to_path_buf();

    Ok(Self { repo, root })
  }

  fn root(&self) -> &Path {
    &self.root
  }

  fn head_commit(&self) -> Result<String> {
    let mut head = self.repo.head()?;
    let commit = head.peel_to_commit()?;
    Ok(commit.id().to_string())
  }

  fn commit_history(&self, _path: &Path, limit: Option<usize>) -> Result<Vec<CommitInfo>> {
    let mut head = self.repo.head()?;
    let commit = head.peel_to_commit()?;

    let mut commits = Vec::new();
    let mut current = Some(commit);
    let mut count = 0;

    while let Some(commit_obj) = current {
      if let Some(max) = limit
        && count >= max
      {
        break;
      }

      let commit_id = commit_obj.id();
      let commit = commit_obj.decode()?;

      // Get author and committer info
      let author = commit.author();
      let committer = commit.committer();

      // Parse timestamp - gix uses seconds since Unix epoch
      let timestamp = std::str::from_utf8(author.time.as_ref())
        .ok()
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);

      commits.push(CommitInfo {
        sha: commit_id.to_string(),
        author: author.name.to_string(),
        author_email: author.email.to_string(),
        committer: committer.name.to_string(),
        committer_email: committer.email.to_string(),
        message: commit.message.to_string(),
        timestamp,
        parent_shas: commit.parents().map(|p| p.to_string()).collect(),
      });

      count += 1;

      // Move to first parent
      current = commit
        .parents()
        .next()
        .and_then(|parent_id| self.repo.find_object(parent_id).ok())
        .and_then(|obj| obj.try_into_commit().ok());
    }

    Ok(commits)
  }

  fn is_tracked(&self, path: &Path) -> Result<bool> {
    // Check if the file exists in the index
    let relative_path = if path.is_absolute() {
      path.strip_prefix(&self.root).unwrap_or(path)
    } else {
      path
    };

    let index = self.repo.index()?;
    let path_bytes = gix::path::os_str_into_bstr(relative_path.as_os_str())?;

    Ok(index.entry_by_path(path_bytes).is_some())
  }

  fn list_files_at_commit(&self, commit_sha: &str, path: &Path) -> Result<Vec<PathBuf>> {
    let commit_id =
      gix::ObjectId::from_hex(commit_sha.as_bytes()).with_context(|| format!("Invalid commit SHA: {}", commit_sha))?;
    let commit = self.repo.find_object(commit_id)?.try_into_commit()?;
    let tree = commit.tree()?;

    let mut files = Vec::new();
    let relative_path = if path.is_absolute() {
      path.strip_prefix(&self.root).unwrap_or(path)
    } else {
      path
    };

    // If path is empty or ".", list all files
    if relative_path.as_os_str().is_empty() || relative_path == Path::new(".") {
      for entry in tree.iter() {
        let entry = entry?;
        if entry.mode().is_blob() {
          files.push(PathBuf::from(entry.filename().to_string()));
        }
      }
    } else {
      // Look up specific subtree
      if let Some(entry) = tree.lookup_entry_by_path(relative_path)? {
        if entry.mode().is_tree() {
          let subtree = entry.object()?.into_tree();
          for entry in subtree.iter() {
            let entry = entry?;
            if entry.mode().is_blob() {
              let full_path = relative_path.join(entry.filename().to_string());
              files.push(full_path);
            }
          }
        } else if entry.mode().is_blob() {
          files.push(relative_path.to_path_buf());
        }
      }
    }

    Ok(files)
  }

  fn read_file_at_commit(&self, commit_sha: &str, path: &Path) -> Result<Vec<u8>> {
    let commit_id =
      gix::ObjectId::from_hex(commit_sha.as_bytes()).with_context(|| format!("Invalid commit SHA: {}", commit_sha))?;
    let commit = self.repo.find_object(commit_id)?.try_into_commit()?;
    let tree = commit.tree()?;

    let relative_path = if path.is_absolute() {
      path.strip_prefix(&self.root).unwrap_or(path)
    } else {
      path
    };

    let entry = tree
      .lookup_entry_by_path(relative_path)?
      .with_context(|| format!("File not found at commit {}: {}", commit_sha, path.display()))?;

    let blob = entry.object()?.into_blob();
    Ok(blob.data.to_vec())
  }
}

// Remote operations (push, fetch, remotes)
impl GitBackend {
  /// Add a remote to the repository
  pub fn add_remote(&self, name: &str, url: &str) -> Result<()> {
    use std::process::Command;

    let output = Command::new("git")
      .current_dir(&self.root)
      .args(["remote", "add", name, url])
      .output()
      .context("Failed to execute git remote add")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      // Ignore error if remote already exists
      if stderr.contains("already exists") {
        return Ok(());
      }
      anyhow::bail!("git remote add failed: {}", stderr);
    }

    Ok(())
  }

  /// List all remotes in the repository
  pub fn list_remotes(&self) -> Result<Vec<(String, String)>> {
    use std::process::Command;

    let output = Command::new("git")
      .current_dir(&self.root)
      .args(["remote", "-v"])
      .output()
      .context("Failed to execute git remote -v")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      anyhow::bail!("git remote -v failed: {}", stderr);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut remotes = Vec::new();

    for line in stdout.lines() {
      // Format: "origin  git@github.com:user/repo.git (fetch)"
      let parts: Vec<&str> = line.split_whitespace().collect();
      if parts.len() >= 2 {
        let name = parts[0].to_string();
        let url = parts[1].to_string();
        // Only add fetch URLs (ignore push URLs which are duplicates)
        if line.contains("(fetch)") {
          remotes.push((name, url));
        }
      }
    }

    Ok(remotes)
  }

  /// Push commits to a remote repository
  /// Uses SSH authentication (must be configured in git/ssh config)
  pub fn push_to_remote(&self, remote_name: &str, branch: &str) -> Result<()> {
    use std::process::Command;

    println!("   Pushing to remote '{}'...", remote_name);

    let output = Command::new("git")
      .current_dir(&self.root)
      .args(["push", "-u", remote_name, branch])
      .output()
      .context("Failed to execute git push")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      anyhow::bail!("git push failed: {}", stderr);
    }

    println!("   ✅ Pushed to {}/{}", remote_name, branch);
    Ok(())
  }

  /// Fetch commits from a remote repository
  pub fn fetch_from_remote(&self, remote_name: &str) -> Result<()> {
    use std::process::Command;

    println!("   Fetching from remote '{}'...", remote_name);

    let output = Command::new("git")
      .current_dir(&self.root)
      .args(["fetch", remote_name])
      .output()
      .context("Failed to execute git fetch")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      anyhow::bail!("git fetch failed: {}", stderr);
    }

    println!("   ✅ Fetched from {}", remote_name);
    Ok(())
  }

  /// Check if a remote exists
  pub fn has_remote(&self, name: &str) -> Result<bool> {
    let remotes = self.list_remotes()?;
    Ok(remotes.iter().any(|(remote_name, _)| remote_name == name))
  }

  /// Get the URL of a remote
  pub fn get_remote_url(&self, name: &str) -> Result<Option<String>> {
    let remotes = self.list_remotes()?;
    Ok(
      remotes
        .into_iter()
        .find(|(remote_name, _)| remote_name == name)
        .map(|(_, url)| url),
    )
  }
}

// Additional methods for split operations (not part of Vcs trait)
impl GitBackend {
  /// Get detailed information about a specific commit
  pub fn get_commit(&self, sha: &str) -> Result<CommitInfo> {
    let commit_id = gix::ObjectId::from_hex(sha.as_bytes()).with_context(|| format!("Invalid commit SHA: {}", sha))?;
    let commit_obj = self.repo.find_object(commit_id)?.try_into_commit()?;
    let commit = commit_obj.decode()?;

    let author = commit.author();
    let committer = commit.committer();

    let timestamp = std::str::from_utf8(author.time.as_ref())
      .ok()
      .and_then(|s| s.split_whitespace().next())
      .and_then(|s| s.parse::<i64>().ok())
      .unwrap_or(0);

    Ok(CommitInfo {
      sha: sha.to_string(),
      author: author.name.to_string(),
      author_email: author.email.to_string(),
      committer: committer.name.to_string(),
      committer_email: committer.email.to_string(),
      message: commit.message.to_string(),
      timestamp,
      parent_shas: commit.parents().map(|p| p.to_string()).collect(),
    })
  }

  /// Get all commits in chronological order (oldest first)
  pub fn get_all_commits_chronological(&self) -> Result<Vec<CommitInfo>> {
    let mut commits = self.commit_history(Path::new("."), None)?;
    commits.reverse(); // Reverse to get chronological order
    Ok(commits)
  }

  /// Check if a commit touches any of the given paths
  /// Returns true if the commit actually changed any of the given paths
  pub fn commit_touches_paths(&self, sha: &str, paths: &[PathBuf]) -> Result<bool> {
    let commit_id = gix::ObjectId::from_hex(sha.as_bytes()).with_context(|| format!("Invalid commit SHA: {}", sha))?;
    let commit_obj = self.repo.find_object(commit_id)?.try_into_commit()?;

    // For root commits (no parents), check if paths exist
    if commit_obj.parent_ids().count() == 0 {
      let tree = commit_obj.tree()?;
      for path in paths {
        let relative_path = if path.is_absolute() {
          path.strip_prefix(&self.root).unwrap_or(path)
        } else {
          path
        };
        if tree.lookup_entry_by_path(relative_path).ok().flatten().is_some()
          || self.tree_contains_path_prefix(&tree, relative_path)?
        {
          return Ok(true);
        }
      }
      return Ok(false);
    }

    // For commits with parents, get all changed files and check if any match our paths
    let output = std::process::Command::new("git")
      .current_dir(&self.root)
      .args(["diff-tree", "--no-commit-id", "--name-only", "-r", sha])
      .output()
      .context("Failed to run git diff-tree")?;

    if !output.status.success() {
      anyhow::bail!("git diff-tree failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let changed_files = String::from_utf8_lossy(&output.stdout);

    // Check if any changed file is under any of our target paths
    for changed_file in changed_files.lines() {
      let changed_path = Path::new(changed_file);
      for target_path in paths {
        let relative_target = if target_path.is_absolute() {
          target_path.strip_prefix(&self.root).unwrap_or(target_path)
        } else {
          target_path
        };

        // Check if the changed file is under the target path
        if changed_path.starts_with(relative_target) {
          return Ok(true);
        }
      }
    }

    Ok(false)
  }

  /// Helper to check if a tree contains any entries with the given path prefix
  fn tree_contains_path_prefix(&self, tree: &gix::Tree, prefix: &Path) -> Result<bool> {
    check_tree_recursive(tree, Path::new(""), prefix)
  }

  /// Get commits in a range that touch a specific path
  /// Returns commits in chronological order (oldest first)
  pub fn get_commits_touching_path(
    &self,
    path: &Path,
    since_sha: Option<&str>,
    until_ref: &str,
  ) -> Result<Vec<CommitInfo>> {
    use std::process::Command;

    let relative_path = if path.is_absolute() {
      path.strip_prefix(&self.root).unwrap_or(path)
    } else {
      path
    };

    let mut args = vec!["log", "--reverse", "--format=%H"];

    // Add range
    let range;
    if let Some(since) = since_sha {
      range = format!("{}..{}", since, until_ref);
      args.push(&range);
    } else {
      args.push(until_ref);
    }

    // Add path filter
    args.push("--");
    let path_str = relative_path.to_string_lossy();
    args.push(&path_str);

    let output = Command::new("git")
      .current_dir(&self.root)
      .args(&args)
      .output()
      .context("Failed to get commit range")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      anyhow::bail!("git log failed: {}", stderr);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut commits = Vec::new();

    for line in stdout.lines() {
      let sha = line.trim();
      if !sha.is_empty() {
        commits.push(self.get_commit(sha)?);
      }
    }

    Ok(commits)
  }

  /// Get files changed in a specific commit
  /// Returns list of (path, change_type) where change_type is A(dded), M(odified), D(eleted)
  pub fn get_changed_files(&self, commit_sha: &str) -> Result<Vec<(PathBuf, char)>> {
    use std::process::Command;

    let output = Command::new("git")
      .current_dir(&self.root)
      .args(["diff-tree", "--no-commit-id", "--name-status", "-r", commit_sha])
      .output()
      .context("Failed to get changed files")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      anyhow::bail!("git diff-tree failed: {}", stderr);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut files = Vec::new();

    for line in stdout.lines() {
      let parts: Vec<&str> = line.split_whitespace().collect();
      if parts.len() >= 2 {
        let change_type = parts[0].chars().next().unwrap_or('M');
        let path = PathBuf::from(parts[1]);
        files.push((path, change_type));
      }
    }

    Ok(files)
  }

  /// Get file content at a specific commit (returns None if file doesn't exist)
  pub fn get_file_at_commit(&self, commit_sha: &str, path: &Path) -> Result<Option<Vec<u8>>> {
    use std::process::Command;

    let relative_path = if path.is_absolute() {
      path.strip_prefix(&self.root).unwrap_or(path)
    } else {
      path
    };

    let path_str = relative_path.to_string_lossy();

    let output = Command::new("git")
      .current_dir(&self.root)
      .args(["show", &format!("{}:{}", commit_sha, path_str)])
      .output()
      .context("Failed to get file content")?;

    if !output.status.success() {
      // File doesn't exist at this commit
      return Ok(None);
    }

    Ok(Some(output.stdout))
  }

  /// Create a commit with specific metadata
  /// Returns the new commit SHA
  pub fn create_commit_with_metadata(
    &self,
    message: &str,
    author_name: &str,
    author_email: &str,
    timestamp: i64,
    parent_shas: &[String],
  ) -> Result<String> {
    use std::process::Command;

    // Stage all changes
    Command::new("git")
      .current_dir(&self.root)
      .args(["add", "-A"])
      .output()
      .context("Failed to stage changes")?;

    // Write tree
    let tree_output = Command::new("git")
      .current_dir(&self.root)
      .args(["write-tree"])
      .output()
      .context("Failed to write tree")?;

    let tree_sha = String::from_utf8(tree_output.stdout)?.trim().to_string();

    // Build commit-tree command
    let author_date = format!("{} +0000", timestamp);
    let mut cmd = Command::new("git");
    cmd
      .current_dir(&self.root)
      .env("GIT_AUTHOR_NAME", author_name)
      .env("GIT_AUTHOR_EMAIL", author_email)
      .env("GIT_AUTHOR_DATE", &author_date)
      .env("GIT_COMMITTER_NAME", author_name)
      .env("GIT_COMMITTER_EMAIL", author_email)
      .env("GIT_COMMITTER_DATE", &author_date)
      .arg("commit-tree")
      .arg(&tree_sha)
      .arg("-m")
      .arg(message);

    // Add parent arguments
    for parent in parent_shas {
      cmd.arg("-p").arg(parent);
    }

    let output = cmd.output().context("Failed to create commit")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      anyhow::bail!("git commit-tree failed: {}", stderr);
    }

    let commit_sha = String::from_utf8(output.stdout)?.trim().to_string();

    // Update HEAD
    Command::new("git")
      .current_dir(&self.root)
      .args(["reset", "--soft", &commit_sha])
      .output()
      .context("Failed to update HEAD")?;

    Ok(commit_sha)
  }

  /// Recursively collect all files under a path in a tree
  pub fn collect_tree_files(&self, commit_sha: &str, path: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let commit_id =
      gix::ObjectId::from_hex(commit_sha.as_bytes()).with_context(|| format!("Invalid commit SHA: {}", commit_sha))?;
    let commit = self.repo.find_object(commit_id)?.try_into_commit()?;
    let tree = commit.tree()?;

    let relative_path = if path.is_absolute() {
      path.strip_prefix(&self.root).unwrap_or(path)
    } else {
      path
    };

    let mut files = Vec::new();

    // Get the subtree for this path
    let subtree = if relative_path.as_os_str().is_empty() || relative_path == Path::new(".") {
      tree
    } else {
      match tree.lookup_entry_by_path(relative_path)? {
        Some(entry) if entry.mode().is_tree() => entry.object()?.into_tree(),
        _ => return Ok(files),
      }
    };

    // Recursively collect all files
    collect_files_recursive(&subtree, relative_path, &mut files)?;

    Ok(files)
  }
}

/// Recursively check if any entry in the tree matches the prefix
fn check_tree_recursive(tree: &gix::Tree, current_path: &Path, prefix: &Path) -> Result<bool> {
  for entry in tree.iter() {
    let entry = entry?;
    let name = entry.filename().to_string();
    let entry_path = current_path.join(&name);

    // Check if this entry path starts with the prefix
    if entry_path.starts_with(prefix) {
      return Ok(true);
    }

    // If prefix starts with this path, recurse into subdirectory
    if prefix.starts_with(&entry_path) && entry.mode().is_tree() {
      let subtree = entry.object()?.into_tree();
      if check_tree_recursive(&subtree, &entry_path, prefix)? {
        return Ok(true);
      }
    }
  }

  Ok(false)
}

/// Helper to recursively collect files from a tree
fn collect_files_recursive(tree: &gix::Tree, base_path: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) -> Result<()> {
  for entry in tree.iter() {
    let entry = entry?;
    let name = entry.filename().to_string();
    let entry_path = if base_path.as_os_str().is_empty() || base_path == Path::new(".") {
      PathBuf::from(&name)
    } else {
      base_path.join(&name)
    };

    if entry.mode().is_blob() {
      let blob = entry.object()?.into_blob();
      files.push((entry_path, blob.data.to_vec()));
    } else if entry.mode().is_tree() {
      let subtree = entry.object()?.into_tree();
      collect_files_recursive(&subtree, &entry_path, files)?;
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Helper to find the git repository root from the current directory.
  /// This is needed because tests run from the crate directory, but the
  /// git repository may be at the workspace root.
  fn find_git_root() -> PathBuf {
    let current_dir = std::env::current_dir().unwrap();
    match gix::discover(&current_dir) {
      Ok(repo) => repo.workdir().unwrap().to_path_buf(),
      Err(_) => current_dir,
    }
  }

  #[test]
  fn test_open_current_repo() {
    let git_root = find_git_root();
    let result = GitBackend::open(&git_root);
    assert!(result.is_ok(), "Should be able to open current git repo");
  }

  #[test]
  fn test_head_commit() {
    let git_root = find_git_root();
    let git = GitBackend::open(&git_root).unwrap();
    let head = git.head_commit();
    assert!(head.is_ok());
    let sha = head.unwrap();
    assert_eq!(sha.len(), 40); // Git SHA-1 is 40 hex chars
  }

  #[test]
  fn test_commit_history() {
    let git_root = find_git_root();
    let git = GitBackend::open(&git_root).unwrap();
    let commits = git.commit_history(Path::new("."), Some(5));
    assert!(commits.is_ok());
    let commits = commits.unwrap();
    assert!(!commits.is_empty());
    assert!(commits.len() <= 5);

    // Check first commit has required fields
    let first = &commits[0];
    assert!(!first.sha.is_empty());
    assert!(!first.author.is_empty());
    assert!(first.timestamp > 0);
  }

  #[test]
  fn test_is_tracked() {
    let git_root = find_git_root();
    let git = GitBackend::open(&git_root).unwrap();

    // Cargo.toml should be tracked
    let result = git.is_tracked(Path::new("Cargo.toml"));
    assert!(result.is_ok());
    assert!(result.unwrap());

    // A non-existent file should not be tracked
    let result = git.is_tracked(Path::new("this-file-does-not-exist-12345.txt"));
    assert!(result.is_ok());
    assert!(!result.unwrap());
  }

  #[test]
  fn test_list_remotes() {
    let git_root = find_git_root();
    let git = GitBackend::open(&git_root).unwrap();

    // Should be able to list remotes (may be empty for local repo)
    let result = git.list_remotes();
    assert!(result.is_ok());
    let remotes = result.unwrap();

    // If origin exists, check it's properly parsed
    if let Some((name, url)) = remotes.first() {
      assert!(!name.is_empty());
      assert!(!url.is_empty());
    }
  }

  #[test]
  fn test_has_remote() {
    let git_root = find_git_root();
    let git = GitBackend::open(&git_root).unwrap();

    // Check if origin exists (may or may not)
    let has_origin = git.has_remote("origin");
    assert!(has_origin.is_ok());

    // Non-existent remote should return false
    let has_fake = git.has_remote("this-remote-does-not-exist-12345");
    assert!(has_fake.is_ok());
    assert!(!has_fake.unwrap());
  }

  #[test]
  fn test_get_remote_url() {
    let git_root = find_git_root();
    let git = GitBackend::open(&git_root).unwrap();

    // Try to get origin URL (may or may not exist)
    let url = git.get_remote_url("origin");
    assert!(url.is_ok());

    // Non-existent remote should return None
    let url = git.get_remote_url("this-remote-does-not-exist-12345");
    assert!(url.is_ok());
    assert_eq!(url.unwrap(), None);
  }
}
