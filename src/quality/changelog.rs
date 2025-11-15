//! Safe changelog generation from conventional commits
//!
//! Replaces git-cliff-core with a deterministic, zero-panic parser.
//! Uses winnow for parsing (not regex) to ensure correctness and safety.

use std::collections::BTreeMap;
use std::fmt;

/// A parsed conventional commit
///
/// Format: `<type>(<scope>): <description>`
///
/// Example: `feat(auth): add OAuth2 support`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConventionalCommit {
  /// Commit type (feat, fix, chore, docs, etc.)
  pub commit_type: CommitType,
  /// Optional scope (e.g., "auth", "api", "core")
  pub scope: Option<String>,
  /// Short description
  pub description: String,
  /// Full commit body (optional)
  pub body: Option<String>,
  /// Breaking change footer (optional)
  pub breaking_change: Option<String>,
  /// Other footers (e.g., "Closes #123")
  pub footers: Vec<(String, String)>,
}

/// Conventional commit types
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommitType {
  /// New feature
  Feat,
  /// Bug fix
  Fix,
  /// Documentation changes
  Docs,
  /// Code style changes (formatting, etc.)
  Style,
  /// Refactoring (no functional changes)
  Refactor,
  /// Performance improvements
  Perf,
  /// Test additions or changes
  Test,
  /// Build system or external dependency changes
  Build,
  /// CI configuration changes
  Ci,
  /// Chores (maintenance tasks)
  Chore,
  /// Reverts a previous commit
  Revert,
  /// Other/unknown type
  Other,
}

impl CommitType {
  /// Parse commit type from string
  #[track_caller]
  pub fn from_str(s: &str) -> Self {
    match s.to_lowercase().as_str() {
      "feat" | "feature" => Self::Feat,
      "fix" => Self::Fix,
      "docs" | "doc" => Self::Docs,
      "style" => Self::Style,
      "refactor" => Self::Refactor,
      "perf" | "performance" => Self::Perf,
      "test" | "tests" => Self::Test,
      "build" => Self::Build,
      "ci" => Self::Ci,
      "chore" => Self::Chore,
      "revert" => Self::Revert,
      _ => Self::Other,
    }
  }

  /// Check if this commit type triggers a version bump
  #[allow(dead_code)] // Used by has_user_facing_changes() and in tests
  pub fn is_user_facing(&self) -> bool {
    matches!(self, Self::Feat | Self::Fix | Self::Perf)
  }

  /// Get the display name for this commit type
  pub fn display_name(&self) -> &'static str {
    match self {
      Self::Feat => "Features",
      Self::Fix => "Bug Fixes",
      Self::Docs => "Documentation",
      Self::Style => "Style",
      Self::Refactor => "Refactoring",
      Self::Perf => "Performance",
      Self::Test => "Tests",
      Self::Build => "Build",
      Self::Ci => "CI",
      Self::Chore => "Chores",
      Self::Revert => "Reverts",
      Self::Other => "Other",
    }
  }
}

impl fmt::Display for CommitType {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.display_name())
  }
}

/// Changelog output format
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangelogFormat {
  /// Markdown format (default)
  Markdown,
  /// JSON format for programmatic use
  #[allow(dead_code)] // Used by render() and in tests
  Json,
}

/// Generated changelog
#[derive(Debug, Clone)]
pub struct Changelog {
  /// Version for this changelog
  pub version: String,
  /// Date of the release (ISO 8601)
  pub date: String,
  /// Grouped commits by type
  pub commits_by_type: BTreeMap<CommitType, Vec<ConventionalCommit>>,
  /// All commits (including non-conventional)
  pub all_commit_shas: Vec<String>,
}

impl Changelog {
  /// Create a new changelog
  pub fn new(version: String, date: String) -> Self {
    Self {
      version,
      date,
      commits_by_type: BTreeMap::new(),
      all_commit_shas: Vec::new(),
    }
  }

  /// Add a commit to the changelog
  pub fn add_commit(&mut self, commit: ConventionalCommit, sha: String) {
    self.all_commit_shas.push(sha);
    self.commits_by_type.entry(commit.commit_type).or_default().push(commit);
  }

  /// Check if there are any user-facing changes
  #[allow(dead_code)] // Public API for future use, tested in test_changelog_has_user_facing_changes
  pub fn has_user_facing_changes(&self) -> bool {
    self.commits_by_type.keys().any(|t| t.is_user_facing())
  }

  /// Render as markdown
  pub fn to_markdown(&self) -> String {
    let mut output = String::new();

    // Header
    output.push_str(&format!("## [{}] - {}\n\n", self.version, self.date));

    // Iterate through commit types in a deterministic order
    let ordered_types = [
      CommitType::Feat,
      CommitType::Fix,
      CommitType::Perf,
      CommitType::Docs,
      CommitType::Refactor,
      CommitType::Test,
      CommitType::Build,
      CommitType::Ci,
      CommitType::Chore,
      CommitType::Style,
      CommitType::Revert,
      CommitType::Other,
    ];

    for commit_type in &ordered_types {
      if let Some(commits) = self.commits_by_type.get(commit_type) {
        if commits.is_empty() {
          continue;
        }

        // Section header
        output.push_str(&format!("### {}\n\n", commit_type.display_name()));

        // List commits
        for commit in commits {
          let scope_str = commit
            .scope
            .as_ref()
            .map(|s| format!("**{}**: ", s))
            .unwrap_or_default();

          output.push_str(&format!("- {}{}\n", scope_str, commit.description));

          // Add breaking change indicator
          if let Some(ref breaking) = commit.breaking_change {
            if !breaking.is_empty() {
              output.push_str(&format!("  - **BREAKING**: {}\n", breaking));
            } else {
              output.push_str("  - **BREAKING CHANGE**\n");
            }
          }
        }

        output.push('\n');
      }
    }

    output
  }

  /// Render as JSON
  pub fn to_json(&self) -> Result<String, serde_json::Error> {
    use serde::{Serialize, Serializer};

    // Helper struct for JSON serialization
    #[derive(Serialize)]
    struct ChangelogJson {
      version: String,
      date: String,
      sections: Vec<Section>,
      total_commits: usize,
    }

    #[derive(Serialize)]
    struct Section {
      #[serde(serialize_with = "serialize_commit_type")]
      commit_type: CommitType,
      commits: Vec<CommitEntry>,
    }

    #[derive(Serialize)]
    struct CommitEntry {
      description: String,
      scope: Option<String>,
      breaking: bool,
      breaking_description: Option<String>,
    }

    fn serialize_commit_type<S>(ct: &CommitType, s: S) -> Result<S::Ok, S::Error>
    where
      S: Serializer,
    {
      s.serialize_str(ct.display_name())
    }

    let sections: Vec<Section> = self
      .commits_by_type
      .iter()
      .map(|(commit_type, commits)| Section {
        commit_type: *commit_type,
        commits: commits
          .iter()
          .map(|c| CommitEntry {
            description: c.description.clone(),
            scope: c.scope.clone(),
            breaking: c.is_breaking(),
            breaking_description: c.breaking_change.clone(),
          })
          .collect(),
      })
      .collect();

    let json_output = ChangelogJson {
      version: self.version.clone(),
      date: self.date.clone(),
      sections,
      total_commits: self.all_commit_shas.len(),
    };

    serde_json::to_string_pretty(&json_output)
  }

  /// Render in the specified format
  pub fn render(&self, format: ChangelogFormat) -> Result<String, String> {
    match format {
      ChangelogFormat::Markdown => Ok(self.to_markdown()),
      ChangelogFormat::Json => self.to_json().map_err(|e| e.to_string()),
    }
  }
}

impl ConventionalCommit {
  /// Check if this commit is a breaking change
  pub fn is_breaking(&self) -> bool {
    self.breaking_change.is_some()
  }

  /// Parse a conventional commit from a git commit message
  ///
  /// Returns None if the message doesn't follow conventional commit format.
  /// This is intentional - not all commits need to be conventional.
  #[track_caller]
  pub fn parse(message: &str) -> Option<Self> {
    use winnow::ascii::{alphanumeric1, space0};
    use winnow::combinator::{opt, preceded, terminated};
    use winnow::prelude::*;
    use winnow::token::take_till;

    // Split message into first line and rest
    let (first_line, rest) = message.split_once('\n').unwrap_or((message, ""));

    // Parse type(scope)!: description
    let mut parser = (
      // Parse type
      alphanumeric1::<_, ()>.map(|s: &str| CommitType::from_str(s)),
      // Optional scope in parentheses
      opt(preceded('(', terminated(take_till(1.., ')'), ')'))),
      // Optional ! for breaking change
      opt('!'),
      // Colon separator
      ':',
      // Space
      space0,
      // Description (rest of first line)
      take_till(0.., ['\n', '\r']),
    );

    let Ok((commit_type, scope, breaking_indicator, _, _, description)) = parser.parse(first_line) else {
      return None;
    };

    // Parse body and footers
    let mut body_lines = Vec::new();
    let mut breaking_change = None;
    let mut footers = Vec::new();

    let mut in_body = true;
    let mut seen_empty_line = false;

    for line in rest.lines() {
      let trimmed = line.trim();

      // Track empty lines - they separate body from footers
      if trimmed.is_empty() {
        seen_empty_line = true;
        continue;
      }

      // Check for footer format: "Key: value" or "BREAKING CHANGE: description"
      // Footers must come after an empty line
      if seen_empty_line && let Some((key, value)) = trimmed.split_once(':') {
        let key_trimmed = key.trim();
        let value_trimmed = value.trim();

        if key_trimmed.eq_ignore_ascii_case("BREAKING CHANGE") || key_trimmed.eq_ignore_ascii_case("BREAKING-CHANGE") {
          breaking_change = Some(value_trimmed.to_string());
          in_body = false;
          continue;
        } else if key_trimmed.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
          footers.push((key_trimmed.to_string(), value_trimmed.to_string()));
          in_body = false;
          continue;
        }
      }

      // Otherwise, it's part of the body
      if in_body {
        body_lines.push(line);
        seen_empty_line = false; // Reset when we see body content
      }
    }

    // If breaking_change is still None but we had a ! indicator, set it to empty
    if breaking_change.is_none() && breaking_indicator.is_some() {
      breaking_change = Some(String::new());
    }

    let body = if body_lines.is_empty() {
      None
    } else {
      Some(body_lines.join("\n"))
    };

    Some(Self {
      commit_type,
      scope: scope.map(|s: &str| s.to_string()),
      description: description.trim().to_string(),
      body,
      breaking_change,
      footers,
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_commit_type_parsing() {
    assert_eq!(CommitType::from_str("feat"), CommitType::Feat);
    assert_eq!(CommitType::from_str("FEAT"), CommitType::Feat);
    assert_eq!(CommitType::from_str("fix"), CommitType::Fix);
    assert_eq!(CommitType::from_str("docs"), CommitType::Docs);
    assert_eq!(CommitType::from_str("unknown"), CommitType::Other);
  }

  #[test]
  fn test_commit_type_user_facing() {
    assert!(CommitType::Feat.is_user_facing());
    assert!(CommitType::Fix.is_user_facing());
    assert!(CommitType::Perf.is_user_facing());
    assert!(!CommitType::Chore.is_user_facing());
    assert!(!CommitType::Docs.is_user_facing());
  }

  #[test]
  fn test_parse_simple_commit() {
    let msg = "feat: add new feature";
    let commit = ConventionalCommit::parse(msg).unwrap();

    assert_eq!(commit.commit_type, CommitType::Feat);
    assert_eq!(commit.scope, None);
    assert_eq!(commit.description, "add new feature");
    assert_eq!(commit.body, None);
    assert_eq!(commit.breaking_change, None);
    assert!(!commit.is_breaking());
  }

  #[test]
  fn test_parse_commit_with_scope() {
    let msg = "fix(auth): resolve login issue";
    let commit = ConventionalCommit::parse(msg).unwrap();

    assert_eq!(commit.commit_type, CommitType::Fix);
    assert_eq!(commit.scope, Some("auth".to_string()));
    assert_eq!(commit.description, "resolve login issue");
  }

  #[test]
  fn test_parse_commit_with_body() {
    let msg = "feat: add OAuth support\n\nThis adds OAuth2 authentication support.";
    let commit = ConventionalCommit::parse(msg).unwrap();

    assert_eq!(commit.commit_type, CommitType::Feat);
    assert_eq!(commit.description, "add OAuth support");
    assert_eq!(
      commit.body,
      Some("This adds OAuth2 authentication support.".to_string())
    );
  }

  #[test]
  fn test_parse_commit_with_breaking_change() {
    let msg = "feat!: complete redesign\n\nBREAKING CHANGE: API completely redesigned";
    let commit = ConventionalCommit::parse(msg).unwrap();

    assert_eq!(commit.commit_type, CommitType::Feat);
    assert_eq!(commit.breaking_change, Some("API completely redesigned".to_string()));
    assert!(commit.is_breaking());
  }

  #[test]
  fn test_parse_commit_with_footers() {
    let msg = "fix: resolve bug\n\nCloses: #123\nReviewed-by: Alice";
    let commit = ConventionalCommit::parse(msg).unwrap();

    assert_eq!(commit.footers.len(), 2);
    assert_eq!(commit.footers[0], ("Closes".to_string(), "#123".to_string()));
    assert_eq!(commit.footers[1], ("Reviewed-by".to_string(), "Alice".to_string()));
  }

  #[test]
  fn test_parse_non_conventional_commit() {
    let msg = "This is not a conventional commit";
    assert_eq!(ConventionalCommit::parse(msg), None);
  }

  #[test]
  fn test_parse_malformed_commit() {
    let msg = "feat missing colon";
    assert_eq!(ConventionalCommit::parse(msg), None);
  }

  #[test]
  fn test_commit_type_display() {
    assert_eq!(CommitType::Feat.to_string(), "Features");
    assert_eq!(CommitType::Fix.to_string(), "Bug Fixes");
    assert_eq!(CommitType::Docs.to_string(), "Documentation");
  }

  #[test]
  fn test_changelog_creation() {
    let changelog = Changelog::new("1.0.0".to_string(), "2025-01-15".to_string());

    assert_eq!(changelog.version, "1.0.0");
    assert_eq!(changelog.date, "2025-01-15");
    assert_eq!(changelog.commits_by_type.len(), 0);
    assert_eq!(changelog.all_commit_shas.len(), 0);
  }

  #[test]
  fn test_changelog_add_commits() {
    let mut changelog = Changelog::new("1.0.0".to_string(), "2025-01-15".to_string());

    let commit1 = ConventionalCommit::parse("feat: add feature").unwrap();
    changelog.add_commit(commit1, "abc123".to_string());

    let commit2 = ConventionalCommit::parse("fix: fix bug").unwrap();
    changelog.add_commit(commit2, "def456".to_string());

    assert_eq!(changelog.all_commit_shas.len(), 2);
    assert_eq!(changelog.commits_by_type.len(), 2);
    assert!(changelog.commits_by_type.contains_key(&CommitType::Feat));
    assert!(changelog.commits_by_type.contains_key(&CommitType::Fix));
  }

  #[test]
  fn test_changelog_has_user_facing_changes() {
    let mut changelog = Changelog::new("1.0.0".to_string(), "2025-01-15".to_string());

    // No changes yet
    assert!(!changelog.has_user_facing_changes());

    // Add a chore (not user-facing)
    let commit = ConventionalCommit::parse("chore: update deps").unwrap();
    changelog.add_commit(commit, "abc123".to_string());
    assert!(!changelog.has_user_facing_changes());

    // Add a feat (user-facing)
    let commit = ConventionalCommit::parse("feat: add feature").unwrap();
    changelog.add_commit(commit, "def456".to_string());
    assert!(changelog.has_user_facing_changes());
  }

  #[test]
  fn test_changelog_to_markdown() {
    let mut changelog = Changelog::new("1.0.0".to_string(), "2025-01-15".to_string());

    let commit1 = ConventionalCommit::parse("feat(auth): add OAuth").unwrap();
    changelog.add_commit(commit1, "abc123".to_string());

    let commit2 = ConventionalCommit::parse("fix: resolve bug").unwrap();
    changelog.add_commit(commit2, "def456".to_string());

    let markdown = changelog.to_markdown();

    assert!(markdown.contains("## [1.0.0] - 2025-01-15"));
    assert!(markdown.contains("### Features"));
    assert!(markdown.contains("**auth**: add OAuth"));
    assert!(markdown.contains("### Bug Fixes"));
    assert!(markdown.contains("resolve bug"));
  }

  #[test]
  fn test_changelog_to_markdown_with_breaking_change() {
    let mut changelog = Changelog::new("2.0.0".to_string(), "2025-01-15".to_string());

    let commit = ConventionalCommit::parse("feat!: complete redesign\n\nBREAKING CHANGE: API changed").unwrap();
    changelog.add_commit(commit, "abc123".to_string());

    let markdown = changelog.to_markdown();

    assert!(markdown.contains("complete redesign"));
    assert!(markdown.contains("**BREAKING**: API changed"));
  }

  #[test]
  fn test_changelog_to_json() {
    let mut changelog = Changelog::new("1.0.0".to_string(), "2025-01-15".to_string());

    let commit = ConventionalCommit::parse("feat: add feature").unwrap();
    changelog.add_commit(commit, "abc123".to_string());

    let json = changelog.to_json().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed["version"], "1.0.0");
    assert_eq!(parsed["date"], "2025-01-15");
    assert_eq!(parsed["total_commits"], 1);
    assert_eq!(parsed["sections"][0]["commit_type"], "Features");
  }

  #[test]
  fn test_changelog_render() {
    let mut changelog = Changelog::new("1.0.0".to_string(), "2025-01-15".to_string());
    let commit = ConventionalCommit::parse("feat: add feature").unwrap();
    changelog.add_commit(commit, "abc123".to_string());

    // Test markdown rendering
    let markdown = changelog.render(ChangelogFormat::Markdown).unwrap();
    assert!(markdown.contains("## [1.0.0]"));

    // Test JSON rendering
    let json = changelog.render(ChangelogFormat::Json).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["version"], "1.0.0");
  }
}
