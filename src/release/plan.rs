//! Release planning: analyze changes and suggest version bumps
//!
//! Uses existing infrastructure:
//! - SystemGit for commit analysis
//! - WorkspaceGraph for dependency tracking (future: publishing order)

use crate::core::config::ReleaseConfig;
use crate::core::error::{RailResult, ResultExt};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

/// Version bump type based on conventional commits
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VersionBump {
  /// Major version bump (breaking changes)
  Major,
  /// Minor version bump (new features)
  Minor,
  /// Patch version bump (bug fixes)
  Patch,
  /// No bump needed (no relevant changes)
  None,
}

impl VersionBump {
  /// Apply bump to a semver version
  pub fn apply(&self, version: &semver::Version) -> semver::Version {
    match self {
      VersionBump::Major => semver::Version::new(version.major + 1, 0, 0),
      VersionBump::Minor => semver::Version::new(version.major, version.minor + 1, 0),
      VersionBump::Patch => semver::Version::new(version.major, version.minor, version.patch + 1),
      VersionBump::None => version.clone(),
    }
  }
}

/// A single commit relevant to the release
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseCommit {
  pub sha: String,
  pub message: String,
  pub commit_type: CommitType,
  pub is_breaking: bool,
}

/// Conventional commit type
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommitType {
  Feat,
  Fix,
  Docs,
  Style,
  Refactor,
  Perf,
  Test,
  Chore,
  Other,
}

impl CommitType {
  /// Parse commit type from message
  fn from_message(msg: &str) -> Self {
    let first_line = msg.lines().next().unwrap_or("");
    let lower = first_line.to_lowercase();

    if lower.starts_with("feat:") || lower.starts_with("feat(") {
      CommitType::Feat
    } else if lower.starts_with("fix:") || lower.starts_with("fix(") {
      CommitType::Fix
    } else if lower.starts_with("docs:") || lower.starts_with("docs(") {
      CommitType::Docs
    } else if lower.starts_with("style:") || lower.starts_with("style(") {
      CommitType::Style
    } else if lower.starts_with("refactor:") || lower.starts_with("refactor(") {
      CommitType::Refactor
    } else if lower.starts_with("perf:") || lower.starts_with("perf(") {
      CommitType::Perf
    } else if lower.starts_with("test:") || lower.starts_with("test(") {
      CommitType::Test
    } else if lower.starts_with("chore:") || lower.starts_with("chore(") {
      CommitType::Chore
    } else {
      CommitType::Other
    }
  }

  /// Check if this commit type affects version bumping
  #[allow(dead_code)] // TODO(Pillar 4): Use for smarter changelog generation
  fn affects_version(&self) -> bool {
    matches!(self, CommitType::Feat | CommitType::Fix | CommitType::Perf)
  }
}

/// Release plan for a single release channel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleasePlan {
  pub name: String,
  pub crate_path: std::path::PathBuf,
  pub current_version: semver::Version,
  pub proposed_version: semver::Version,
  pub bump_type: VersionBump,
  pub commits: Vec<ReleaseCommit>,
  pub has_changes: bool,
  pub is_first_release: bool,
}

impl ReleasePlan {
  /// Create a release plan for a configured release
  pub fn analyze(workspace_root: &Path, release: &ReleaseConfig) -> RailResult<Self> {
    let current_version = release.current_version();
    let is_first_release = release.is_first_release();

    // Get commits since last release
    let commits = if let Some(last_sha) = &release.last_sha {
      Self::get_commits_since(workspace_root, last_sha, &release.crate_path)?
    } else {
      // First release: get all commits touching this crate
      Self::get_all_commits(workspace_root, &release.crate_path)?
    };

    // Analyze commits to determine version bump
    let bump_type = Self::determine_bump(&commits);
    let proposed_version = bump_type.apply(&current_version);

    Ok(Self {
      name: release.name.clone(),
      crate_path: release.crate_path.clone(),
      current_version,
      proposed_version,
      bump_type,
      has_changes: !commits.is_empty(),
      commits,
      is_first_release,
    })
  }

  /// Get commits since last release that touch the crate
  fn get_commits_since(workspace_root: &Path, last_sha: &str, crate_path: &Path) -> RailResult<Vec<ReleaseCommit>> {
    // Get commit range: last_sha..HEAD
    let range = format!("{}..HEAD", last_sha);

    // Get commits that touched the crate path
    let output = Command::new("git")
      .current_dir(workspace_root)
      .args([
        "log",
        &range,
        "--pretty=format:%H|||%s|||%b",
        "--",
        crate_path.to_str().unwrap_or("."),
      ])
      .output()
      .with_context(|| "Failed to run git log".to_string())?;

    if !output.status.success() {
      return Err(crate::core::error::RailError::message(format!(
        "git log failed: {}",
        String::from_utf8_lossy(&output.stderr)
      )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Self::parse_commits(&stdout)
  }

  /// Get all commits that touch the crate (for first release)
  fn get_all_commits(workspace_root: &Path, crate_path: &Path) -> RailResult<Vec<ReleaseCommit>> {
    let output = Command::new("git")
      .current_dir(workspace_root)
      .args([
        "log",
        "--pretty=format:%H|||%s|||%b",
        "--",
        crate_path.to_str().unwrap_or("."),
      ])
      .output()
      .with_context(|| "Failed to run git log".to_string())?;

    if !output.status.success() {
      return Err(crate::core::error::RailError::message(format!(
        "git log failed: {}",
        String::from_utf8_lossy(&output.stderr)
      )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Self::parse_commits(&stdout)
  }

  /// Parse git log output into ReleaseCommit structs
  fn parse_commits(output: &str) -> RailResult<Vec<ReleaseCommit>> {
    let mut commits = Vec::new();

    for line in output.lines() {
      if line.trim().is_empty() {
        continue;
      }

      let parts: Vec<&str> = line.split("|||").collect();
      if parts.len() >= 2 {
        let sha = parts[0].trim().to_string();
        let subject = parts[1].trim();
        let body = parts.get(2).map(|s| s.trim()).unwrap_or("");

        let full_message = if body.is_empty() {
          subject.to_string()
        } else {
          format!("{}\n{}", subject, body)
        };

        let commit_type = CommitType::from_message(&full_message);
        let is_breaking =
          full_message.contains("BREAKING CHANGE") || full_message.contains("BREAKING-CHANGE") || subject.contains('!');

        commits.push(ReleaseCommit {
          sha,
          message: full_message,
          commit_type,
          is_breaking,
        });
      }
    }

    Ok(commits)
  }

  /// Determine version bump from commits
  fn determine_bump(commits: &[ReleaseCommit]) -> VersionBump {
    if commits.is_empty() {
      return VersionBump::None;
    }

    // Check for breaking changes
    if commits.iter().any(|c| c.is_breaking) {
      return VersionBump::Major;
    }

    // Check for features
    if commits.iter().any(|c| c.commit_type == CommitType::Feat) {
      return VersionBump::Minor;
    }

    // Check for fixes or perf improvements
    if commits
      .iter()
      .any(|c| matches!(c.commit_type, CommitType::Fix | CommitType::Perf))
    {
      return VersionBump::Patch;
    }

    // Only non-version-affecting commits (docs, chore, etc.)
    VersionBump::Patch
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_version_bump_apply() {
    let v = semver::Version::new(1, 2, 3);

    assert_eq!(VersionBump::Major.apply(&v).to_string(), "2.0.0");
    assert_eq!(VersionBump::Minor.apply(&v).to_string(), "1.3.0");
    assert_eq!(VersionBump::Patch.apply(&v).to_string(), "1.2.4");
    assert_eq!(VersionBump::None.apply(&v).to_string(), "1.2.3");
  }

  #[test]
  fn test_commit_type_parsing() {
    assert_eq!(CommitType::from_message("feat: add feature"), CommitType::Feat);
    assert_eq!(CommitType::from_message("fix: bug fix"), CommitType::Fix);
    assert_eq!(CommitType::from_message("docs: update readme"), CommitType::Docs);
    assert_eq!(CommitType::from_message("chore: cleanup"), CommitType::Chore);
    assert_eq!(CommitType::from_message("random commit"), CommitType::Other);
  }

  #[test]
  fn test_breaking_change_detection() {
    let commit = ReleaseCommit {
      sha: "abc123".to_string(),
      message: "feat!: breaking change".to_string(),
      commit_type: CommitType::Feat,
      is_breaking: true,
    };

    let bump = ReleasePlan::determine_bump(&[commit]);
    assert_eq!(bump, VersionBump::Major);
  }

  #[test]
  fn test_feature_bump() {
    let commit = ReleaseCommit {
      sha: "abc123".to_string(),
      message: "feat: new feature".to_string(),
      commit_type: CommitType::Feat,
      is_breaking: false,
    };

    let bump = ReleasePlan::determine_bump(&[commit]);
    assert_eq!(bump, VersionBump::Minor);
  }

  #[test]
  fn test_fix_bump() {
    let commit = ReleaseCommit {
      sha: "abc123".to_string(),
      message: "fix: bug fix".to_string(),
      commit_type: CommitType::Fix,
      is_breaking: false,
    };

    let bump = ReleasePlan::determine_bump(&[commit]);
    assert_eq!(bump, VersionBump::Patch);
  }
}
