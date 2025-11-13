#![allow(dead_code)]

pub mod git;
pub mod system_git;
mod system_git_ops;

pub use git::GitBackend;
pub use system_git::SystemGit;

/// Information about a commit
#[derive(Debug, Clone)]
pub struct CommitInfo {
  pub sha: String,
  pub author: String,
  pub author_email: String,
  pub committer: String,
  pub committer_email: String,
  pub message: String,
  pub timestamp: i64,
  pub parent_shas: Vec<String>,
}

impl CommitInfo {
  /// Get the first line of the commit message
  pub fn summary(&self) -> &str {
    self.message.lines().next().unwrap_or("")
  }
}
