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
  /// Type of commit (feat, fix, etc.)
  pub commit_type: CommitType,
  /// Optional scope
  pub scope: Option<&'a str>,
  /// Whether this is a breaking change
  pub breaking: bool,
  /// Commit description
  pub description: &'a str,
  /// Optional commit body
  pub body: Option<&'a str>,
  /// Commit SHA
  pub sha: &'a str,
}

/// Type of conventional commit
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommitType {
  /// New feature
  Feature,
  /// Bug fix
  Fix,
  /// Breaking change
  Breaking,
  /// Maintenance tasks
  Chore,
  /// Documentation changes
  Docs,
  /// Code style changes
  Style,
  /// Code refactoring
  Refactor,
  /// Performance improvements
  Perf,
  /// Test additions or changes
  Test,
  /// Build system changes
  Build,
  /// CI configuration changes
  Ci,
  /// Uncategorized changes
  Other,
}

impl CommitType {
  /// Get string representation of commit type
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

  /// Get emoji representation for this commit type
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

  /// Get section title for changelog output
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

/// Parse a GitHub remote URL into (org, repo)
fn parse_github_remote(url: &str) -> Option<(String, String)> {
  let trimmed = url.trim().trim_end_matches(".git").trim_end_matches('/');

  let repo_part = if let Some(ssh) = trimmed.strip_prefix("git@github.com:") {
    ssh
  } else if let Some(ssh) = trimmed.strip_prefix("ssh://git@github.com/") {
    ssh
  } else if let Some(https) = trimmed.strip_prefix("https://github.com/") {
    https
  } else {
    return None;
  };

  let mut parts = repo_part.split('/');
  let org = parts.next()?;
  let repo = parts.next()?;

  Some((org.to_string(), repo.to_string()))
}

/// Detect GitHub repository from the git remote
pub fn detect_github_repo(workspace_root: &Path) -> Option<(String, String)> {
  let output = Command::new("git")
    .current_dir(workspace_root)
    .args(["config", "--get", "remote.origin.url"])
    .output()
    .ok()?;

  if !output.status.success() {
    return None;
  }

  let url = String::from_utf8_lossy(&output.stdout);
  parse_github_remote(&url)
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

/// Extract PR references (#123) from text
fn extract_pr_numbers(text: &str) -> Vec<u32> {
  let mut prs = Vec::new();

  for word in text.split_whitespace() {
    if let Some(num_str) = word.strip_prefix("(#").and_then(|s| s.strip_suffix(')'))
      && let Ok(num) = num_str.parse::<u32>()
    {
      prs.push(num);
      continue;
    }

    if let Some(num_str) = word.strip_prefix('#') {
      let numeric = num_str.trim_end_matches(|c: char| !c.is_ascii_digit());
      if let Ok(num) = numeric.parse::<u32>() {
        prs.push(num);
      }
    }
  }

  prs.sort_unstable();
  prs.dedup();
  prs
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
  /// Workspace root directory
  workspace_root: std::path::PathBuf,
  /// GitHub repository (org, repo) if detected
  github_repo: Option<(String, String)>,
}

impl ChangelogGenerator {
  /// Create a new changelog generator
  pub fn new(workspace_root: &Path) -> Self {
    Self {
      workspace_root: workspace_root.to_path_buf(),
      github_repo: detect_github_repo(workspace_root),
    }
  }

  /// Get the detected GitHub repository (org, repo)
  pub fn github_repo(&self) -> Option<&(String, String)> {
    self.github_repo.as_ref()
  }

  fn short_sha<'a>(&self, sha: &'a str) -> &'a str {
    sha.get(..7).unwrap_or(sha)
  }

  fn format_sha(&self, sha: &str) -> String {
    if let Some((org, repo)) = &self.github_repo {
      return format!(
        "[{}](https://github.com/{}/{}/commit/{})",
        self.short_sha(sha),
        org,
        repo,
        sha
      );
    }

    self.short_sha(sha).to_string()
  }

  fn format_pr_links(&self, commit: &ConventionalCommit) -> Option<String> {
    let mut text = commit.description.to_string();
    if let Some(body) = commit.body {
      text.push(' ');
      text.push_str(body);
    }

    let prs = extract_pr_numbers(&text);
    if prs.is_empty() {
      return None;
    }

    if let Some((org, repo)) = &self.github_repo {
      let links = prs
        .iter()
        .map(|n| format!("[#{}](https://github.com/{}/{}/pull/{})", n, org, repo, n))
        .collect::<Vec<_>>()
        .join(" ");
      Some(links)
    } else {
      Some(prs.iter().map(|pr| format!("#{}", pr)).collect::<Vec<_>>().join(" "))
    }
  }

  fn format_description(&self, commit: &ConventionalCommit) -> String {
    let mut desc = commit.description.trim().to_string();

    if commit.breaking {
      desc = format!("[**breaking**] {}", desc);
    }

    desc
  }

  fn format_entry(&self, commit: &ConventionalCommit) -> String {
    let scope_prefix = commit.scope.map(|s| format!("**{}**: ", s)).unwrap_or_default();
    let sha = self.format_sha(commit.sha);
    let desc = self.format_description(commit);

    if let Some(prs) = self.format_pr_links(commit) {
      format!("- {}{} {} ({})\n", scope_prefix, desc, prs, sha)
    } else {
      format!("- {}{} ({})\n", scope_prefix, desc, sha)
    }
  }

  /// Generate changelog from git history
  ///
  /// Uses conventional-commit grouping and optional path filtering for
  /// per-crate changelog generation in monorepos.
  pub fn generate(&self, from_tag: Option<&str>, to_ref: &str, paths: Option<&[&Path]>) -> RailResult<String> {
    // 1. Get commits from git
    let commits = self.get_commits(from_tag, to_ref, paths)?;

    // 2. Parse commits
    let parsed: Vec<ConventionalCommit> = commits
      .iter()
      .map(|(sha, subject, body)| parse_conventional_commit(sha, subject, body.as_deref()))
      .collect();

    // 3. Group by type - pre-allocate based on expected distribution
    // Most commits are features/fixes, breaking changes are rare
    let commit_count = parsed.len();
    let mut breaking = Vec::with_capacity(commit_count / 10); // ~10% breaking
    let mut features = Vec::with_capacity(commit_count / 3); // ~33% features
    let mut fixes = Vec::with_capacity(commit_count / 3); // ~33% fixes
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
        changelog.push_str(&self.format_entry(commit));
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
        changelog.push_str(&self.format_entry(commit));
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
        changelog.push_str(&self.format_entry(commit));
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
        changelog.push_str(&self.format_entry(commit));
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

  #[test]
  fn parse_github_remote_supports_common_patterns() {
    assert_eq!(
      parse_github_remote("git@github.com:org/repo.git"),
      Some(("org".to_string(), "repo".to_string()))
    );

    assert_eq!(
      parse_github_remote("https://github.com/org/repo"),
      Some(("org".to_string(), "repo".to_string()))
    );

    assert_eq!(
      parse_github_remote("ssh://git@github.com/org/repo"),
      Some(("org".to_string(), "repo".to_string()))
    );
  }

  #[test]
  fn parse_github_remote_non_github_returns_none() {
    assert_eq!(parse_github_remote("git@gitlab.com:org/repo.git"), None);
  }

  #[test]
  fn extract_pr_numbers_supports_common_patterns() {
    let prs = extract_pr_numbers("feat(auth): add login (#12) closes #34 and refs #34");
    assert_eq!(prs, vec![12, 34]);
  }

  #[test]
  fn format_entry_includes_pr_and_links_when_available() {
    let commit = ConventionalCommit {
      commit_type: CommitType::Feature,
      scope: Some("api"),
      breaking: false,
      description: "redesign REST endpoints (#123)",
      body: Some("closes #456"),
      sha: "abcdef1234567890",
    };

    let generator = ChangelogGenerator {
      workspace_root: std::path::PathBuf::new(),
      github_repo: Some(("org".to_string(), "repo".to_string())),
    };

    let line = generator.format_entry(&commit);

    assert!(line.contains("[#123](https://github.com/org/repo/pull/123)"));
    assert!(line.contains("[#456](https://github.com/org/repo/pull/456)"));
    assert!(line.contains("**api**: redesign REST endpoints (#123)"));
    assert!(line.contains("[abcdef1](https://github.com/org/repo/commit/abcdef1234567890)"));
  }

  #[test]
  fn format_entry_marks_breaking_inline() {
    let commit = ConventionalCommit {
      commit_type: CommitType::Breaking,
      scope: None,
      breaking: true,
      description: "change API",
      body: None,
      sha: "1234567",
    };

    let generator = ChangelogGenerator {
      workspace_root: std::path::PathBuf::new(),
      github_repo: None,
    };

    let line = generator.format_entry(&commit);
    assert!(line.contains("[**breaking**] change API"));
  }

  #[test]
  fn format_entry_without_github_keeps_plain_pr_refs() {
    let commit = ConventionalCommit {
      commit_type: CommitType::Feature,
      scope: None,
      breaking: false,
      description: "add feature (#12)",
      body: Some("follow-up #34"),
      sha: "abcdef1234567890",
    };

    let generator = ChangelogGenerator {
      workspace_root: std::path::PathBuf::new(),
      github_repo: None,
    };

    let line = generator.format_entry(&commit);
    assert!(line.contains("#12 #34"));
    assert!(line.contains("(abcdef1)"));
  }
}
