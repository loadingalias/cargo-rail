#![allow(dead_code)]

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

/// Commit mapping store using git-notes
/// Maps commits between monorepo and split repos
///
/// Format in git-notes: `refs/notes/rail/{crate_name}`
/// Each note contains: `mono_sha -> remote_sha` or `remote_sha -> mono_sha`
pub struct MappingStore {
  crate_name: String,
  /// In-memory cache of mappings
  mappings: HashMap<String, String>,
}

impl MappingStore {
  /// Create a new mapping store for a specific crate
  pub fn new(crate_name: String) -> Self {
    Self {
      crate_name,
      mappings: HashMap::new(),
    }
  }

  /// Load mappings from git-notes in a repository
  pub fn load(&mut self, repo_path: &Path) -> Result<()> {
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

    // Parse output: each line is "{note_sha} {commit_sha}"
    for line in stdout.lines() {
      let parts: Vec<&str> = line.split_whitespace().collect();
      if parts.len() != 2 {
        continue;
      }

      let commit_sha = parts[1];

      // Read the note content: git notes --ref=refs/notes/rail/{crate} show {commit_sha}
      let note_output = Command::new("git")
        .current_dir(repo_path)
        .args(["notes", "--ref", &notes_ref, "show", commit_sha])
        .output()
        .context("Failed to read note content")?;

      if note_output.status.success() {
        let note_content = String::from_utf8_lossy(&note_output.stdout);
        let target_sha = note_content.trim();
        self.mappings.insert(commit_sha.to_string(), target_sha.to_string());
      }
    }

    Ok(())
  }

  /// Save mappings to git-notes in a repository
  pub fn save(&self, repo_path: &Path) -> Result<()> {
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
          anyhow::bail!("git notes add failed: {}", stderr);
        }
      }
    }

    Ok(())
  }

  /// Record a mapping between two commits
  pub fn record_mapping(&mut self, from_sha: &str, to_sha: &str) -> Result<()> {
    self.mappings.insert(from_sha.to_string(), to_sha.to_string());
    Ok(())
  }

  /// Get the mapped commit SHA if it exists
  pub fn get_mapping(&self, sha: &str) -> Result<Option<String>> {
    Ok(self.mappings.get(sha).cloned())
  }

  /// Check if a commit has been mapped
  pub fn has_mapping(&self, sha: &str) -> bool {
    self.mappings.contains_key(sha)
  }

  /// Get all mappings
  pub fn all_mappings(&self) -> &HashMap<String, String> {
    &self.mappings
  }

  /// Clear all mappings
  pub fn clear(&mut self) {
    self.mappings.clear();
  }

  /// Get the number of mappings
  pub fn count(&self) -> usize {
    self.mappings.len()
  }

  /// Push git-notes to a remote repository
  pub fn push_notes(&self, repo_path: &Path, remote: &str) -> Result<()> {
    use std::process::Command;

    // Skip if no mappings exist
    if self.mappings.is_empty() {
      println!("   No git-notes to push (no mappings recorded)");
      return Ok(());
    }

    let notes_ref = format!("refs/notes/rail/{}", self.crate_name);

    println!("   Pushing git-notes to remote '{}'...", remote);

    let output = Command::new("git")
      .current_dir(repo_path)
      .args(["push", remote, &notes_ref])
      .output()
      .context("Failed to push git-notes")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      anyhow::bail!("git push notes failed: {}", stderr);
    }

    println!("   ✅ Pushed git-notes");
    Ok(())
  }

  /// Fetch git-notes from a remote repository
  pub fn fetch_notes(&self, repo_path: &Path, remote: &str) -> Result<()> {
    use std::process::Command;

    let notes_ref = format!("refs/notes/rail/{}", self.crate_name);
    let refspec = format!("{}:{}", notes_ref, notes_ref);

    println!("   Fetching git-notes from remote '{}'...", remote);

    let output = Command::new("git")
      .current_dir(repo_path)
      .args(["fetch", remote, &refspec])
      .output()
      .context("Failed to fetch git-notes")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);

      // Ignore "couldn't find remote ref" - notes may not exist yet
      if stderr.contains("couldn't find remote ref") {
        println!("   ℹ️  No remote git-notes found yet (this is normal for first sync)");
        return Ok(());
      }

      // Handle non-fast-forward (notes conflict)
      if stderr.contains("non-fast-forward") || stderr.contains("rejected") {
        println!("   ⚠️  Git-notes conflict detected (local and remote notes diverged)");
        println!("   🔄 Attempting automatic merge with union strategy...");

        // Fetch to FETCH_HEAD without updating the ref
        let fetch_output = Command::new("git")
          .current_dir(repo_path)
          .args(["fetch", remote, &notes_ref])
          .output()
          .context("Failed to fetch notes to FETCH_HEAD")?;

        if !fetch_output.status.success() {
          let fetch_stderr = String::from_utf8_lossy(&fetch_output.stderr);
          if !fetch_stderr.contains("couldn't find remote ref") {
            anyhow::bail!("git fetch notes to FETCH_HEAD failed: {}", fetch_stderr);
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
          eprintln!("   ❌ Automatic git-notes merge failed");
          eprintln!("   📋 Manual resolution required:");
          eprintln!("      1. cd {}", repo_path.display());
          eprintln!("      2. git notes --ref={} merge FETCH_HEAD", notes_ref);
          eprintln!("      3. Resolve conflicts manually");
          eprintln!("      4. git notes --ref={} merge --commit", notes_ref);
          eprintln!("");
          anyhow::bail!("git notes merge failed: {}\n\nThis usually happens when the same commit has different mappings on different machines.\nPlease resolve manually using the steps above.", merge_stderr);
        }

        println!("   ✅ Git-notes merged successfully (union strategy)");
        return Ok(());
      }

      // Unknown error
      anyhow::bail!("git fetch notes failed: {}", stderr);
    }

    println!("   ✅ Fetched git-notes");
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

  #[test]
  fn test_new_mapping_store() {
    let store = MappingStore::new("test-crate".to_string());
    assert_eq!(store.count(), 0);
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
    use std::process::Command;

    let temp = TempDir::new().unwrap();
    let repo_path = temp.path();

    // Initialize git repo
    Command::new("git")
      .current_dir(repo_path)
      .args(["init"])
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
  fn test_clear() {
    let mut store = MappingStore::new("test-crate".to_string());
    store.record_mapping("abc123", "def456").unwrap();
    assert_eq!(store.count(), 1);

    store.clear();
    assert_eq!(store.count(), 0);
    assert!(!store.has_mapping("abc123"));
  }

  #[test]
  fn test_all_mappings() {
    let mut store = MappingStore::new("test-crate".to_string());
    store.record_mapping("sha1", "sha2").unwrap();
    store.record_mapping("sha3", "sha4").unwrap();

    let all = store.all_mappings();
    assert_eq!(all.len(), 2);
    assert_eq!(all.get("sha1"), Some(&"sha2".to_string()));
    assert_eq!(all.get("sha3"), Some(&"sha4".to_string()));
  }
}
