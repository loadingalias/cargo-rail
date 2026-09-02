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

impl ConflictStrategy {
    pub(crate) fn authority_name(self) -> &'static str {
        match self {
            Self::Ours => "ours",
            Self::Theirs => "theirs",
            Self::Manual => "manual",
            Self::Union => "union",
        }
    }
}

/// Information about a conflict
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictClass {
    /// Both sides changed the same content and Git could not merge it cleanly.
    Content,
}

/// One unresolved conflict that requires operator resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
        let (result, merged_content) = self.resolve_file_content(current_path, base_content, incoming_content)?;
        crate::utils::write_file_atomic(current_path, &merged_content)?;
        Ok(result)
    }

    /// Resolve one file without changing the real worktree.
    pub(crate) fn resolve_file_content(
        &self,
        current_path: &Path,
        base_content: &[u8],
        incoming_content: &[u8],
    ) -> RailResult<(MergeResult, Vec<u8>)> {
        let current_metadata = std::fs::symlink_metadata(current_path)?;
        if current_metadata.file_type().is_symlink() || !current_metadata.is_file() {
            return Err(crate::error::RailError::message(format!(
                "sync conflict path '{}' must be a regular file",
                current_path.display()
            )));
        }
        let current_content = std::fs::read(current_path)?;

        self.resolve_content(current_path, &current_content, base_content, incoming_content)
    }

    /// Resolve exact current, base, and incoming bytes without reading or
    /// changing the real worktree. `path` is used only in diagnostics.
    pub(crate) fn resolve_content(
        &self,
        path: &Path,
        current_content: &[u8],
        base_content: &[u8],
        incoming_content: &[u8],
    ) -> RailResult<(MergeResult, Vec<u8>)> {
        std::fs::create_dir_all(&self.work_dir).context("Failed to create conflict working directory")?;
        let temp_base = self.work_dir.join("merge-base");
        let temp_current = self.work_dir.join("merge-current");
        let temp_incoming = self.work_dir.join("merge-incoming");

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

        cmd.arg("-L")
            .arg(path)
            .args(["-L", "cargo-rail merge base", "-L", "incoming split commit"]);

        // Add file arguments: current base incoming
        cmd.arg(&temp_current);
        cmd.arg(&temp_base);
        cmd.arg(&temp_incoming);

        let output = cmd.output().context("Failed to run git merge-file")?;

        let merged_content = std::fs::read(&temp_current)?;
        let result = match output.status.code() {
            Some(code @ (0 | 1)) => {
                if code == 0 {
                    MergeResult::Success
                } else {
                    MergeResult::Conflicts(vec![path.to_path_buf()])
                }
            }
            Some(code) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                MergeResult::Failed(format!("git merge-file failed with code {}: {}", code, stderr))
            }
            None => MergeResult::Failed("git merge-file was terminated by signal".to_string()),
        };
        std::fs::remove_file(&temp_base)?;
        std::fs::remove_file(&temp_current)?;
        std::fs::remove_file(&temp_incoming)?;
        Ok((result, merged_content))
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
