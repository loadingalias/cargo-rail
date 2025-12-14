use crate::error::{GitError, RailError, RailResult, ResultExt};
use crate::{progress, warn};
use std::collections::HashMap;
use std::path::Path;
use std::thread;
use std::time::Duration;

/// Commit mapping store using git-notes
/// Maps commits between monorepo and split repos
///
/// Format in git-notes: `refs/notes/rail/{crate_name}`
/// Each note contains: `mono_sha -> remote_sha` or `remote_sha -> mono_sha`
pub struct MappingStore {
  crate_name: String,
  /// In-memory cache of mappings (from_sha -> to_sha)
  mappings: HashMap<String, String>,
  /// Reverse index for O(1) reverse lookups (to_sha -> from_sha)
  reverse_mappings: HashMap<String, String>,
}

impl MappingStore {
  /// Create a new mapping store for a specific crate
  pub fn new(crate_name: String) -> Self {
    Self {
      crate_name,
      mappings: HashMap::new(),
      reverse_mappings: HashMap::new(),
    }
  }

  /// Load mappings from git-notes in a repository
  pub fn load(&mut self, repo_path: &Path) -> RailResult<()> {
    use std::process::Command;

    let notes_ref = format!("refs/notes/rail/{}", self.crate_name);

    // List all notes: git notes --ref=refs/notes/rail/{crate} list
    let output = Command::new("git")
      .current_dir(repo_path)
      .args(["notes", "--ref", &notes_ref, "list"])
      .output();

    // If command fails, notes ref doesn't exist yet - that's ok
    let output = match output {
      Ok(o) if o.status.success() => o,
      _ => return Ok(()), // No notes yet
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Collect all commit SHAs first
    let commit_shas: Vec<&str> = stdout
      .lines()
      .filter_map(|line| {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() == 2 { Some(parts[1]) } else { None }
      })
      .collect();

    // Read each note
    for commit_sha in commit_shas {
      // Read the note content: git notes --ref=refs/notes/rail/{crate} show {commit_sha}
      let note_output = Command::new("git")
        .current_dir(repo_path)
        .args(["notes", "--ref", &notes_ref, "show", commit_sha])
        .output()
        .context("Failed to read note content")?;

      if note_output.status.success() {
        let note_content = String::from_utf8_lossy(&note_output.stdout);
        let target_sha = note_content.trim();
        let from = commit_sha.to_string();
        let to = target_sha.to_string();
        self.mappings.insert(from.clone(), to.clone());
        self.reverse_mappings.insert(to, from);
      }
    }

    Ok(())
  }

  /// Save mappings to git-notes in a repository
  pub fn save(&self, repo_path: &Path) -> RailResult<()> {
    use std::process::Command;

    let notes_ref = format!("refs/notes/rail/{}", self.crate_name);

    // For each mapping, add a note: git notes --ref=refs/notes/rail/{crate} add -f -m "{target_sha}" {source_sha}
    for (source_sha, target_sha) in &self.mappings {
      let output = Command::new("git")
        .current_dir(repo_path)
        .args(["notes", "--ref", &notes_ref, "add", "-f", "-m", target_sha, source_sha])
        .output()
        .context("Failed to add git note")?;

      if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "already has a note" errors
        if !stderr.contains("already has a note") {
          return Err(RailError::Git(GitError::CommandFailed {
            command: "git notes add".to_string(),
            stderr: stderr.to_string(),
          }));
        }
      }
    }

    Ok(())
  }

  /// Record a mapping between two commits
  pub fn record_mapping(&mut self, from_sha: &str, to_sha: &str) -> RailResult<()> {
    let from = from_sha.to_string();
    let to = to_sha.to_string();
    self.mappings.insert(from.clone(), to.clone());
    self.reverse_mappings.insert(to, from);
    Ok(())
  }

  /// Get the mapped commit SHA if it exists
  pub fn get_mapping(&self, sha: &str) -> RailResult<Option<String>> {
    Ok(self.mappings.get(sha).cloned())
  }

  /// Check if a commit has been mapped (forward direction)
  pub fn has_mapping(&self, sha: &str) -> bool {
    self.mappings.contains_key(sha)
  }

  /// Check if a commit exists as a mapping target (reverse direction)
  /// O(1) lookup using reverse index
  pub fn has_reverse_mapping(&self, sha: &str) -> bool {
    self.reverse_mappings.contains_key(sha)
  }

  /// Clear all mappings (only used in tests)
  #[cfg(test)]
  pub fn clear(&mut self) {
    self.mappings.clear();
    self.reverse_mappings.clear();
  }

  /// Get the number of mappings (only used in tests)
  #[cfg(test)]
  pub fn count(&self) -> usize {
    self.mappings.len()
  }

  /// Push git-notes to a remote repository
  pub fn push_notes(&self, repo_path: &Path, remote: &str) -> RailResult<()> {
    use std::process::Command;

    // Skip if no mappings exist
    if self.mappings.is_empty() {
      progress!("   No git-notes to push (no mappings recorded)");
      return Ok(());
    }

    let notes_ref = format!("refs/notes/rail/{}", self.crate_name);

    progress!("   Pushing git-notes to remote '{}'...", remote);

    retry_operation(|| {
      let output = Command::new("git")
        .current_dir(repo_path)
        .args(["push", remote, &notes_ref])
        .output()
        .context("Failed to push git-notes")?;

      if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RailError::Git(GitError::CommandFailed {
          command: "git push notes".to_string(),
          stderr: stderr.to_string(),
        }));
      }
      Ok(())
    })?;

    progress!("   ✅ Pushed git-notes");
    Ok(())
  }

  /// Fetch git-notes from a remote repository
  pub fn fetch_notes(&self, repo_path: &Path, remote: &str) -> RailResult<()> {
    use std::process::Command;

    let notes_ref = format!("refs/notes/rail/{}", self.crate_name);
    let refspec = format!("{}:{}", notes_ref, notes_ref);

    progress!("   Fetching git-notes from remote '{}'...", remote);

    // Retry the fetch operation
    let result = retry_operation(|| {
      let output = Command::new("git")
        .current_dir(repo_path)
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
          command: "git fetch notes".to_string(),
          stderr: stderr.to_string(),
        }));
      }
      Ok(())
    });

    match result {
      Ok(_) => {
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
          progress!("   ⚠️  Git-notes conflict detected (local and remote notes diverged)");
          progress!("   🔄 Attempting automatic merge with union strategy...");

          // Fetch to FETCH_HEAD without updating the ref
          let fetch_output = Command::new("git")
            .current_dir(repo_path)
            .args(["fetch", remote, &notes_ref])
            .output()
            .context("Failed to fetch notes to FETCH_HEAD")?;

          if !fetch_output.status.success() {
            let fetch_stderr = String::from_utf8_lossy(&fetch_output.stderr);
            if !fetch_stderr.contains("couldn't find remote ref") {
              return Err(RailError::Git(GitError::CommandFailed {
                command: "git fetch notes to FETCH_HEAD".to_string(),
                stderr: fetch_stderr.to_string(),
              }));
            }
            return Ok(()); // No remote notes
          }

          // Merge notes using union strategy (combines both without conflict)
          let merge_output = Command::new("git")
            .current_dir(repo_path)
            .args(["notes", "--ref", &notes_ref, "merge", "--strategy=union", "FETCH_HEAD"])
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
        } else {
          // Propagate other errors
          Err(e)
        }
      }
    }
  }
}

/// Retry an operation with exponential backoff
fn retry_operation<F, T>(mut operation: F) -> RailResult<T>
where
  F: FnMut() -> RailResult<T>,
{
  let max_retries = 3;
  let mut attempt = 0;
  let mut delay = Duration::from_millis(500);

  loop {
    match operation() {
      Ok(result) => return Ok(result),
      Err(e) => {
        attempt += 1;
        if attempt > max_retries {
          return Err(e);
        }

        // Only retry on network/lock errors, not logical errors
        // For now, we assume most git command failures in this context are transient or lock-related
        // unless it's a conflict which we handle separately in fetch_notes
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
          max_retries
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
  use tempfile::TempDir;

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
    use std::process::Command;

    let temp = TempDir::new().unwrap();
    let repo_path = temp.path();

    // Initialize git repo
    Command::new("git")
      .current_dir(repo_path)
      .args(["init"])
      .output()
      .unwrap();

    // Configure git identity (required for commits in CI environments)
    Command::new("git")
      .current_dir(repo_path)
      .args(["config", "user.name", "Test User"])
      .output()
      .unwrap();
    Command::new("git")
      .current_dir(repo_path)
      .args(["config", "user.email", "test@example.com"])
      .output()
      .unwrap();

    // Create an initial commit (git-notes need at least one commit)
    std::fs::write(repo_path.join("test.txt"), "test").unwrap();
    Command::new("git")
      .current_dir(repo_path)
      .args(["add", "."])
      .output()
      .unwrap();
    Command::new("git")
      .current_dir(repo_path)
      .args(["commit", "-m", "Initial commit"])
      .output()
      .unwrap();

    // Get the commit SHA to use for mapping
    let output = Command::new("git")
      .current_dir(repo_path)
      .args(["rev-parse", "HEAD"])
      .output()
      .unwrap();
    let mono_sha = String::from_utf8(output.stdout).unwrap().trim().to_string();

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
    use std::process::Command;

    let temp = TempDir::new().unwrap();
    let repo_path = temp.path();

    // Initialize git repo with config
    Command::new("git")
      .current_dir(repo_path)
      .args(["init"])
      .output()
      .unwrap();
    Command::new("git")
      .current_dir(repo_path)
      .args(["config", "user.name", "Test User"])
      .output()
      .unwrap();
    Command::new("git")
      .current_dir(repo_path)
      .args(["config", "user.email", "test@example.com"])
      .output()
      .unwrap();

    // Create initial commit
    std::fs::write(repo_path.join("test.txt"), "test").unwrap();
    Command::new("git")
      .current_dir(repo_path)
      .args(["add", "."])
      .output()
      .unwrap();
    Command::new("git")
      .current_dir(repo_path)
      .args(["commit", "-m", "Initial commit"])
      .output()
      .unwrap();

    let output = Command::new("git")
      .current_dir(repo_path)
      .args(["rev-parse", "HEAD"])
      .output()
      .unwrap();
    let mono_sha = String::from_utf8(output.stdout).unwrap().trim().to_string();

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
}
