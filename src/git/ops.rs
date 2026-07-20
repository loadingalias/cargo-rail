//! Additional operations for SystemGit (commit walking, remotes, etc.)

use super::SystemGit;
use super::system::{CommitInfo, CommitMetadata};
use crate::error::{GitError, RailError, RailResult, ResultExt, git_command_diagnostics};
use crate::progress;
use crate::utils;
use std::path::{Path, PathBuf};

/// One exact entry from a Git tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitTreeEntry {
  /// Git file mode (`100644`, `100755`, `120000`, or `160000`).
  pub mode: String,
  /// Object ID referenced by the tree entry.
  pub object_id: String,
  /// Repository-relative entry path.
  pub path: PathBuf,
}

/// Exact index mutation used to synthesize one commit tree.
pub(crate) enum GitIndexChange {
  /// Insert or replace an entry with an exact Git mode and object ID.
  Upsert(GitTreeEntry),
  /// Remove one repository-relative path.
  Delete(PathBuf),
}

impl SystemGit {
  /// Normalize a path to be relative to the work tree
  ///
  /// If the path is absolute, resolve both representations before stripping the
  /// worktree prefix. This handles Windows drive casing, separators, and the
  /// verbatim prefix returned by `canonicalize` without accepting outside paths.
  fn normalize_path(&self, path: &Path) -> RailResult<PathBuf> {
    if path.is_absolute() {
      utils::path_relative_to(&self.worktree_root, path).map_err(|error| {
        RailError::message(format!(
          "failed to make '{}' relative to git worktree '{}': {}",
          path.display(),
          self.worktree_root.display(),
          error
        ))
      })
    } else {
      Ok(path.to_path_buf())
    }
  }

  /// Get commit history from HEAD with optional limit
  ///
  /// Returns commits in reverse chronological order (newest first).
  /// Uses parallel batch processing for optimal performance.
  pub fn commit_history(&self, limit: Option<usize>) -> RailResult<Vec<CommitInfo>> {
    let mut args = vec!["log", "--format=%H"];
    let limit_str;
    if let Some(max) = limit {
      limit_str = format!("-{}", max);
      args.push(&limit_str);
    }

    let output = self.run_git(&args)?;
    let shas: Vec<String> = String::from_utf8_lossy(&output.stdout)
      .lines()
      .map(|s| s.trim().to_string())
      .filter(|s| !s.is_empty())
      .collect();

    // Fetch commit info in parallel using bulk operation
    self.get_commits_bulk(&shas)
  }

  /// Get commits reachable from ordinary local, remote, and tag refs.
  ///
  /// Notes are intentionally excluded: origin recovery must work from normal
  /// refs, including a local sync branch that is not currently checked out.
  pub fn ordinary_commit_history(&self) -> RailResult<Vec<CommitInfo>> {
    let output = self.run_git(&["rev-list", "--branches", "--remotes", "--tags"])?;
    let mut seen = std::collections::HashSet::new();
    let shas = String::from_utf8_lossy(&output.stdout)
      .lines()
      .map(str::trim)
      .filter(|sha| !sha.is_empty())
      .filter(|sha| seen.insert((*sha).to_string()))
      .map(str::to_string)
      .collect::<Vec<_>>();
    self.get_commits_bulk(&shas)
  }

  /// Whether `ancestor` is reachable from `descendant`.
  pub(crate) fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
    self.run_git_check(&["merge-base", "--is-ancestor", ancestor, descendant])
  }

  /// Get files changed in a specific commit
  ///
  /// Returns list of (path, change_type) where change_type is A(dded), M(odified), D(eleted),
  /// R(enamed), or C(opied).
  ///
  /// For renames and copies, both the old and new paths are returned:
  /// - Old path with 'D' (deleted from old location)
  /// - New path with 'A' (added at new location)
  pub fn get_changed_files(&self, commit_sha: &str) -> RailResult<Vec<(PathBuf, char)>> {
    let output = self.run_git(&["diff-tree", "--no-commit-id", "--name-status", "-r", "-z", commit_sha])?;
    parse_name_status_output_z(&output.stdout)
  }

  /// Get per-commit path changes through one bounded `diff-tree --stdin` stream.
  pub(crate) fn get_changed_files_bulk(&self, commit_shas: &[String]) -> RailResult<Vec<Vec<(PathBuf, char)>>> {
    use std::io::Write as _;
    use std::process::Stdio;

    if commit_shas.is_empty() {
      return Ok(Vec::new());
    }
    crate::instrumentation::record_git_path_change_batch(commit_shas.len());
    let mut command = self.git_cmd();
    command
      .args([
        "diff-tree",
        "--stdin",
        "--root",
        "--no-renames",
        "--name-status",
        "-r",
        "-z",
        "--format=cargo-rail-commit:%H",
      ])
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped());
    let mut child = command.spawn().context("Failed to start batched git diff-tree")?;
    let mut stdin = child
      .stdin
      .take()
      .ok_or_else(|| RailError::message("git diff-tree stdin was unavailable"))?;
    for commit in commit_shas {
      stdin
        .write_all(commit.as_bytes())
        .and_then(|()| stdin.write_all(b"\n"))
        .context("Failed to write batched git diff-tree input")?;
    }
    drop(stdin);
    let output = child
      .wait_with_output()
      .context("Failed to finish batched git diff-tree")?;
    if !output.status.success() {
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git diff-tree --stdin".to_string(),
        stderr: git_command_diagnostics(&output.stdout, &output.stderr),
      }));
    }

    let records = output
      .stdout
      .split(|byte| *byte == 0)
      .filter(|record| !record.is_empty())
      .collect::<Vec<_>>();
    let mut index = 0;
    let mut results = Vec::with_capacity(commit_shas.len());
    for expected in commit_shas {
      let commit = records
        .get(index)
        .ok_or_else(|| RailError::message(format!("git diff-tree omitted commit '{expected}'")))?;
      let expected_marker = format!("cargo-rail-commit:{expected}");
      if *commit != expected_marker.as_bytes() {
        return Err(RailError::message(format!(
          "git diff-tree returned commit '{}' while '{}' was expected",
          String::from_utf8_lossy(commit),
          expected
        )));
      }
      index += 1;
      let start = index;
      while index < records.len() && !records[index].starts_with(b"cargo-rail-commit:") {
        if index + 1 >= records.len() {
          return Err(RailError::message(
            "git diff-tree returned an incomplete status/path pair",
          ));
        }
        index += 2;
      }
      results.push(parse_name_status_records(&records[start..index])?);
    }
    if index != records.len() {
      return Err(RailError::message("git diff-tree returned unexpected trailing records"));
    }
    Ok(results)
  }

  /// Get all files that changed between two refs.
  ///
  /// Returns list of (path, change_type) where change_type is A(dded), M(odified), D(eleted),
  /// R(enamed), or C(opied).
  ///
  /// For renames and copies, both the old and new paths are returned:
  /// - Old path with 'D' (deleted from old location)
  /// - New path with 'A' (added at new location)
  ///
  /// # Performance
  /// Uses `git diff --name-status` which is optimized for listing changes.
  /// Typically <100ms even for large diffs with 1000s of files.
  pub fn get_changed_files_between(&self, base_ref: &str, head_ref: Option<&str>) -> RailResult<Vec<(PathBuf, char)>> {
    let mut args = vec!["diff", "--name-status", "-z", "--end-of-options", base_ref];
    if let Some(head) = head_ref {
      args.push(head);
    }
    args.push("--");
    let output = self.run_git(&args)?;
    parse_name_status_output_z(&output.stdout)
  }

  /// Get the merge-base (common ancestor) between two refs
  ///
  /// This is useful for finding the point where a feature branch diverged
  /// from the main branch, which gives more accurate change detection in CI.
  pub fn get_merge_base(&self, ref1: &str, ref2: &str) -> RailResult<String> {
    let output = self.run_git(&["merge-base", ref1, ref2])?;
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
      return Err(crate::error::RailError::message(format!(
        "no common ancestor between {} and {}",
        ref1, ref2
      )));
    }
    Ok(sha)
  }

  /// Get commits touching a specific path in a range
  ///
  /// Returns commits in chronological order (oldest first).
  pub fn get_commits_touching_path(
    &self,
    path: &Path,
    since_sha: Option<&str>,
    until_ref: &str,
  ) -> RailResult<Vec<CommitInfo>> {
    let relative_path = self.normalize_path(path)?;
    let git_path = utils::path_to_git_format(&relative_path);

    let mut args = vec!["log", "--reverse", "--format=%H"];
    let range_arg;

    // Add range
    if let Some(since) = since_sha {
      range_arg = format!("{}..{}", since, until_ref);
      args.push(&range_arg);
    } else {
      args.push(until_ref);
    }

    // Add path filter
    args.push("--");
    args.push(&git_path);

    let output = self.run_git(&args)?;
    let shas: Vec<String> = String::from_utf8_lossy(&output.stdout)
      .lines()
      .map(|s| s.trim().to_string())
      .filter(|s| !s.is_empty())
      .collect();

    self.get_commits_bulk(&shas)
  }

  /// Get commits touching any of the given paths (batched for performance)
  /// Returns commits in chronological order (oldest first), deduplicated
  pub fn get_commits_touching_paths(
    &self,
    paths: &[PathBuf],
    since_sha: Option<&str>,
    until_ref: &str,
  ) -> RailResult<Vec<CommitInfo>> {
    if paths.is_empty() {
      return Ok(Vec::new());
    }

    // Normalize all paths
    let relative_paths: Vec<String> = paths
      .iter()
      .map(|path| {
        self
          .normalize_path(path)
          .map(|relative| utils::path_to_git_format(&relative))
      })
      .collect::<RailResult<_>>()?;

    let mut args = vec!["log", "--reverse", "--format=%H"];
    let range_arg;

    // Add range
    if let Some(since) = since_sha {
      range_arg = format!("{}..{}", since, until_ref);
      args.push(&range_arg);
    } else {
      args.push(until_ref);
    }

    // Add all path filters
    args.push("--");
    let path_refs: Vec<&str> = relative_paths.iter().map(|s| s.as_str()).collect();
    args.extend(path_refs);

    let output = self.run_git(&args)?;
    let shas: Vec<String> = String::from_utf8_lossy(&output.stdout)
      .lines()
      .map(|s| s.trim().to_string())
      .filter(|s| !s.is_empty())
      .collect();

    self.get_commits_bulk(&shas)
  }

  /// One-pass commit log with per-commit changed files
  ///
  /// Returns entries newest-first for `from..to` (or the full history of
  /// `to` when `from` is `None`), skipping merge commits. One git subprocess
  /// regardless of how many crates consume the result — this is the
  /// changelog-attribution workhorse.
  pub fn log_with_files(&self, from: Option<&str>, to: &str) -> RailResult<Vec<LogEntry>> {
    let range;
    let range_arg = if let Some(from) = from {
      range = format!("{}..{}", from, to);
      &range
    } else {
      to
    };

    let output = self.run_git(&[
      "log",
      "--no-merges",
      "-z",
      "--name-only",
      "--format=%x01%H%x02%s%x02%b%x02",
      range_arg,
    ])?;

    parse_log_with_files(&output.stdout)
  }

  /// Get commit metadata for a single SHA
  ///
  /// Uses `git log -1 --format` for efficient single-commit lookup.
  pub fn get_commit(&self, sha: &str) -> RailResult<CommitInfo> {
    // Format: %H (hash) %an (author name) %ae (author email) %at (author time)
    //         %cn (committer name) %ce (committer email) %ct (committer time)
    //         %P (parent hashes) %B (body)
    let format = "%H%n%an%n%ae%n%at%n%ai%n%cn%n%ce%n%ct%n%ci%n%P%n%B";
    let format_arg = format!("--format={}", format);

    let output = self
      .run_git(&["log", "-1", &format_arg, sha])
      .map_err(|error| match error {
        RailError::Git(GitError::CommandFailed { .. }) => {
          RailError::Git(GitError::CommitNotFound { sha: sha.to_string() })
        }
        other => other,
      })?;

    parse_commit_output(&output.stdout)
  }

  /// List all files at a specific commit under a path
  pub fn list_files_at_commit(&self, commit_sha: &str, path: &Path) -> RailResult<Vec<PathBuf>> {
    let spec = if path.as_os_str().is_empty() {
      commit_sha.to_string()
    } else {
      let git_path = utils::path_to_git_format(path);
      format!("{}:{}", commit_sha, git_path)
    };

    // Use run_git_check since failure is not an error (empty result)
    if !self.run_git_check(&["ls-tree", "-r", "--name-only", &spec]) {
      return Ok(vec![]);
    }

    // If successful, get the output
    let output = self.run_git(&["ls-tree", "-r", "--name-only", &spec])?;
    let files = String::from_utf8_lossy(&output.stdout)
      .lines()
      .map(PathBuf::from)
      .collect();

    Ok(files)
  }

  /// Collect all files from a tree recursively
  ///
  /// Uses bulk file reading for 100x+ speedup on large trees.
  pub fn collect_tree_files(&self, commit_sha: &str, path: &Path) -> RailResult<Vec<(PathBuf, Vec<u8>)>> {
    let files = self.list_files_at_commit(commit_sha, path)?;

    if files.is_empty() {
      return Ok(vec![]);
    }

    // Build paths first (owned), then create references for bulk API
    let paths: Vec<PathBuf> = files.iter().map(|file| path.join(file)).collect();
    let items: Vec<(&str, &Path)> = paths.iter().map(|p| (commit_sha, p.as_path())).collect();

    // Read all files in one batch (100x+ faster than loop)
    let contents = self.read_files_bulk(&items)?;

    // Combine full paths (with crate prefix) with contents
    let results: Vec<(PathBuf, Vec<u8>)> = paths.into_iter().zip(contents).collect();

    Ok(results)
  }

  /// Read exact tree entries under `path` without materializing the worktree.
  pub(crate) fn collect_tree_entries(&self, commit_sha: &str, path: &Path) -> RailResult<Vec<GitTreeEntry>> {
    self.collect_tree_entries_for_paths(commit_sha, &[path.to_path_buf()])
  }

  /// Collect exact entries for many paths in one `ls-tree` subprocess.
  pub(crate) fn collect_tree_entries_for_paths(
    &self,
    commit_sha: &str,
    paths: &[PathBuf],
  ) -> RailResult<Vec<GitTreeEntry>> {
    if paths.is_empty() {
      return Ok(Vec::new());
    }
    let normalized = paths
      .iter()
      .map(|path| self.normalize_path(path).map(|path| utils::path_to_git_format(&path)))
      .collect::<RailResult<Vec<_>>>()?;
    let mut command = self.git_cmd();
    command.args(["ls-tree", "-r", "-z", "--full-tree", commit_sha, "--"]);
    command.args(&normalized);
    let output = command.output().context("Failed to collect exact Git tree entries")?;
    if !output.status.success() {
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git ls-tree".to_string(),
        stderr: git_command_diagnostics(&output.stdout, &output.stderr),
      }));
    }
    let mut entries = Vec::new();
    for record in output
      .stdout
      .split(|byte| *byte == 0)
      .filter(|record| !record.is_empty())
    {
      let tab = record
        .iter()
        .position(|byte| *byte == b'\t')
        .ok_or_else(|| RailError::message("invalid git ls-tree record: missing path separator"))?;
      let metadata = String::from_utf8_lossy(&record[..tab]);
      let mut fields = metadata.split_whitespace();
      let mode = fields
        .next()
        .ok_or_else(|| RailError::message("invalid git ls-tree record: missing mode"))?;
      let _kind = fields
        .next()
        .ok_or_else(|| RailError::message("invalid git ls-tree record: missing object kind"))?;
      let object_id = fields
        .next()
        .ok_or_else(|| RailError::message("invalid git ls-tree record: missing object ID"))?;
      if fields.next().is_some() {
        return Err(RailError::message("invalid git ls-tree record: unexpected metadata"));
      }
      let path =
        std::str::from_utf8(&record[tab + 1..]).map_err(|_| RailError::message("git tree path is not valid UTF-8"))?;
      entries.push(GitTreeEntry {
        mode: mode.to_string(),
        object_id: object_id.to_string(),
        path: PathBuf::from(path),
      });
    }
    Ok(entries)
  }

  /// Import one commit's object closure from another local repository.
  pub(crate) fn import_objects(&self, source_repo: &Path, commit: &str) -> RailResult<()> {
    let source = source_repo
      .to_str()
      .ok_or_else(|| RailError::message("Git source repository path is not valid UTF-8"))?;
    self.run_git(&["fetch", "--quiet", "--no-tags", source, commit])?;
    Ok(())
  }

  /// Write one blob into this repository's object database.
  pub(crate) fn write_blob(&self, content: &[u8]) -> RailResult<String> {
    use std::io::Write as _;
    use std::process::Stdio;

    let mut command = self.git_cmd();
    command
      .args(["hash-object", "-w", "--stdin"])
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped());
    let mut child = command.spawn().context("Failed to start git hash-object")?;
    child
      .stdin
      .take()
      .ok_or_else(|| RailError::message("git hash-object stdin was unavailable"))?
      .write_all(content)
      .context("Failed to write Git blob")?;
    let output = child.wait_with_output().context("Failed to finish git hash-object")?;
    if !output.status.success() {
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git hash-object -w --stdin".to_string(),
        stderr: git_command_diagnostics(&output.stdout, &output.stderr),
      }));
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
  }

  /// Add a remote repository
  pub fn add_remote(&self, name: &str, url: &str) -> RailResult<()> {
    match self.run_git(&["remote", "add", name, url]) {
      Ok(_) => Ok(()),
      Err(e) => {
        // Check if error is because remote already exists
        if let RailError::Git(GitError::CommandFailed { stderr, .. }) = &e
          && stderr.contains("already exists")
        {
          return Ok(());
        }
        Err(e)
      }
    }
  }

  /// List all remotes
  pub fn list_remotes(&self) -> RailResult<Vec<(String, String)>> {
    // Use run_git_check since failure returns empty list
    if !self.run_git_check(&["remote", "-v"]) {
      return Ok(vec![]);
    }

    let output = self.run_git(&["remote", "-v"])?;
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
    progress!("   Pushing to remote '{}'...", remote_name);

    if let Err(error) = self.run_git_observable(&["push", "-u", remote_name, branch]) {
      return match error {
        RailError::Git(GitError::CommandFailed { stderr, .. }) => Err(RailError::Git(GitError::PushFailed {
          remote: remote_name.to_string(),
          branch: branch.to_string(),
          reason: stderr,
        })),
        other => Err(other),
      };
    }

    progress!("   ✅ Pushed to {}/{}", remote_name, branch);
    Ok(())
  }

  /// Fetch from remote
  pub fn fetch_from_remote(&self, remote_name: &str) -> RailResult<()> {
    progress!("   Fetching from remote '{}'...", remote_name);

    self.run_git(&["fetch", remote_name])?;

    progress!("   ✅ Fetched from {}", remote_name);
    Ok(())
  }

  /// Check if remote exists
  pub fn has_remote(&self, name: &str) -> RailResult<bool> {
    let remotes = self.list_remotes()?;
    Ok(remotes.iter().any(|(n, _)| n == name))
  }

  /// Create a branch
  pub fn create_branch(&self, branch_name: &str) -> RailResult<()> {
    self.run_git(&["branch", branch_name])?;
    Ok(())
  }

  /// Checkout a branch
  pub fn checkout_branch(&self, branch_name: &str) -> RailResult<()> {
    self.run_git_observable(&["checkout", branch_name])?;
    Ok(())
  }

  /// Check if a local branch exists
  pub fn branch_exists(&self, branch_name: &str) -> RailResult<bool> {
    let ref_name = format!("refs/heads/{}", branch_name);
    Ok(self.run_git_check(&["show-ref", "--verify", "--quiet", &ref_name]))
  }

  /// Create and checkout a branch
  pub fn create_and_checkout_branch(&self, branch_name: &str) -> RailResult<()> {
    self.create_branch(branch_name)?;
    self.checkout_branch(branch_name)?;
    Ok(())
  }

  /// Create a commit with specific metadata
  ///
  /// Creates a commit and returns its SHA.
  pub fn create_commit_with_metadata(
    &self,
    message: &str,
    metadata: &CommitMetadata,
    parent_shas: &[String],
    changed_paths: &[PathBuf],
  ) -> RailResult<String> {
    // Stage only the paths explicitly owned by this synthesized commit.
    self.stage_paths(changed_paths)?;

    // Write tree
    let tree_output = self.run_git(&["write-tree"])?;
    let tree_sha = String::from_utf8_lossy(&tree_output.stdout).trim().to_string();

    // Build commit-tree command (needs custom env vars, so we use git_cmd directly)
    let author_date = format!("{} {}", metadata.author_timestamp, metadata.author_timezone);
    let committer_date = format!("{} {}", metadata.committer_timestamp, metadata.committer_timezone);
    let mut cmd = self.git_cmd();
    cmd
      .env("GIT_AUTHOR_NAME", &metadata.author)
      .env("GIT_AUTHOR_EMAIL", &metadata.author_email)
      .env("GIT_AUTHOR_DATE", &author_date)
      .env("GIT_COMMITTER_NAME", &metadata.committer)
      .env("GIT_COMMITTER_EMAIL", &metadata.committer_email)
      .env("GIT_COMMITTER_DATE", &committer_date)
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
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git commit-tree".to_string(),
        stderr: git_command_diagnostics(&output.stdout, &output.stderr),
      }));
    }

    let commit_sha = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Update HEAD
    self.run_git(&["reset", "--soft", &commit_sha])?;

    Ok(commit_sha)
  }

  /// Create a commit from exact Git index changes without staging ambient work.
  pub(crate) fn create_commit_with_index_changes(
    &self,
    message: &str,
    metadata: &CommitMetadata,
    parent_shas: &[String],
    changes: &[GitIndexChange],
  ) -> RailResult<String> {
    use std::io::Write as _;
    use std::process::Stdio;

    let expected_head = self.head_commit()?;
    if !parent_shas.contains(&expected_head) {
      return Err(RailError::message(
        "exact Git commit parent does not match the current repository head",
      ));
    }
    let temporary = tempfile::Builder::new()
      .prefix("cargo-rail-index-")
      .tempfile()
      .context("Failed to allocate temporary Git index")?
      .into_temp_path();
    std::fs::remove_file(&temporary).context("Failed to initialize temporary Git index path")?;

    let mut read_tree = self.git_cmd();
    let output = read_tree
      .env("GIT_INDEX_FILE", &temporary)
      .args(["read-tree", &expected_head])
      .output()
      .context("Failed to seed exact Git index")?;
    if !output.status.success() {
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git read-tree".to_string(),
        stderr: git_command_diagnostics(&output.stdout, &output.stderr),
      }));
    }

    let mut command = self.git_cmd();
    command
      .env("GIT_INDEX_FILE", &temporary)
      .args(["update-index", "-z", "--index-info"])
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped());
    let mut child = command.spawn().context("Failed to start exact Git index update")?;
    let mut stdin = child
      .stdin
      .take()
      .ok_or_else(|| RailError::message("git update-index stdin was unavailable"))?;
    let zero_id = "0".repeat(expected_head.len());
    let mut changed_paths = Vec::with_capacity(changes.len());
    let mut upsert_paths = Vec::with_capacity(changes.len());
    for change in changes {
      let (mode, object_id, path) = match change {
        GitIndexChange::Upsert(entry) => (&*entry.mode, &*entry.object_id, &entry.path),
        GitIndexChange::Delete(path) => ("0", &*zero_id, path),
      };
      let path = self.normalize_path(path)?;
      let path = path
        .to_str()
        .ok_or_else(|| RailError::message(format!("Git index path '{}' is not UTF-8", path.display())))?
        .replace('\\', "/");
      write!(stdin, "{} {}\t{}\0", mode, object_id, path).context("Failed to write exact Git index entry")?;
      if matches!(change, GitIndexChange::Upsert(_)) {
        upsert_paths.push(path.clone());
      }
      changed_paths.push(path);
    }
    drop(stdin);
    let output = child
      .wait_with_output()
      .context("Failed to finish exact Git index update")?;
    if !output.status.success() {
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git update-index --index-info".to_string(),
        stderr: git_command_diagnostics(&output.stdout, &output.stderr),
      }));
    }

    let output = self
      .git_cmd()
      .env("GIT_INDEX_FILE", &temporary)
      .arg("write-tree")
      .output()
      .context("Failed to write exact Git tree")?;
    if !output.status.success() {
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git write-tree".to_string(),
        stderr: git_command_diagnostics(&output.stdout, &output.stderr),
      }));
    }
    let tree = String::from_utf8(output.stdout)?.trim().to_string();
    let author_date = format!("{} {}", metadata.author_timestamp, metadata.author_timezone);
    let committer_date = format!("{} {}", metadata.committer_timestamp, metadata.committer_timezone);
    let mut command = self.git_cmd();
    command
      .env("GIT_AUTHOR_NAME", &metadata.author)
      .env("GIT_AUTHOR_EMAIL", &metadata.author_email)
      .env("GIT_AUTHOR_DATE", &author_date)
      .env("GIT_COMMITTER_NAME", &metadata.committer)
      .env("GIT_COMMITTER_EMAIL", &metadata.committer_email)
      .env("GIT_COMMITTER_DATE", &committer_date)
      .arg("commit-tree")
      .arg(&tree)
      .arg("-m")
      .arg(message);
    for parent in parent_shas {
      command.arg("-p").arg(parent);
    }
    let output = command.output().context("Failed to create exact Git commit")?;
    if !output.status.success() {
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git commit-tree".to_string(),
        stderr: git_command_diagnostics(&output.stdout, &output.stderr),
      }));
    }
    let commit = String::from_utf8(output.stdout)?.trim().to_string();
    self.run_git(&["update-ref", "HEAD", &commit, &expected_head])?;

    if !changed_paths.is_empty() {
      let mut reset = self.git_cmd();
      reset.args(["reset", "-q", &commit, "--"]);
      reset.args(&changed_paths);
      let output = reset.output().context("Failed to align exact Git index paths")?;
      if !output.status.success() {
        return Err(RailError::Git(GitError::CommandFailed {
          command: "git reset -- <exact-paths>".to_string(),
          stderr: git_command_diagnostics(&output.stdout, &output.stderr),
        }));
      }
      if !upsert_paths.is_empty() {
        let mut checkout = self.git_cmd();
        checkout.args(["checkout-index", "-f", "--"]);
        checkout.args(&upsert_paths);
        let output = checkout.output().context("Failed to materialize exact Git paths")?;
        if !output.status.success() {
          return Err(RailError::Git(GitError::CommandFailed {
            command: "git checkout-index -- <exact-paths>".to_string(),
            stderr: git_command_diagnostics(&output.stdout, &output.stderr),
          }));
        }
      }
      for path in changes.iter().filter_map(|change| match change {
        GitIndexChange::Delete(path) => Some(path),
        GitIndexChange::Upsert(_) => None,
      }) {
        let path = self.worktree_root.join(self.normalize_path(path)?);
        match std::fs::symlink_metadata(&path) {
          Ok(metadata) if metadata.file_type().is_dir() => {
            return Err(RailError::message(format!(
              "refusing to remove directory '{}' for an exact file deletion",
              path.display()
            )));
          }
          Ok(_) => std::fs::remove_file(&path)?,
          Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
          Err(error) => return Err(error.into()),
        }
      }
    }
    Ok(commit)
  }

  /// Read multiple files in bulk using git cat-file --batch
  ///
  /// This is 100x+ faster than calling read_file_at_commit in a loop.
  /// Uses a single subprocess with `git cat-file --batch` to read all files.
  ///
  /// Used by `collect_tree_files` for optimal performance.
  ///
  /// # Performance
  /// - Single subprocess call (vs N calls for N files)
  /// - Can read 1000+ files in <500ms
  /// - Processes files in parallel chunks using rayon
  ///
  /// Output order matches input order. Missing files produce empty byte vectors.
  pub fn read_files_bulk(&self, items: &[(&str, &Path)]) -> RailResult<Vec<Vec<u8>>> {
    if items.is_empty() {
      return Ok(vec![]);
    }

    let mut requests = Vec::with_capacity(items.len());
    for (commit_sha, path) in items {
      let relative_path = self.normalize_path(path)?;
      let git_path = utils::path_to_git_format(&relative_path);
      requests.push(format!("{}:{}", commit_sha, git_path));
    }
    self.read_batch_objects(&requests, true, b"blob")
  }

  /// Read exact blob object IDs in one `git cat-file --batch` subprocess.
  pub(crate) fn read_blobs_bulk(&self, object_ids: &[&str]) -> RailResult<Vec<Vec<u8>>> {
    let requests: Vec<String> = object_ids.iter().map(|object_id| (*object_id).to_string()).collect();
    self.read_batch_objects(&requests, false, b"blob")
  }

  fn read_batch_objects(
    &self,
    requests: &[String],
    missing_as_empty: bool,
    expected_type: &[u8],
  ) -> RailResult<Vec<Vec<u8>>> {
    use std::io::Write as _;
    use std::process::Stdio;

    if requests.is_empty() {
      return Ok(Vec::new());
    }
    crate::instrumentation::record_git_object_read_batch(requests.len());

    let mut command = self.git_cmd();
    command
      .args(["cat-file", "--batch"])
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped());
    let mut child = command.spawn().context("Failed to spawn git cat-file")?;

    let mut stdin = child
      .stdin
      .take()
      .ok_or_else(|| RailError::message("Failed to open stdin"))?;

    for request in requests {
      stdin
        .write_all(request.as_bytes())
        .and_then(|()| stdin.write_all(b"\n"))
        .context("Failed to write to git cat-file stdin")?;
    }

    drop(stdin);
    let output = child.wait_with_output().context("Failed to read git cat-file output")?;
    if !output.status.success() {
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git cat-file --batch".to_string(),
        stderr: git_command_diagnostics(&output.stdout, &output.stderr),
      }));
    }

    let mut results = Vec::with_capacity(requests.len());
    let stdout = &output.stdout[..];
    let mut pos = 0;
    for request in requests {
      let line_end = stdout[pos..]
        .iter()
        .position(|&b| b == b'\n')
        .ok_or_else(|| RailError::message("Invalid cat-file output: missing newline"))?;
      let header = &stdout[pos..pos + line_end];
      pos += line_end + 1;

      if header.ends_with(b" missing") {
        if missing_as_empty {
          results.push(Vec::new());
          continue;
        }
        return Err(RailError::message(format!("Git object '{}' is missing", request)));
      }

      let mut parts = header.split(|&b| b == b' ');
      let _object_id = parts.next();
      let object_type = parts.next();
      let size = parts.next();
      if parts.next().is_some() || object_type != Some(expected_type) {
        return Err(RailError::message(format!(
          "Invalid cat-file header: {}",
          String::from_utf8_lossy(header)
        )));
      }
      let size = size.ok_or_else(|| RailError::message("Invalid cat-file header: missing object size"))?;
      let size_str = String::from_utf8_lossy(size);
      let size: usize = size_str
        .parse()
        .map_err(|_| RailError::message(format!("Invalid size in cat-file output: {}", size_str)))?;

      if pos + size > stdout.len() {
        return Err(RailError::message("Unexpected end of cat-file output"));
      }
      let content = stdout[pos..pos + size].to_vec();
      pos += size;
      if stdout.get(pos) != Some(&b'\n') {
        return Err(RailError::message(
          "Invalid cat-file output: missing content terminator",
        ));
      }
      pos += 1;
      results.push(content);
    }
    Ok(results)
  }

  /// Get multiple commits through one bounded `cat-file --batch` stream.
  ///
  /// Used by history and path-filtered walks so commit count does not become
  /// subprocess count. Output order matches input SHA order.
  ///
  /// # Performance
  /// - One subprocess for any number of commits
  /// - Memory bounded by returned metadata plus Git's stream output
  pub fn get_commits_bulk(&self, shas: &[String]) -> RailResult<Vec<CommitInfo>> {
    let objects = self.read_batch_objects(shas, false, b"commit")?;
    shas
      .iter()
      .zip(objects)
      .map(|(sha, object)| parse_raw_commit(sha, &object))
      .collect()
  }

  /// Resolve a git reference (tag, branch) to a commit SHA
  pub fn resolve_reference(&self, ref_name: &str) -> RailResult<String> {
    self.run_git_stdout(&["rev-parse", ref_name])
  }
}

/// Parse `git diff --name-status -z` output into (path, change_type) pairs.
///
/// Handles:
/// - Simple changes: `M\0path\0` (Modified), `A\0path\0` (Added), `D\0path\0` (Deleted)
/// - Renames: `R100\0old_path\0new_path\0` - emits both (old_path, 'D') and (new_path, 'A')
/// - Copies: `C100\0src_path\0dest_path\0` - emits both (src_path, 'M') and (dest_path, 'A')
/// - Paths with spaces/newlines/tabs: handled safely via NUL separation
///
/// The status codes from git are:
/// - A: Added
/// - C: Copied (followed by similarity percentage)
/// - D: Deleted
/// - M: Modified
/// - R: Renamed (followed by similarity percentage)
/// - T: Type changed
/// - U: Unmerged
/// - X: Unknown
///
/// One commit from [`SystemGit::log_with_files`]
#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
  /// Full commit SHA
  pub sha: String,
  /// Commit subject line
  pub subject: String,
  /// Commit body (trimmed), if any
  pub body: Option<String>,
  /// Paths changed by this commit, relative to the repository root
  pub files: Vec<PathBuf>,
}

/// Parse `git log -z --name-only --format=%x01%H%x02%s%x02%b%x02` output
///
/// Record layout: `\x01` + sha + `\x02` + subject + `\x02` + body + `\x02` +
/// NUL (+ `\n` when files follow) + NUL-terminated file paths. Bodies may
/// contain newlines; the trailing `\x02` before the NUL delimits them.
fn parse_log_with_files(output: &[u8]) -> RailResult<Vec<LogEntry>> {
  let text = String::from_utf8_lossy(output);
  let mut entries = Vec::new();

  for record in text.split('\x01') {
    if record.is_empty() {
      continue;
    }

    let Some((sha, rest)) = record.split_once('\x02') else {
      continue;
    };
    let Some((subject, rest)) = rest.split_once('\x02') else {
      continue;
    };
    // The body may contain any text except \x02; the last \x02 in the
    // record closes it and the file list follows.
    let Some((body_raw, files_raw)) = rest.rsplit_once('\x02') else {
      continue;
    };

    let body = body_raw.trim();
    let files = files_raw
      .trim_start_matches(['\0', '\n'])
      .split('\0')
      .filter(|f| !f.is_empty())
      .map(PathBuf::from)
      .collect();

    entries.push(LogEntry {
      sha: sha.trim().to_string(),
      subject: subject.trim().to_string(),
      body: (!body.is_empty()).then(|| body.to_string()),
      files,
    });
  }

  Ok(entries)
}

fn parse_name_status_output_z(output: &[u8]) -> RailResult<Vec<(PathBuf, char)>> {
  let mut files = Vec::new();

  let mut parts = output.split(|&b| b == 0);
  while let Some(status_bytes) = parts.next() {
    if status_bytes.is_empty() {
      continue;
    }

    let status = String::from_utf8_lossy(status_bytes);
    let change_type = status.chars().next().unwrap_or('M');

    let mut next_path = || parts.next().filter(|p| !p.is_empty());

    match change_type {
      'R' => {
        // Rename: R100\0old_path\0new_path\0
        // Treat as: delete from old location, add at new location
        let Some(old_path) = next_path() else {
          continue;
        };
        let Some(new_path) = next_path() else {
          // Fallback: if only one path, treat as modified
          files.push((PathBuf::from(String::from_utf8_lossy(old_path).into_owned()), 'M'));
          continue;
        };

        files.push((PathBuf::from(String::from_utf8_lossy(old_path).into_owned()), 'D'));
        files.push((PathBuf::from(String::from_utf8_lossy(new_path).into_owned()), 'A'));
      }
      'C' => {
        // Copy: C100\0src_path\0dest_path\0
        // Source still exists (mark as touched), dest is new
        let Some(src_path) = next_path() else {
          continue;
        };
        let Some(dest_path) = next_path() else {
          files.push((PathBuf::from(String::from_utf8_lossy(src_path).into_owned()), 'A'));
          continue;
        };

        files.push((PathBuf::from(String::from_utf8_lossy(src_path).into_owned()), 'M'));
        files.push((PathBuf::from(String::from_utf8_lossy(dest_path).into_owned()), 'A'));
      }
      'A' | 'D' | 'M' | 'T' | 'U' => {
        let Some(path) = next_path() else {
          continue;
        };
        files.push((PathBuf::from(String::from_utf8_lossy(path).into_owned()), change_type));
      }
      _ => {
        // Unknown status - treat as modified if we have a path
        let Some(path) = next_path() else {
          continue;
        };
        files.push((PathBuf::from(String::from_utf8_lossy(path).into_owned()), 'M'));
      }
    }
  }

  Ok(files)
}

fn parse_name_status_records(records: &[&[u8]]) -> RailResult<Vec<(PathBuf, char)>> {
  if !records.len().is_multiple_of(2) {
    return Err(RailError::message(
      "git diff-tree returned an incomplete status/path pair",
    ));
  }
  records
    .chunks_exact(2)
    .map(|pair| {
      let status_record = pair[0].strip_prefix(b"\n").unwrap_or(pair[0]);
      let status = status_record
        .first()
        .copied()
        .map(char::from)
        .ok_or_else(|| RailError::message("git diff-tree returned an empty change status"))?;
      let change_type = match status {
        'A' | 'D' | 'M' | 'T' | 'U' => status,
        _ => 'M',
      };
      Ok((
        PathBuf::from(String::from_utf8_lossy(pair[1]).into_owned()),
        change_type,
      ))
    })
    .collect()
}

/// Parse git log output into CommitInfo
///
/// Format is %H%n%an%n%ae%n%at%n%ai%n%cn%n%ce%n%ct%n%ci%n%P%n%B
/// Which gives us: hash, author name, author email, author time,
///                 committer name, committer email, committer time,
///                 parent hashes, body
fn parse_commit_output(data: &[u8]) -> RailResult<CommitInfo> {
  let output = String::from_utf8_lossy(data);
  let mut lines = output.lines();

  let sha = lines
    .next()
    .ok_or_else(|| RailError::message("Missing commit SHA"))?
    .to_string();
  let author = lines
    .next()
    .ok_or_else(|| RailError::message("Missing author name"))?
    .to_string();
  let author_email = lines
    .next()
    .ok_or_else(|| RailError::message("Missing author email"))?
    .to_string();
  let timestamp = lines
    .next()
    .and_then(|s| s.parse::<i64>().ok())
    .ok_or_else(|| RailError::message("Missing/invalid author timestamp"))?;
  let author_timezone =
    parse_formatted_timezone(lines.next().ok_or_else(|| RailError::message("Missing author date"))?)?;
  let committer = lines
    .next()
    .ok_or_else(|| RailError::message("Missing committer name"))?
    .to_string();
  let committer_email = lines
    .next()
    .ok_or_else(|| RailError::message("Missing committer email"))?
    .to_string();
  let committer_timestamp = lines
    .next()
    .and_then(|s| s.parse::<i64>().ok())
    .ok_or_else(|| RailError::message("Missing/invalid committer timestamp"))?;
  let committer_timezone = parse_formatted_timezone(
    lines
      .next()
      .ok_or_else(|| RailError::message("Missing committer date"))?,
  )?;
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
    author_timezone,
    committer_timestamp,
    committer_timezone,
    parent_shas,
  })
}

fn parse_raw_commit(sha: &str, data: &[u8]) -> RailResult<CommitInfo> {
  let separator = data
    .windows(2)
    .position(|window| window == b"\n\n")
    .ok_or_else(|| RailError::message(format!("Git commit '{}' has no message separator", sha)))?;
  let headers = std::str::from_utf8(&data[..separator])
    .map_err(|_| RailError::message(format!("Git commit '{}' has non-UTF-8 headers", sha)))?;
  let mut author = None;
  let mut committer = None;
  let mut parent_shas = Vec::new();
  for line in headers.lines() {
    if let Some(parent) = line.strip_prefix("parent ") {
      validate_batch_object_id(parent)?;
      parent_shas.push(parent.to_string());
    } else if let Some(signature) = line.strip_prefix("author ") {
      author = Some(parse_raw_signature(signature, "author")?);
    } else if let Some(signature) = line.strip_prefix("committer ") {
      committer = Some(parse_raw_signature(signature, "committer")?);
    }
  }
  let (author, author_email, timestamp, author_timezone) =
    author.ok_or_else(|| RailError::message(format!("Git commit '{}' has no author", sha)))?;
  let (committer, committer_email, committer_timestamp, committer_timezone) =
    committer.ok_or_else(|| RailError::message(format!("Git commit '{}' has no committer", sha)))?;
  let raw_message = String::from_utf8_lossy(&data[separator + 2..]);
  let message = raw_message.strip_suffix('\n').unwrap_or(&raw_message).to_string();

  Ok(CommitInfo {
    sha: sha.to_string(),
    author,
    author_email,
    committer,
    committer_email,
    message,
    timestamp,
    author_timezone,
    committer_timestamp,
    committer_timezone,
    parent_shas,
  })
}

fn parse_raw_signature(signature: &str, field: &str) -> RailResult<(String, String, i64, String)> {
  let mut suffix = signature.rsplitn(3, ' ');
  let timezone = suffix
    .next()
    .ok_or_else(|| RailError::message(format!("Git {} has no time-zone offset", field)))?;
  validate_timezone(timezone)?;
  let timestamp = suffix
    .next()
    .and_then(|value| value.parse::<i64>().ok())
    .ok_or_else(|| RailError::message(format!("Git {} has an invalid timestamp", field)))?;
  let identity = suffix
    .next()
    .ok_or_else(|| RailError::message(format!("Git {} has no identity", field)))?;
  let email_end = identity
    .strip_suffix('>')
    .ok_or_else(|| RailError::message(format!("Git {} email is malformed", field)))?;
  let (name, email) = email_end
    .rsplit_once(" <")
    .ok_or_else(|| RailError::message(format!("Git {} identity is malformed", field)))?;
  Ok((name.to_string(), email.to_string(), timestamp, timezone.to_string()))
}

fn validate_batch_object_id(value: &str) -> RailResult<()> {
  if matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
    Ok(())
  } else {
    Err(RailError::message(format!(
      "Git commit has invalid parent object ID '{}'",
      value
    )))
  }
}

fn parse_formatted_timezone(date: &str) -> RailResult<String> {
  let timezone = date
    .split_whitespace()
    .next_back()
    .ok_or_else(|| RailError::message("Git date has no time-zone offset"))?;
  validate_timezone(timezone)?;
  Ok(timezone.to_string())
}

fn validate_timezone(timezone: &str) -> RailResult<()> {
  if timezone.len() == 5
    && matches!(timezone.as_bytes().first(), Some(b'+' | b'-'))
    && timezone.as_bytes()[1..].iter().all(u8::is_ascii_digit)
  {
    return Ok(());
  }
  Err(RailError::message(format!(
    "Git date has invalid time-zone offset '{}'",
    timezone
  )))
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::env;

  /// Helper to get the git repository root
  fn find_git_root() -> PathBuf {
    env::current_dir().unwrap()
  }

  #[test]
  fn parse_log_with_files_round_trips_records() {
    // Two commits: one with a body and two files, one bodyless with one file.
    let raw = b"\x01aaaa1111\x02feat: add planner\x02Longer body\nwith newlines\x02\x00\nsrc/lib.rs\x00src/main.rs\x00\x01bbbb2222\x02fix: typo\x02\x02\x00\nREADME.md\x00";
    let entries = parse_log_with_files(raw).unwrap();

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].sha, "aaaa1111");
    assert_eq!(entries[0].subject, "feat: add planner");
    assert_eq!(entries[0].body.as_deref(), Some("Longer body\nwith newlines"));
    assert_eq!(
      entries[0].files,
      vec![PathBuf::from("src/lib.rs"), PathBuf::from("src/main.rs")]
    );
    assert_eq!(entries[1].sha, "bbbb2222");
    assert_eq!(entries[1].body, None);
    assert_eq!(entries[1].files, vec![PathBuf::from("README.md")]);
  }

  #[test]
  fn parse_log_with_files_handles_empty_file_lists() {
    // A commit with no files (e.g. empty commit) ends after the format NUL.
    let raw = b"\x01cccc3333\x02chore: empty\x02\x02\x00";
    let entries = parse_log_with_files(raw).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].files.is_empty());
  }

  #[test]
  fn log_with_files_reads_real_history() {
    let git = SystemGit::open(&find_git_root()).unwrap();
    let entries = git.log_with_files(None, "HEAD").unwrap();
    assert!(!entries.is_empty());
    let first = &entries[0];
    assert_eq!(first.sha.len(), 40);
    assert!(!first.subject.is_empty());
  }

  #[test]
  fn test_commit_history() {
    let git = SystemGit::open(&find_git_root()).unwrap();

    // Test with limit
    let commits = git.commit_history(Some(5)).unwrap();
    assert!(!commits.is_empty());
    assert!(commits.len() <= 5);

    // Check first commit has required fields
    let first = &commits[0];
    assert!(!first.sha.is_empty());
    assert_eq!(first.sha.len(), 40); // Full SHA
    assert!(!first.author.is_empty());
    assert!(first.timestamp > 0);
    assert!(!first.message.is_empty());
  }

  #[test]
  fn test_get_changed_files() {
    let git = SystemGit::open(&find_git_root()).unwrap();
    let head = git.head_commit().unwrap();

    // Get changed files for HEAD
    let changed = git.get_changed_files(&head).unwrap();

    // HEAD should have at least one changed file (unless it's the initial commit)
    // Just verify the call succeeds and returns valid data
    for (path, change_type) in changed {
      assert!(!path.as_os_str().is_empty());
      assert!(['A', 'M', 'D', 'R', 'C'].contains(&change_type));
    }
  }

  #[test]
  fn test_get_commits_bulk() {
    let git = SystemGit::open(&find_git_root()).unwrap();

    // Get some commit SHAs
    let history = git.commit_history(Some(5)).unwrap();
    let shas: Vec<String> = history.iter().map(|c| c.sha.clone()).collect();

    // Fetch them in bulk
    let commits = git.get_commits_bulk(&shas).unwrap();

    assert_eq!(commits.len(), shas.len());

    // Verify all commits match
    for (i, commit) in commits.iter().enumerate() {
      assert_eq!(commit.sha, shas[i]);
    }
  }

  #[test]
  fn test_read_files_bulk() {
    let git = SystemGit::open(&find_git_root()).unwrap();
    let head = git.head_commit().unwrap();

    // Prepare items to read
    let paths = [
      PathBuf::from("Cargo.toml"),
      PathBuf::from("README.md"),
      PathBuf::from("this-does-not-exist.txt"),
    ];
    let items: Vec<(&str, &Path)> = paths.iter().map(|p| (head.as_str(), p.as_path())).collect();

    let results = git.read_files_bulk(&items).unwrap();

    assert_eq!(results.len(), 3);

    // First two should have content (Cargo.toml and README.md exist)
    assert!(!results[0].is_empty(), "Cargo.toml should exist");
    assert!(!results[1].is_empty(), "README.md should exist");

    // Third should be empty (file doesn't exist)
    assert!(results[2].is_empty(), "Non-existent file should be empty");

    // Verify Cargo.toml content
    let cargo_toml = String::from_utf8_lossy(&results[0]);
    assert!(cargo_toml.contains("package") || cargo_toml.contains("dependencies"));
  }

  #[test]
  fn test_read_files_bulk_empty() {
    let git = SystemGit::open(&find_git_root()).unwrap();

    // Empty input should return empty output
    let results = git.read_files_bulk(&[]).unwrap();
    assert!(results.is_empty());
  }

  #[test]
  fn exact_index_changes_normalize_windows_separators() {
    let temp = tempfile::TempDir::new().unwrap();
    crate::git::init_repo(temp.path(), "main").unwrap();
    let git = SystemGit::open(temp.path()).unwrap();
    git.set_config("user.name", "Test User").unwrap();
    git.set_config("user.email", "test@example.com").unwrap();

    std::fs::create_dir(temp.path().join("nested")).unwrap();
    std::fs::write(temp.path().join("nested/file.txt"), "before\n").unwrap();
    git.stage_all().unwrap();
    git.commit("initial").unwrap();

    let parent = git.head_commit().unwrap();
    let metadata = git.get_commit(&parent).unwrap().metadata();
    let object_id = git.write_blob(b"after\n").unwrap();
    let commit = git
      .create_commit_with_index_changes(
        "update nested file",
        &metadata,
        std::slice::from_ref(&parent),
        &[GitIndexChange::Upsert(GitTreeEntry {
          mode: "100644".to_string(),
          object_id,
          path: PathBuf::from(r"nested\file.txt"),
        })],
      )
      .unwrap();

    let contents = git.read_files_bulk(&[(&commit, Path::new("nested/file.txt"))]).unwrap();
    assert_eq!(contents, vec![b"after\n".to_vec()]);
    assert!(temp.path().join("nested/file.txt").is_file());
    git.run_git(&["diff", "--quiet", "--", "nested/file.txt"]).unwrap();
  }

  #[test]
  fn test_get_commits_touching_path() {
    let git = SystemGit::open(&find_git_root()).unwrap();

    // Get commits that touched Cargo.toml
    let commits = git
      .get_commits_touching_path(Path::new("Cargo.toml"), None, "HEAD")
      .unwrap();

    // Cargo.toml should have been modified at least once
    assert!(!commits.is_empty(), "Cargo.toml should have commits");

    // Verify chronological order (oldest first)
    if commits.len() >= 2 {
      assert!(
        commits[0].timestamp <= commits[1].timestamp,
        "Commits should be in chronological order"
      );
    }
  }

  #[test]
  fn test_collect_tree_files_with_bulk() {
    let git = SystemGit::open(&find_git_root()).unwrap();
    let head = git.head_commit().unwrap();

    // Collect files from src/ directory at HEAD
    let files = git.collect_tree_files(&head, Path::new("src")).unwrap();

    // Should have at least main.rs and some core files
    assert!(!files.is_empty(), "src/ should contain files");

    // Verify all files have valid paths
    for (path, _content) in &files {
      assert!(!path.as_os_str().is_empty(), "Path should not be empty");
      // Most source files should have content (some may be empty but most won't be)
    }

    // Verify at least one Rust file exists with actual content
    let has_rust_with_content = files
      .iter()
      .any(|(path, content)| path.extension().and_then(|s| s.to_str()) == Some("rs") && !content.is_empty());
    assert!(has_rust_with_content, "Should have at least one .rs file with content");

    // Test with empty path (root directory) - should get all files
    let all_files = git.collect_tree_files(&head, Path::new("")).unwrap();
    assert!(
      all_files.len() >= files.len(),
      "Root should have at least as many files as src/"
    );
  }

  #[test]
  fn test_collect_tree_files_nonexistent() {
    let git = SystemGit::open(&find_git_root()).unwrap();
    let head = git.head_commit().unwrap();

    // Try to collect from non-existent directory
    let files = git
      .collect_tree_files(&head, Path::new("this-directory-does-not-exist-12345"))
      .unwrap();

    // Should return empty list for non-existent directory
    assert!(files.is_empty(), "Non-existent directory should return empty list");
  }

  #[test]
  fn test_parse_name_status_simple() {
    // Simple modifications
    let output = b"M\0src/main.rs\0A\0src/new.rs\0D\0src/old.rs\0";
    let result = parse_name_status_output_z(output).unwrap();

    assert_eq!(result.len(), 3);
    assert_eq!(result[0], (PathBuf::from("src/main.rs"), 'M'));
    assert_eq!(result[1], (PathBuf::from("src/new.rs"), 'A'));
    assert_eq!(result[2], (PathBuf::from("src/old.rs"), 'D'));
  }

  #[test]
  fn test_parse_name_status_rename() {
    // Rename: R100 (100% similarity) old_path -> new_path
    let output = b"R100\0src/old_name.rs\0src/new_name.rs\0";
    let result = parse_name_status_output_z(output).unwrap();

    // Should emit both: delete from old, add at new
    assert_eq!(result.len(), 2);
    assert_eq!(result[0], (PathBuf::from("src/old_name.rs"), 'D'));
    assert_eq!(result[1], (PathBuf::from("src/new_name.rs"), 'A'));
  }

  #[test]
  fn test_parse_name_status_copy() {
    // Copy: C100 (100% similarity) src_path -> dest_path
    let output = b"C095\0src/original.rs\0src/copied.rs\0";
    let result = parse_name_status_output_z(output).unwrap();

    // Should emit: source touched, dest added
    assert_eq!(result.len(), 2);
    assert_eq!(result[0], (PathBuf::from("src/original.rs"), 'M'));
    assert_eq!(result[1], (PathBuf::from("src/copied.rs"), 'A'));
  }

  #[test]
  fn test_parse_name_status_paths_with_spaces() {
    // Paths with spaces are handled by NUL separation
    let output = b"M\0path with spaces/file name.rs\0A\0another path/new file.txt\0";
    let result = parse_name_status_output_z(output).unwrap();

    assert_eq!(result.len(), 2);
    assert_eq!(result[0], (PathBuf::from("path with spaces/file name.rs"), 'M'));
    assert_eq!(result[1], (PathBuf::from("another path/new file.txt"), 'A'));
  }

  #[test]
  fn test_parse_name_status_rename_with_spaces() {
    // Rename with spaces in paths
    let output = b"R100\0old path/old file.rs\0new path/new file.rs\0";
    let result = parse_name_status_output_z(output).unwrap();

    assert_eq!(result.len(), 2);
    assert_eq!(result[0], (PathBuf::from("old path/old file.rs"), 'D'));
    assert_eq!(result[1], (PathBuf::from("new path/new file.rs"), 'A'));
  }

  #[test]
  fn test_parse_name_status_mixed() {
    // Mixed changes in one diff
    let output = b"M\0src/lib.rs\0R100\0src/old.rs\0src/renamed.rs\0A\0src/new.rs\0D\0src/deleted.rs\0";
    let result = parse_name_status_output_z(output).unwrap();

    assert_eq!(result.len(), 5);
    assert_eq!(result[0], (PathBuf::from("src/lib.rs"), 'M'));
    assert_eq!(result[1], (PathBuf::from("src/old.rs"), 'D')); // Rename source
    assert_eq!(result[2], (PathBuf::from("src/renamed.rs"), 'A')); // Rename dest
    assert_eq!(result[3], (PathBuf::from("src/new.rs"), 'A'));
    assert_eq!(result[4], (PathBuf::from("src/deleted.rs"), 'D'));
  }

  #[test]
  fn test_parse_name_status_empty() {
    let result = parse_name_status_output_z(b"").unwrap();
    assert!(result.is_empty());

    let result = parse_name_status_output_z(b"\0\0").unwrap();
    assert!(result.is_empty());
  }

  #[test]
  fn test_parse_name_status_type_change() {
    // Type change (e.g., file to symlink)
    let output = b"T\0src/link.rs\0";
    let result = parse_name_status_output_z(output).unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0], (PathBuf::from("src/link.rs"), 'T'));
  }
}
