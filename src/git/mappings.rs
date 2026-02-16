//! Git-notes based commit mapping storage.
//!
//! Maps commits between monorepo and split repos using git-notes.
//! Notes are stored at `refs/notes/rail/{crate_name}`.

use crate::error::{GitError, RailError, RailResult, ResultExt};
use crate::git::git_cmd_for_path;
use crate::{progress, warn};
use rustc_hash::FxHashMap;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::thread;
use std::time::Duration;

/// Commit mapping store using git-notes.
///
/// Maps commits between monorepo and split repos. Format in git-notes:
/// `refs/notes/rail/{crate_name}` where each note contains the target SHA.
pub struct MappingStore {
  crate_name: String,
  /// In-memory cache of mappings (from_sha -> to_sha)
  mappings: FxHashMap<String, String>,
  /// Reverse index for O(1) reverse lookups (to_sha -> from_sha)
  reverse_mappings: FxHashMap<String, String>,
}

impl MappingStore {
  /// Create a new mapping store for a specific crate.
  pub fn new(crate_name: String) -> Self {
    Self {
      crate_name,
      mappings: FxHashMap::default(),
      reverse_mappings: FxHashMap::default(),
    }
  }

  /// Load mappings from git-notes in a repository.
  ///
  /// Uses batch `cat-file` for efficient bulk reading of note contents.
  pub fn load(&mut self, repo_path: &Path) -> RailResult<()> {
    let notes_ref = format!("refs/notes/rail/{}", self.crate_name);

    // List all notes: git notes --ref=refs/notes/rail/{crate} list
    // Output format: "<blob_sha> <commit_sha>" per line
    let output = git_cmd_for_path(repo_path)
      .args(["notes", "--ref", &notes_ref, "list"])
      .output();

    // If command fails, notes ref doesn't exist yet - that's ok
    let output = match output {
      Ok(o) if o.status.success() => o,
      _ => return Ok(()), // No notes yet
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse notes list into (blob_sha, commit_sha) pairs
    let note_entries: Vec<(&str, &str)> = stdout
      .lines()
      .filter_map(|line| {
        let mut parts = line.split_whitespace();
        match (parts.next(), parts.next()) {
          (Some(blob_sha), Some(commit_sha)) => Some((blob_sha, commit_sha)),
          _ => None,
        }
      })
      .collect();

    if note_entries.is_empty() {
      return Ok(());
    }

    // Batch read all note blobs using cat-file --batch
    let blob_contents = read_blobs_batch(repo_path, &note_entries)?;

    // Build mappings from results
    for ((_, commit_sha), note_content) in note_entries.iter().zip(blob_contents.iter()) {
      if let Some(target_sha) = note_content {
        let from = (*commit_sha).to_string();
        let to = target_sha.clone();
        self.reverse_mappings.insert(to.clone(), from.clone());
        self.mappings.insert(from, to);
      }
    }

    Ok(())
  }

  /// Save mappings to git-notes in a repository.
  pub fn save(&self, repo_path: &Path) -> RailResult<()> {
    let notes_ref = format!("refs/notes/rail/{}", self.crate_name);

    // For each mapping, add a note
    for (source_sha, target_sha) in &self.mappings {
      let output = git_cmd_for_path(repo_path)
        .args(["notes", "--ref", &notes_ref, "add", "-f", "-m", target_sha, source_sha])
        .output()
        .context("Failed to add git note")?;

      if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "already has a note" errors
        if !stderr.contains("already has a note") {
          return Err(RailError::Git(GitError::CommandFailed {
            command: String::from("git notes add"),
            stderr: stderr.into_owned(),
          }));
        }
      }
    }

    Ok(())
  }

  /// Record a mapping between two commits.
  pub fn record_mapping(&mut self, from_sha: &str, to_sha: &str) -> RailResult<()> {
    let from = from_sha.to_string();
    let to = to_sha.to_string();
    self.reverse_mappings.insert(to.clone(), from.clone());
    self.mappings.insert(from, to);
    Ok(())
  }

  /// Get the mapped commit SHA if it exists.
  pub fn get_mapping(&self, sha: &str) -> RailResult<Option<String>> {
    Ok(self.mappings.get(sha).cloned())
  }

  /// Check if a commit has been mapped (forward direction).
  pub fn has_mapping(&self, sha: &str) -> bool {
    self.mappings.contains_key(sha)
  }

  /// Check if a commit exists as a mapping target (reverse direction).
  ///
  /// O(1) lookup using reverse index.
  pub fn has_reverse_mapping(&self, sha: &str) -> bool {
    self.reverse_mappings.contains_key(sha)
  }

  /// Clear all mappings (only used in tests).
  #[cfg(test)]
  pub fn clear(&mut self) {
    self.mappings.clear();
    self.reverse_mappings.clear();
  }

  /// Get the number of mappings currently loaded.
  pub fn count(&self) -> usize {
    self.mappings.len()
  }

  /// Push git-notes to a remote repository.
  pub fn push_notes(&self, repo_path: &Path, remote: &str) -> RailResult<()> {
    // Skip if no mappings exist
    if self.mappings.is_empty() {
      progress!("   No git-notes to push (no mappings recorded)");
      return Ok(());
    }

    let notes_ref = format!("refs/notes/rail/{}", self.crate_name);

    progress!("   Pushing git-notes to remote '{}'...", remote);

    retry_operation(|| {
      let output = git_cmd_for_path(repo_path)
        .args(["push", remote, &notes_ref])
        .output()
        .context("Failed to push git-notes")?;

      if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RailError::Git(GitError::CommandFailed {
          command: String::from("git push notes"),
          stderr: stderr.into_owned(),
        }));
      }
      Ok(())
    })?;

    progress!("   ✅ Pushed git-notes");
    Ok(())
  }

  /// Fetch git-notes from a remote repository.
  pub fn fetch_notes(&self, repo_path: &Path, remote: &str) -> RailResult<()> {
    let notes_ref = format!("refs/notes/rail/{}", self.crate_name);
    let refspec = format!("{}:{}", notes_ref, notes_ref);

    progress!("   Fetching git-notes from remote '{}'...", remote);

    // Retry the fetch operation
    let result = retry_operation(|| {
      let output = git_cmd_for_path(repo_path)
        .args(["fetch", remote, &refspec])
        .output()
        .context("Failed to fetch git-notes")?;

      if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Ignore "couldn't find remote ref" - notes may not exist yet
        if stderr.contains("couldn't find remote ref") {
          return Ok(()); // Treat as success (no notes yet)
        }

        return Err(RailError::Git(GitError::CommandFailed {
          command: String::from("git fetch notes"),
          stderr: stderr.into_owned(),
        }));
      }
      Ok(())
    });

    match result {
      Ok(()) => {
        progress!("   ✅ Fetched git-notes");
        Ok(())
      }
      Err(e) => {
        // Check if it's a conflict (non-fast-forward)
        let is_conflict = if let RailError::Git(GitError::CommandFailed { stderr, .. }) = &e {
          stderr.contains("non-fast-forward") || stderr.contains("rejected")
        } else {
          false
        };

        if is_conflict {
          self.handle_notes_conflict(repo_path, remote, &notes_ref)
        } else {
          Err(e)
        }
      }
    }
  }

  /// Handle git-notes conflict by attempting automatic merge with union strategy.
  fn handle_notes_conflict(&self, repo_path: &Path, remote: &str, notes_ref: &str) -> RailResult<()> {
    progress!("   ⚠️  Git-notes conflict detected (local and remote notes diverged)");
    progress!("   🔄 Attempting automatic merge with union strategy...");

    // Fetch to FETCH_HEAD without updating the ref
    let fetch_output = git_cmd_for_path(repo_path)
      .args(["fetch", remote, notes_ref])
      .output()
      .context("Failed to fetch notes to FETCH_HEAD")?;

    if !fetch_output.status.success() {
      let fetch_stderr = String::from_utf8_lossy(&fetch_output.stderr);
      if !fetch_stderr.contains("couldn't find remote ref") {
        return Err(RailError::Git(GitError::CommandFailed {
          command: String::from("git fetch notes to FETCH_HEAD"),
          stderr: fetch_stderr.into_owned(),
        }));
      }
      return Ok(()); // No remote notes
    }

    // Merge notes using union strategy (combines both without conflict)
    let merge_output = git_cmd_for_path(repo_path)
      .args(["notes", "--ref", notes_ref, "merge", "--strategy=union", "FETCH_HEAD"])
      .output()
      .context("Failed to merge git-notes")?;

    if !merge_output.status.success() {
      let merge_stderr = String::from_utf8_lossy(&merge_output.stderr);

      // If union merge fails, provide clear guidance
      warn!("   ❌ Automatic git-notes merge failed");
      warn!("   📋 Manual resolution required:");
      warn!("      1. cd {}", repo_path.display());
      warn!("      2. git notes --ref={} merge FETCH_HEAD", notes_ref);
      warn!("      3. Resolve conflicts manually");
      warn!("      4. git notes --ref={} merge --commit", notes_ref);
      warn!("");
      return Err(RailError::with_help(
        format!("git notes merge failed: {}", merge_stderr),
        format!(
          "This usually happens when the same commit has different mappings on different machines.\n\
           Manual resolution steps:\n  \
           1. cd {}\n  \
           2. git notes --ref={} merge FETCH_HEAD\n  \
           3. Resolve conflicts manually\n  \
           4. git notes --ref={} merge --commit",
          repo_path.display(),
          notes_ref,
          notes_ref
        ),
      ));
    }

    progress!("   ✅ Git-notes merged successfully (union strategy)");
    Ok(())
  }
}

/// Read multiple blob objects in a single `git cat-file --batch` call.
///
/// Takes a list of (blob_sha, commit_sha) pairs from `git notes list` output
/// and returns the content of each blob (the target SHA stored in the note).
///
/// Returns `None` for blobs that don't exist (shouldn't happen for valid notes).
fn read_blobs_batch(repo_path: &Path, entries: &[(&str, &str)]) -> RailResult<Vec<Option<String>>> {
  if entries.is_empty() {
    return Ok(vec![]);
  }

  // Start cat-file --batch process
  let mut child = git_cmd_for_path(repo_path)
    .args(["cat-file", "--batch"])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .context("Failed to spawn git cat-file")?;

  let mut stdin = child
    .stdin
    .take()
    .ok_or_else(|| RailError::message("Failed to open stdin for cat-file"))?;

  // Write all blob SHAs to stdin
  for (blob_sha, _commit_sha) in entries {
    writeln!(stdin, "{}", blob_sha).context("Failed to write to git cat-file stdin")?;
  }

  drop(stdin); // Close stdin to signal we're done

  // Read output
  let output = child.wait_with_output().context("Failed to read git cat-file output")?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    return Err(RailError::Git(GitError::CommandFailed {
      command: String::from("git cat-file --batch"),
      stderr: stderr.into_owned(),
    }));
  }

  // Parse batch output
  // Format: "<sha> <type> <size>\n<content>\n"
  // Or for missing: "<sha> missing\n"
  let mut results = Vec::with_capacity(entries.len());
  let stdout = &output.stdout[..];
  let mut pos = 0;

  for _ in 0..entries.len() {
    // Read header line
    let line_end = stdout[pos..]
      .iter()
      .position(|&b| b == b'\n')
      .ok_or_else(|| RailError::message("Invalid cat-file output: missing newline"))?;
    let header = &stdout[pos..pos + line_end];
    pos += line_end + 1;

    // Check if blob is missing
    if header.ends_with(b" missing") {
      results.push(None);
      continue;
    }

    // Parse size from header: "<sha> <type> <size>"
    let parts: Vec<&[u8]> = header.split(|&b| b == b' ').collect();
    if parts.len() < 3 {
      return Err(RailError::message(format!(
        "Invalid cat-file header: {}",
        String::from_utf8_lossy(header)
      )));
    }

    let size_str = String::from_utf8_lossy(parts[2]);
    let size: usize = size_str
      .parse()
      .map_err(|_| RailError::message(format!("Invalid size in cat-file output: {}", size_str)))?;

    // Read content
    if pos + size > stdout.len() {
      return Err(RailError::message("Unexpected end of cat-file output"));
    }

    let content = String::from_utf8_lossy(&stdout[pos..pos + size]).trim().to_string();
    pos += size;

    // Skip trailing newline after content
    if pos < stdout.len() && stdout[pos] == b'\n' {
      pos += 1;
    }

    results.push(Some(content));
  }

  Ok(results)
}

/// Retry an operation with exponential backoff.
fn retry_operation<F, T>(mut operation: F) -> RailResult<T>
where
  F: FnMut() -> RailResult<T>,
{
  const MAX_RETRIES: u32 = 3;
  let mut attempt = 0;
  let mut delay = Duration::from_millis(500);

  loop {
    match operation() {
      Ok(result) => return Ok(result),
      Err(e) => {
        attempt += 1;
        if attempt > MAX_RETRIES {
          return Err(e);
        }

        // Only retry on network/lock errors
        let should_retry = match &e {
          RailError::Git(GitError::CommandFailed { stderr, .. }) => {
            stderr.contains("lock")
              || stderr.contains("temporarily unavailable")
              || stderr.contains("Connection timed out")
              || stderr.contains("Could not resolve host")
              || stderr.contains("Failed to connect")
          }
          _ => false,
        };

        if !should_retry {
          return Err(e);
        }

        progress!(
          "   ⚠️  Operation failed. Retrying in {:?}... (Attempt {}/{})",
          delay,
          attempt,
          MAX_RETRIES
        );
        thread::sleep(delay);
        delay *= 2;
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::process::Command;
  use tempfile::TempDir;

  /// Helper to run git commands in tests.
  fn git(repo_path: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git").current_dir(repo_path).args(args).output().unwrap()
  }

  /// Initialize a git repo for testing with required config.
  fn init_test_repo(repo_path: &Path) {
    git(repo_path, &["init"]);
    git(repo_path, &["config", "user.name", "Test User"]);
    git(repo_path, &["config", "user.email", "test@example.com"]);
    git(repo_path, &["config", "commit.gpgsign", "false"]);
    git(repo_path, &["config", "tag.gpgsign", "false"]);
  }

  /// Create an initial commit and return its SHA.
  fn create_initial_commit(repo_path: &Path) -> String {
    std::fs::write(repo_path.join("test.txt"), "test").unwrap();
    git(repo_path, &["add", "."]);
    git(repo_path, &["commit", "-m", "Initial commit"]);

    let output = git(repo_path, &["rev-parse", "HEAD"]);
    String::from_utf8(output.stdout).unwrap().trim().to_string()
  }

  #[test]
  fn test_record_and_get_mapping() {
    let mut store = MappingStore::new("test-crate".to_string());

    store.record_mapping("abc123", "def456").unwrap();

    assert_eq!(store.count(), 1);
    assert!(store.has_mapping("abc123"));
    assert_eq!(store.get_mapping("abc123").unwrap(), Some("def456".to_string()));
    assert_eq!(store.get_mapping("unknown").unwrap(), None);
  }

  #[test]
  fn test_save_and_load() {
    let temp = TempDir::new().unwrap();
    let repo_path = temp.path();

    init_test_repo(repo_path);
    let mono_sha = create_initial_commit(repo_path);

    // Create and save mappings
    let mut store = MappingStore::new("test-crate".to_string());
    store.record_mapping(&mono_sha, "remote_sha_1").unwrap();
    store.save(repo_path).unwrap();

    // Load into new store
    let mut loaded_store = MappingStore::new("test-crate".to_string());
    loaded_store.load(repo_path).unwrap();

    assert_eq!(loaded_store.count(), 1);
    assert_eq!(
      loaded_store.get_mapping(&mono_sha).unwrap(),
      Some("remote_sha_1".to_string())
    );
  }

  #[test]
  fn test_load_nonexistent() {
    let temp = TempDir::new().unwrap();
    let repo_path = temp.path();

    let mut store = MappingStore::new("test-crate".to_string());
    let result = store.load(repo_path);

    assert!(result.is_ok());
    assert_eq!(store.count(), 0);
  }

  #[test]
  fn test_reverse_mapping() {
    let mut store = MappingStore::new("test-crate".to_string());

    // Record mapping: mono_sha -> remote_sha
    store.record_mapping("mono_abc123", "remote_def456").unwrap();

    // Forward lookup works
    assert!(store.has_mapping("mono_abc123"));
    assert_eq!(
      store.get_mapping("mono_abc123").unwrap(),
      Some("remote_def456".to_string())
    );

    // Reverse lookup works (O(1) instead of O(n) scan)
    assert!(store.has_reverse_mapping("remote_def456"));
    assert!(!store.has_reverse_mapping("mono_abc123"));
    assert!(!store.has_reverse_mapping("nonexistent"));
  }

  #[test]
  fn test_reverse_mapping_persistence() {
    let temp = TempDir::new().unwrap();
    let repo_path = temp.path();

    init_test_repo(repo_path);
    let mono_sha = create_initial_commit(repo_path);

    // Create and save mappings
    let mut store = MappingStore::new("test-crate".to_string());
    store.record_mapping(&mono_sha, "remote_sha_xyz").unwrap();

    // Verify reverse mapping before save
    assert!(store.has_reverse_mapping("remote_sha_xyz"));

    store.save(repo_path).unwrap();

    // Load into new store and verify reverse mapping is rebuilt
    let mut loaded_store = MappingStore::new("test-crate".to_string());
    loaded_store.load(repo_path).unwrap();

    assert!(loaded_store.has_mapping(&mono_sha));
    assert!(loaded_store.has_reverse_mapping("remote_sha_xyz"));
  }

  #[test]
  fn test_batch_load_multiple_notes() {
    let temp = TempDir::new().unwrap();
    let repo_path = temp.path();

    init_test_repo(repo_path);

    // Create multiple commits
    let sha1 = create_initial_commit(repo_path);

    std::fs::write(repo_path.join("file2.txt"), "content2").unwrap();
    git(repo_path, &["add", "."]);
    git(repo_path, &["commit", "-m", "Second commit"]);
    let output = git(repo_path, &["rev-parse", "HEAD"]);
    let sha2 = String::from_utf8(output.stdout).unwrap().trim().to_string();

    std::fs::write(repo_path.join("file3.txt"), "content3").unwrap();
    git(repo_path, &["add", "."]);
    git(repo_path, &["commit", "-m", "Third commit"]);
    let output = git(repo_path, &["rev-parse", "HEAD"]);
    let sha3 = String::from_utf8(output.stdout).unwrap().trim().to_string();

    // Save multiple mappings
    let mut store = MappingStore::new("test-crate".to_string());
    store.record_mapping(&sha1, "remote_1").unwrap();
    store.record_mapping(&sha2, "remote_2").unwrap();
    store.record_mapping(&sha3, "remote_3").unwrap();
    store.save(repo_path).unwrap();

    // Load and verify all mappings are retrieved via batch
    let mut loaded_store = MappingStore::new("test-crate".to_string());
    loaded_store.load(repo_path).unwrap();

    assert_eq!(loaded_store.count(), 3);
    assert_eq!(loaded_store.get_mapping(&sha1).unwrap(), Some("remote_1".to_string()));
    assert_eq!(loaded_store.get_mapping(&sha2).unwrap(), Some("remote_2".to_string()));
    assert_eq!(loaded_store.get_mapping(&sha3).unwrap(), Some("remote_3".to_string()));
  }
}
