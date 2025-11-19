//! Lightweight changelog generation
//!
//! Replaces git-cliff with a minimal, supply-chain-safe implementation.
//! Uses winnow for zero-copy parsing of conventional commits.

use crate::error::{RailError, RailResult};
use std::path::Path;
use std::process::Command;
use winnow::Parser;
use winnow::ascii::{space0, till_line_ending};
use winnow::combinator::{alt, opt, preceded, terminated};
use winnow::token::take_until;

// Winnow 0.7.x uses Result, not PResult
type PResult<T> = winnow::error::Result<T>;

/// A parsed commit message following conventional commits format
#[derive(Debug, Clone, PartialEq)]
pub struct ConventionalCommit<'a> {
  pub commit_type: CommitType,
  pub scope: Option<&'a str>,
  pub breaking: bool,
  pub description: &'a str,
  pub body: Option<&'a str>,
  pub sha: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommitType {
  Feature,
  Fix,
  Breaking,
  Chore,
  Docs,
  Style,
  Refactor,
  Perf,
  Test,
  Build,
  Ci,
  Other,
}

impl CommitType {
  pub fn as_str(&self) -> &'static str {
    match self {
      Self::Feature => "feat",
      Self::Fix => "fix",
      Self::Breaking => "breaking",
      Self::Chore => "chore",
      Self::Docs => "docs",
      Self::Style => "style",
      Self::Refactor => "refactor",
      Self::Perf => "perf",
      Self::Test => "test",
      Self::Build => "build",
      Self::Ci => "ci",
      Self::Other => "other",
    }
  }

  pub fn emoji(&self) -> &'static str {
    match self {
      Self::Feature => "✨",
      Self::Fix => "🐛",
      Self::Breaking => "⚠️",
      Self::Chore => "🔧",
      Self::Docs => "📝",
      Self::Style => "💄",
      Self::Refactor => "♻️",
      Self::Perf => "⚡",
      Self::Test => "✅",
      Self::Build => "🏗️",
      Self::Ci => "👷",
      Self::Other => "📦",
    }
  }

  pub fn section_title(&self) -> &'static str {
    match self {
      Self::Feature => "Features",
      Self::Fix => "Bug Fixes",
      Self::Breaking => "BREAKING CHANGES",
      Self::Chore => "Chores",
      Self::Docs => "Documentation",
      Self::Style => "Styling",
      Self::Refactor => "Refactoring",
      Self::Perf => "Performance",
      Self::Test => "Testing",
      Self::Build => "Build",
      Self::Ci => "CI",
      Self::Other => "Other Changes",
    }
  }
}

/// Parse commit type from string
fn parse_commit_type(input: &mut &str) -> PResult<CommitType> {
  alt((
    "feat".value(CommitType::Feature),
    "fix".value(CommitType::Fix),
    "chore".value(CommitType::Chore),
    "docs".value(CommitType::Docs),
    "style".value(CommitType::Style),
    "refactor".value(CommitType::Refactor),
    "perf".value(CommitType::Perf),
    "test".value(CommitType::Test),
    "build".value(CommitType::Build),
    "ci".value(CommitType::Ci),
  ))
  .parse_next(input)
}

/// Parse optional scope in parentheses: (scope)
fn parse_scope<'a>(input: &mut &'a str) -> PResult<Option<&'a str>> {
  opt(preceded('(', terminated(take_until(0.., ')'), ')'))).parse_next(input)
}

/// Parse breaking change indicator: ! before colon
fn parse_breaking(input: &mut &str) -> PResult<bool> {
  opt('!').map(|o| o.is_some()).parse_next(input)
}

/// Parse the description after colon
fn parse_description<'a>(input: &mut &'a str) -> PResult<&'a str> {
  preceded((':', space0), till_line_ending).parse_next(input)
}

/// Parse a conventional commit message
///
/// Format: type(scope)!: description
/// Examples:
/// - feat: add new feature
/// - fix(parser): fix parsing bug
/// - feat!: breaking change
pub fn parse_conventional_commit<'a>(sha: &'a str, subject: &'a str, body: Option<&'a str>) -> ConventionalCommit<'a> {
  // Try to parse as conventional commit
  let mut input = subject;

  let result = (parse_commit_type, parse_scope, parse_breaking, parse_description).parse_next(&mut input);

  match result {
    Ok((commit_type, scope, breaking_marker, description)) => {
      // Check for BREAKING CHANGE in body
      let has_breaking_body = body
        .map(|b| b.contains("BREAKING CHANGE:") || b.contains("BREAKING-CHANGE:"))
        .unwrap_or(false);

      let breaking = breaking_marker || has_breaking_body;
      let final_type = if breaking && commit_type != CommitType::Breaking {
        CommitType::Breaking
      } else {
        commit_type
      };

      ConventionalCommit {
        commit_type: final_type,
        scope,
        breaking,
        description,
        body,
        sha,
      }
    }
    Err(_) => {
      // Not a conventional commit - classify as "other"
      ConventionalCommit {
        commit_type: CommitType::Other,
        scope: None,
        breaking: false,
        description: subject,
        body,
        sha,
      }
    }
  }
}

/// Changelog generator
pub struct ChangelogGenerator {
  workspace_root: std::path::PathBuf,
}

impl ChangelogGenerator {
  pub fn new(workspace_root: &Path) -> Self {
    Self {
      workspace_root: workspace_root.to_path_buf(),
    }
  }

  /// Generate changelog from git history
  ///
  /// # Arguments
  /// * `from_tag` - Starting tag (exclusive), or None for all history
  /// * `to_ref` - Ending reference (inclusive), usually "HEAD"
  /// * `paths` - Optional paths to filter commits (for monorepo per-crate changelogs)
  pub fn generate(&self, from_tag: Option<&str>, to_ref: &str, paths: Option<&[&Path]>) -> RailResult<String> {
    // 1. Get commits from git
    let commits = self.get_commits(from_tag, to_ref, paths)?;

    // 2. Parse commits
    let parsed: Vec<ConventionalCommit> = commits
      .iter()
      .map(|(sha, subject, body)| parse_conventional_commit(sha, subject, body.as_deref()))
      .collect();

    // 3. Group by type
    let mut breaking = Vec::new();
    let mut features = Vec::new();
    let mut fixes = Vec::new();
    let mut other_groups: std::collections::HashMap<CommitType, Vec<&ConventionalCommit>> =
      std::collections::HashMap::new();

    for commit in &parsed {
      match commit.commit_type {
        CommitType::Breaking => breaking.push(commit),
        CommitType::Feature => features.push(commit),
        CommitType::Fix => fixes.push(commit),
        _ => {
          other_groups.entry(commit.commit_type).or_default().push(commit);
        }
      }
    }

    // 4. Format as markdown
    let mut changelog = String::new();

    // Breaking changes first (most important)
    if !breaking.is_empty() {
      changelog.push_str(&format!(
        "### {} {}\n\n",
        CommitType::Breaking.emoji(),
        CommitType::Breaking.section_title()
      ));
      for commit in breaking {
        changelog.push_str(&format!("- {} ({})\n", commit.description.trim(), &commit.sha[..7]));
      }
      changelog.push('\n');
    }

    // Features
    if !features.is_empty() {
      changelog.push_str(&format!(
        "### {} {}\n\n",
        CommitType::Feature.emoji(),
        CommitType::Feature.section_title()
      ));
      for commit in features {
        let scope_prefix = commit.scope.map(|s| format!("**{}**: ", s)).unwrap_or_default();
        changelog.push_str(&format!(
          "- {}{} ({})\n",
          scope_prefix,
          commit.description.trim(),
          &commit.sha[..7]
        ));
      }
      changelog.push('\n');
    }

    // Fixes
    if !fixes.is_empty() {
      changelog.push_str(&format!(
        "### {} {}\n\n",
        CommitType::Fix.emoji(),
        CommitType::Fix.section_title()
      ));
      for commit in fixes {
        let scope_prefix = commit.scope.map(|s| format!("**{}**: ", s)).unwrap_or_default();
        changelog.push_str(&format!(
          "- {}{} ({})\n",
          scope_prefix,
          commit.description.trim(),
          &commit.sha[..7]
        ));
      }
      changelog.push('\n');
    }

    // Other types (sorted by type)
    let mut types: Vec<_> = other_groups.keys().collect();
    types.sort_by_key(|t| t.as_str());

    for commit_type in types {
      let commits = &other_groups[commit_type];
      changelog.push_str(&format!(
        "### {} {}\n\n",
        commit_type.emoji(),
        commit_type.section_title()
      ));
      for commit in commits {
        let scope_prefix = commit.scope.map(|s| format!("**{}**: ", s)).unwrap_or_default();
        changelog.push_str(&format!(
          "- {}{} ({})\n",
          scope_prefix,
          commit.description.trim(),
          &commit.sha[..7]
        ));
      }
      changelog.push('\n');
    }

    Ok(changelog)
  }

  /// Get commits from git log
  ///
  /// Returns Vec<(sha, subject, body)>
  fn get_commits(
    &self,
    from_tag: Option<&str>,
    to_ref: &str,
    paths: Option<&[&Path]>,
  ) -> RailResult<Vec<(String, String, Option<String>)>> {
    let range = if let Some(from) = from_tag {
      format!("{}..{}", from, to_ref)
    } else {
      to_ref.to_string()
    };

    let mut cmd = Command::new("git");
    cmd.current_dir(&self.workspace_root).args([
      "log",
      &range,
      "--format=%H%x00%s%x00%b%x00",
      "--no-merges", // Skip merge commits
    ]);

    // Add path filters if specified
    if let Some(paths) = paths
      && !paths.is_empty()
    {
      cmd.arg("--");
      for path in paths {
        cmd.arg(path);
      }
    }

    let output = cmd
      .output()
      .map_err(|e| RailError::message(format!("Failed to execute git log: {}", e)))?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::message(format!("git log failed: {}", stderr)));
    }

    let log = String::from_utf8_lossy(&output.stdout);
    let mut commits = Vec::new();

    // Parse git log output (format: SHA\0SUBJECT\0BODY\0)
    for commit_block in log.split("\0\0") {
      if commit_block.trim().is_empty() {
        continue;
      }

      let parts: Vec<&str> = commit_block.split('\0').collect();
      if parts.len() < 2 {
        continue;
      }

      let sha = parts[0].trim().to_string();
      let subject = parts[1].trim().to_string();
      let body = if parts.len() > 2 && !parts[2].trim().is_empty() {
        Some(parts[2].trim().to_string())
      } else {
        None
      };

      commits.push((sha, subject, body));
    }

    Ok(commits)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_parse_conventional_commit_feature() {
    let commit = parse_conventional_commit("abc123", "feat: add new feature", None);
    assert_eq!(commit.commit_type, CommitType::Feature);
    assert_eq!(commit.scope, None);
    assert!(!commit.breaking);
    assert_eq!(commit.description, "add new feature");
  }

  #[test]
  fn test_parse_conventional_commit_with_scope() {
    let commit = parse_conventional_commit("abc123", "fix(parser): fix parsing bug", None);
    assert_eq!(commit.commit_type, CommitType::Fix);
    assert_eq!(commit.scope, Some("parser"));
    assert!(!commit.breaking);
    assert_eq!(commit.description, "fix parsing bug");
  }

  #[test]
  fn test_parse_conventional_commit_breaking() {
    let commit = parse_conventional_commit("abc123", "feat!: breaking change", None);
    assert_eq!(commit.commit_type, CommitType::Breaking);
    assert!(commit.breaking);
    assert_eq!(commit.description, "breaking change");
  }

  #[test]
  fn test_parse_conventional_commit_breaking_in_body() {
    let commit = parse_conventional_commit(
      "abc123",
      "feat: some feature",
      Some("BREAKING CHANGE: this breaks stuff"),
    );
    assert_eq!(commit.commit_type, CommitType::Breaking);
    assert!(commit.breaking);
  }

  #[test]
  fn test_parse_non_conventional_commit() {
    let commit = parse_conventional_commit("abc123", "Update README", None);
    assert_eq!(commit.commit_type, CommitType::Other);
    assert_eq!(commit.description, "Update README");
  }
}
