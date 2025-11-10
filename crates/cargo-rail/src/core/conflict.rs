/// Conflict resolution for cargo-rail
///
/// Handles file-level conflicts when syncing changes between monorepo and split repos.
/// Uses Git's battle-tested 3-way merge algorithm via `git merge-file`.
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Strategy for resolving conflicts
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictStrategy {
  /// Use the monorepo version (--ours)
  Ours,
  /// Use the remote/split repo version (--theirs)
  Theirs,
  /// Attempt automatic merge; create conflict markers if conflicts exist (default)
  #[default]
  Manual,
  /// Combine both versions line-by-line (union merge)
  Union,
}

impl ConflictStrategy {
  pub fn from_str(s: &str) -> Result<Self> {
    match s.to_lowercase().as_str() {
      "ours" | "use-mono" => Ok(Self::Ours),
      "theirs" | "use-remote" => Ok(Self::Theirs),
      "manual" => Ok(Self::Manual),
      "union" => Ok(Self::Union),
      _ => anyhow::bail!(
        "Invalid conflict strategy '{}'. Valid options: ours, theirs, manual, union",
        s
      ),
    }
  }
}

/// Information about a conflict
#[derive(Debug, Clone)]
pub struct ConflictInfo {
  pub file_path: PathBuf,
  pub message: String,
  #[allow(dead_code)]
  pub resolved: bool,
}

/// Result of a merge operation
#[derive(Debug)]
pub enum MergeResult {
  /// Files merged successfully without conflicts
  Success,
  /// Files have conflicts (conflict markers inserted)
  Conflicts(Vec<PathBuf>),
  /// Merge failed completely
  Failed(String),
}

/// Conflict resolver using Git's 3-way merge
pub struct ConflictResolver {
  strategy: ConflictStrategy,
  /// Working directory for temporary files
  work_dir: PathBuf,
}

impl ConflictResolver {
  /// Create a new conflict resolver
  pub fn new(strategy: ConflictStrategy, work_dir: PathBuf) -> Self {
    Self { strategy, work_dir }
  }

  /// Get the current conflict resolution strategy
  pub fn strategy(&self) -> ConflictStrategy {
    self.strategy
  }

  /// Resolve conflicts for a single file using 3-way merge
  ///
  /// # Arguments
  /// * `current_path` - Path to current file (in monorepo)
  /// * `base_content` - Content of the common ancestor
  /// * `incoming_content` - Content from remote/split repo
  ///
  /// # Returns
  /// * `Ok(MergeResult::Success)` - Merged successfully
  /// * `Ok(MergeResult::Conflicts)` - Conflicts detected (markers inserted)
  /// * `Err(_)` - Merge failed
  pub fn resolve_file(&self, current_path: &Path, base_content: &[u8], incoming_content: &[u8]) -> Result<MergeResult> {
    // Create temporary files for 3-way merge
    let temp_base = self.work_dir.join("merge-base");
    let temp_current = self.work_dir.join("merge-current");
    let temp_incoming = self.work_dir.join("merge-incoming");

    std::fs::write(&temp_base, base_content).context("Failed to write base file for merge")?;
    std::fs::write(&temp_current, std::fs::read(current_path)?).context("Failed to write current file for merge")?;
    std::fs::write(&temp_incoming, incoming_content).context("Failed to write incoming file for merge")?;

    // Build git merge-file command with strategy
    let mut cmd = Command::new("git");
    cmd.arg("merge-file");

    match self.strategy {
      ConflictStrategy::Ours => {
        cmd.arg("--ours");
      }
      ConflictStrategy::Theirs => {
        cmd.arg("--theirs");
      }
      ConflictStrategy::Manual => {
        // Default behavior - create conflict markers
      }
      ConflictStrategy::Union => {
        cmd.arg("--union");
      }
    }

    // Add file arguments: current base incoming
    cmd.arg(&temp_current);
    cmd.arg(&temp_base);
    cmd.arg(&temp_incoming);

    let output = cmd.output().context("Failed to run git merge-file")?;

    // Check result
    // Exit codes: 0 = clean merge, 1 = conflicts, >1 = error
    match output.status.code() {
      Some(0) => {
        // Clean merge - copy result back
        let merged_content = std::fs::read(&temp_current)?;
        std::fs::write(current_path, merged_content)?;

        // Clean up temp files
        let _ = std::fs::remove_file(&temp_base);
        let _ = std::fs::remove_file(&temp_current);
        let _ = std::fs::remove_file(&temp_incoming);

        Ok(MergeResult::Success)
      }
      Some(1) => {
        // Conflicts detected
        let merged_content = std::fs::read(&temp_current)?;
        std::fs::write(current_path, merged_content)?;

        // Clean up temp files
        let _ = std::fs::remove_file(&temp_base);
        let _ = std::fs::remove_file(&temp_current);
        let _ = std::fs::remove_file(&temp_incoming);

        Ok(MergeResult::Conflicts(vec![current_path.to_path_buf()]))
      }
      Some(code) => {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(MergeResult::Failed(format!(
          "git merge-file failed with code {}: {}",
          code, stderr
        )))
      }
      None => Ok(MergeResult::Failed(
        "git merge-file was terminated by signal".to_string(),
      )),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

  #[test]
  fn test_strategy_from_str() {
    assert_eq!(ConflictStrategy::from_str("ours").unwrap(), ConflictStrategy::Ours);
    assert_eq!(ConflictStrategy::from_str("theirs").unwrap(), ConflictStrategy::Theirs);
    assert_eq!(ConflictStrategy::from_str("manual").unwrap(), ConflictStrategy::Manual);
    assert_eq!(ConflictStrategy::from_str("union").unwrap(), ConflictStrategy::Union);

    assert!(ConflictStrategy::from_str("invalid").is_err());
  }

  #[test]
  fn test_clean_merge() {
    let temp = TempDir::new().unwrap();
    let resolver = ConflictResolver::new(ConflictStrategy::Manual, temp.path().to_path_buf());

    let current_file = temp.path().join("test.txt");
    std::fs::write(&current_file, "line 1\nline 2\nline 3\n").unwrap();

    let base = b"line 1\nline 2\nline 3\n";
    let incoming = b"line 1\nline 2 modified\nline 3\n";

    let result = resolver.resolve_file(&current_file, base, incoming).unwrap();

    match result {
      MergeResult::Success => {
        let content = std::fs::read_to_string(&current_file).unwrap();
        assert!(content.contains("line 2 modified"));
      }
      _ => panic!("Expected clean merge"),
    }
  }

  #[test]
  fn test_conflict_detection() {
    let temp = TempDir::new().unwrap();
    let resolver = ConflictResolver::new(ConflictStrategy::Manual, temp.path().to_path_buf());

    let current_file = temp.path().join("test.txt");
    std::fs::write(&current_file, "line 1\nline 2 current\nline 3\n").unwrap();

    let base = b"line 1\nline 2\nline 3\n";
    let incoming = b"line 1\nline 2 incoming\nline 3\n";

    let result = resolver.resolve_file(&current_file, base, incoming).unwrap();

    match result {
      MergeResult::Conflicts(paths) => {
        assert_eq!(paths.len(), 1);
        // Conflict markers are present in the file after merge
        let content = std::fs::read_to_string(&current_file).unwrap();
        assert!(content.contains("<<<<<<<"));
      }
      _ => panic!("Expected conflicts"),
    }
  }

  #[test]
  fn test_ours_strategy() {
    let temp = TempDir::new().unwrap();
    let resolver = ConflictResolver::new(ConflictStrategy::Ours, temp.path().to_path_buf());

    let current_file = temp.path().join("test.txt");
    std::fs::write(&current_file, "line 1\nline 2 current\nline 3\n").unwrap();

    let base = b"line 1\nline 2\nline 3\n";
    let incoming = b"line 1\nline 2 incoming\nline 3\n";

    let result = resolver.resolve_file(&current_file, base, incoming).unwrap();

    match result {
      MergeResult::Success => {
        let content = std::fs::read_to_string(&current_file).unwrap();
        assert!(content.contains("line 2 current"));
        assert!(!content.contains("line 2 incoming"));
      }
      _ => panic!("Expected clean merge with --ours"),
    }
  }

  #[test]
  fn test_theirs_strategy() {
    let temp = TempDir::new().unwrap();
    let resolver = ConflictResolver::new(ConflictStrategy::Theirs, temp.path().to_path_buf());

    let current_file = temp.path().join("test.txt");
    std::fs::write(&current_file, "line 1\nline 2 current\nline 3\n").unwrap();

    let base = b"line 1\nline 2\nline 3\n";
    let incoming = b"line 1\nline 2 incoming\nline 3\n";

    let result = resolver.resolve_file(&current_file, base, incoming).unwrap();

    match result {
      MergeResult::Success => {
        let content = std::fs::read_to_string(&current_file).unwrap();
        assert!(!content.contains("line 2 current"));
        assert!(content.contains("line 2 incoming"));
      }
      _ => panic!("Expected clean merge with --theirs"),
    }
  }

  #[test]
  fn test_union_strategy() {
    let temp = TempDir::new().unwrap();
    let resolver = ConflictResolver::new(ConflictStrategy::Union, temp.path().to_path_buf());

    let current_file = temp.path().join("test.txt");
    std::fs::write(&current_file, "line 1\nline 2 current\nline 3\n").unwrap();

    let base = b"line 1\nline 2\nline 3\n";
    let incoming = b"line 1\nline 2 incoming\nline 3\n";

    let result = resolver.resolve_file(&current_file, base, incoming).unwrap();

    match result {
      MergeResult::Success => {
        let content = std::fs::read_to_string(&current_file).unwrap();
        // Union should contain both versions
        assert!(content.contains("line 2 current"));
        assert!(content.contains("line 2 incoming"));
      }
      _ => panic!("Expected clean merge with --union"),
    }
  }
}
