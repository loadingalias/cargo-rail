//! Conflict resolution for sync operations.
//!
//! Handles file-level conflicts between monorepo and split repositories using
//! Git's 3-way merge (`git merge-file`) with configurable strategy.

use crate::error::{RailResult, ResultExt};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Strategy for resolving conflicts
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
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

/// Information about a conflict
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictClass {
    /// Both sides changed the same content and Git could not merge it cleanly.
    Content,
}

/// One unresolved conflict that requires operator resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictInfo {
    /// Path to the conflicted file
    pub file_path: PathBuf,
    /// Stable machine-readable conflict classification.
    pub class: ConflictClass,
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
#[derive(Debug)]
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

    /// Resolve a single file with 3-way merge.
    ///
    /// Applies the configured conflict strategy and writes merged content back to
    /// `current_path`. When conflicts remain, conflict markers are preserved in
    /// the file and reported via [`MergeResult::Conflicts`].
    ///
    /// # Errors
    ///
    /// Returns an error when temporary file I/O or `git merge-file` execution fails.
    pub fn resolve_file(
        &self,
        current_path: &Path,
        base_content: &[u8],
        incoming_content: &[u8],
    ) -> RailResult<MergeResult> {
        std::fs::create_dir_all(&self.work_dir).context("Failed to create conflict working directory")?;
        let temp_base = self.work_dir.join("merge-base");
        let temp_current = self.work_dir.join("merge-current");
        let temp_incoming = self.work_dir.join("merge-incoming");
        let current_metadata = std::fs::symlink_metadata(current_path)?;
        if current_metadata.file_type().is_symlink() || !current_metadata.is_file() {
            return Err(crate::error::RailError::message(format!(
                "sync conflict path '{}' must be a regular file",
                current_path.display()
            )));
        }
        let current_content = std::fs::read(current_path)?;

        std::fs::write(&temp_base, base_content).context("Failed to write base file for merge")?;
        std::fs::write(&temp_current, current_content).context("Failed to write current file for merge")?;
        std::fs::write(&temp_incoming, incoming_content).context("Failed to write incoming file for merge")?;

        // Build git merge-file command with strategy
        let mut cmd = crate::git::git_command();
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

        let result = match output.status.code() {
            Some(code @ (0 | 1)) => {
                let merged_content = std::fs::read(&temp_current)?;
                crate::utils::write_file_atomic(current_path, &merged_content)?;
                if code == 0 {
                    Ok(MergeResult::Success)
                } else {
                    Ok(MergeResult::Conflicts(vec![current_path.to_path_buf()]))
                }
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
        };
        std::fs::remove_file(&temp_base)?;
        std::fs::remove_file(&temp_current)?;
        std::fs::remove_file(&temp_incoming)?;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_strategy_value_enum() {
        // ValueEnum provides from_str via clap
        use clap::ValueEnum;
        assert_eq!(
            ConflictStrategy::from_str("ours", false).unwrap(),
            ConflictStrategy::Ours
        );
        assert_eq!(
            ConflictStrategy::from_str("theirs", false).unwrap(),
            ConflictStrategy::Theirs
        );
        assert_eq!(
            ConflictStrategy::from_str("manual", false).unwrap(),
            ConflictStrategy::Manual
        );
        assert_eq!(
            ConflictStrategy::from_str("union", false).unwrap(),
            ConflictStrategy::Union
        );
        ConflictStrategy::from_str("invalid", false).unwrap_err();
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

    #[cfg(unix)]
    #[test]
    fn conflict_resolution_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let resolver = ConflictResolver::new(ConflictStrategy::Manual, temp.path().to_path_buf());
        let outside = TempDir::new().unwrap();
        let victim = outside.path().join("victim");
        std::fs::write(&victim, "outside\n").unwrap();
        let current = temp.path().join("current");
        symlink(&victim, &current).unwrap();

        let error = resolver.resolve_file(&current, b"base\n", b"incoming\n").unwrap_err();
        assert!(error.to_string().contains("must be a regular file"));
        assert_eq!(std::fs::read_to_string(victim).unwrap(), "outside\n");
    }
}
