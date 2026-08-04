//! Declarative changelog engine
//!
//! Shape is configured in `[release.changelog]` (workspace defaults) and
//! `[crates.NAME.changelog]` (per-crate overrides), resolved once per release
//! into a [`ChangelogSpec`]: commit types map to sections through a pre-built
//! hash lookup, and the entry template is pre-compiled. Rendering a version
//! section is a single pass over attributed commits.
//!
//! Supply-chain stance: winnow for parsing, no template engine.

pub mod parse;
mod render;

pub use parse::{ParsedSubject, parse_subject, suggest_type};
pub use render::{EntryTemplate, LinkTemplates};

use crate::config::{ChangelogConfig, ChangelogShape, GroupSpec};
use crate::error::{ConfigError, RailError, RailResult};
use rustc_hash::{FxHashMap, FxHashSet};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::fmt::Write;
use std::path::Path;

/// Pseudo-type all breaking commits classify as, regardless of declared type
const BREAKING_TYPE: &str = "breaking";
/// Type used for synthesized dependency-update entries
const DEPS_TYPE: &str = "deps";

/// Built-in commit types: (type, title, emoji)
const BUILTIN_TYPES: &[(&str, &str, &str)] = &[
  ("breaking", "BREAKING CHANGES", "⚠️"),
  ("feat", "Features", "✨"),
  ("fix", "Bug Fixes", "🐛"),
  ("perf", "Performance", "⚡"),
  ("docs", "Documentation", "📝"),
  ("chore", "Chores", "🔧"),
  ("refactor", "Refactoring", "♻️"),
  ("style", "Styling", "💄"),
  ("test", "Testing", "✅"),
  ("build", "Build", "🏗️"),
  ("ci", "CI", "👷"),
  ("deps", "Dependencies", "📦"),
  ("other", "Other Changes", "📦"),
];

/// A resolved changelog section
#[derive(Debug, Clone)]
struct GroupDef {
  title: String,
  emoji: Option<String>,
}

/// One commit as input to rendering/diagnostics
#[derive(Debug, Clone, Copy)]
pub struct CommitRef<'a> {
  /// Full commit SHA
  pub sha: &'a str,
  /// Commit subject line
  pub subject: &'a str,
  /// Commit body (trimmed), if any
  pub body: Option<&'a str>,
}

/// A diagnostic for a commit that does not classify cleanly
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommitDiagnostic {
  /// Short commit SHA
  pub sha: String,
  /// Commit subject
  pub subject: String,
  /// What went wrong
  pub issue: CommitIssue,
}

/// Why a commit was flagged
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommitIssue {
  /// Subject does not match `type(scope)!: description`
  NotConventional,
  /// Conventional syntax but the type is not a known or configured type
  UnknownType {
    /// The unrecognized type token
    found: String,
    /// Closest known type, if within edit distance
    suggestion: Option<String>,
  },
}

impl CommitDiagnostic {
  /// Human-readable one-line description
  pub fn describe(&self) -> String {
    match &self.issue {
      CommitIssue::NotConventional => {
        format!("{}: not a conventional commit: \"{}\"", self.sha, self.subject)
      }
      CommitIssue::UnknownType { found, suggestion } => match suggestion {
        Some(known) => format!(
          "{}: unknown commit type '{}' (did you mean '{}'?): \"{}\"",
          self.sha, found, known, self.subject
        ),
        None => format!("{}: unknown commit type '{}': \"{}\"", self.sha, found, self.subject),
      },
    }
  }
}

/// Resolved changelog shape for one crate
///
/// Built once per (workspace shape, per-crate overrides) pair; classification
/// is an O(1) hash lookup and rendering is a single buffer pass.
#[derive(Debug, Clone)]
pub struct ChangelogSpec {
  /// Sections in render order
  groups: Vec<GroupDef>,
  /// Commit type -> index into `groups` (only ordered types present)
  type_to_group: FxHashMap<Box<str>, usize>,
  /// Where unlisted/unknown types go; `None` drops them
  fallback: Option<usize>,
  /// Pre-compiled entry template
  template: EntryTemplate,
  /// Render emoji in section headers
  emoji: bool,
  /// Commit types dropped before grouping (breaking commits are exempt)
  skip_types: FxHashSet<Box<str>>,
  /// Commit scopes dropped before grouping
  skip_scopes: FxHashSet<Box<str>>,
  /// Every declared type (built-in + custom), sorted, for diagnostics
  known_types: Vec<Box<str>>,
}

impl ChangelogSpec {
  /// Resolve workspace shape + optional per-crate overrides into a spec
  ///
  /// Fails with [`ConfigError::InvalidField`] on unknown types in
  /// `group_order`, an unresolvable `fallback`, or a bad `entry_format`.
  pub fn resolve(shape: &ChangelogShape, overrides: Option<&ChangelogConfig>) -> RailResult<Self> {
    // Effective values: crate override wins, workspace shape is the default.
    let entry_format = overrides
      .and_then(|o| o.entry_format.as_deref())
      .unwrap_or(&shape.entry_format);
    let emoji = overrides.and_then(|o| o.emoji).unwrap_or(shape.emoji);
    let group_order = overrides
      .and_then(|o| o.group_order.as_deref())
      .unwrap_or(&shape.group_order);
    let fallback = overrides.and_then(|o| o.fallback.as_deref()).unwrap_or(&shape.fallback);
    let filters = overrides.and_then(|o| o.filters.as_ref()).unwrap_or(&shape.filters);

    // Type definitions: built-ins, then workspace groups, then crate groups.
    // Later definitions override earlier ones per type.
    let mut definitions: Vec<GroupDef> = Vec::with_capacity(BUILTIN_TYPES.len() + shape.groups.len());
    let mut type_to_def: FxHashMap<Box<str>, usize> = FxHashMap::default();
    for (commit_type, title, emoji_str) in BUILTIN_TYPES {
      type_to_def.insert(Box::from(*commit_type), definitions.len());
      definitions.push(GroupDef {
        title: (*title).to_string(),
        emoji: Some((*emoji_str).to_string()),
      });
    }
    let crate_groups = overrides.map(|o| o.groups.as_slice()).unwrap_or_default();
    for spec in shape.groups.iter().chain(crate_groups) {
      Self::add_custom_group(spec, &mut definitions, &mut type_to_def)?;
    }

    // Order sections: first appearance of a definition in group_order wins.
    let mut groups: Vec<GroupDef> = Vec::with_capacity(group_order.len());
    let mut def_to_group: FxHashMap<usize, usize> = FxHashMap::default();
    for type_key in group_order {
      let Some(&def_idx) = type_to_def.get(type_key.as_str()) else {
        return Err(RailError::Config(ConfigError::InvalidField {
          field: "release.changelog.group_order".to_string(),
          reason: format!(
            "unknown commit type '{}'; define it with [[release.changelog.groups]] (types = [\"{}\"])",
            type_key, type_key
          ),
        }));
      };
      def_to_group.entry(def_idx).or_insert_with(|| {
        groups.push(definitions[def_idx].clone());
        groups.len() - 1
      });
    }

    // Map every type whose definition got ordered.
    let mut type_to_group: FxHashMap<Box<str>, usize> = FxHashMap::default();
    for (type_key, def_idx) in &type_to_def {
      if let Some(&group_idx) = def_to_group.get(def_idx) {
        type_to_group.insert(type_key.clone(), group_idx);
      }
    }

    let fallback = match fallback {
      "skip" => None,
      key => Some(*type_to_group.get(key).ok_or_else(|| {
        RailError::Config(ConfigError::InvalidField {
          field: "release.changelog.fallback".to_string(),
          reason: format!("fallback '{}' must be \"skip\" or a type listed in group_order", key),
        })
      })?),
    };

    let template = EntryTemplate::parse(entry_format).map_err(|reason| {
      RailError::Config(ConfigError::InvalidField {
        field: "release.changelog.entry_format".to_string(),
        reason,
      })
    })?;

    let mut known_types: Vec<Box<str>> = type_to_def.keys().cloned().collect();
    known_types.sort_unstable();

    Ok(Self {
      groups,
      type_to_group,
      fallback,
      template,
      emoji,
      skip_types: to_boxed_set(&filters.skip_types),
      skip_scopes: to_boxed_set(&filters.skip_scopes),
      known_types,
    })
  }

  fn add_custom_group(
    spec: &GroupSpec,
    definitions: &mut Vec<GroupDef>,
    type_to_def: &mut FxHashMap<Box<str>, usize>,
  ) -> RailResult<()> {
    if spec.types.is_empty() {
      return Err(RailError::Config(ConfigError::InvalidField {
        field: "release.changelog.groups".to_string(),
        reason: format!("group '{}' must declare at least one commit type", spec.title),
      }));
    }
    let def_idx = definitions.len();
    definitions.push(GroupDef {
      title: spec.title.clone(),
      emoji: spec.emoji.clone(),
    });
    for commit_type in &spec.types {
      type_to_def.insert(Box::from(commit_type.as_str()), def_idx);
    }
    Ok(())
  }

  /// Classify one parsed commit into a section
  ///
  /// Breaking commits always classify as "breaking" and are exempt from
  /// `skip_types` — breaking changes are never silently dropped.
  fn classify(&self, parsed: &ParsedSubject<'_>) -> Option<usize> {
    if let Some(scope) = parsed.scope
      && self.skip_scopes.contains(scope)
      && !parsed.breaking
    {
      return None;
    }

    if parsed.breaking {
      return self.type_to_group.get(BREAKING_TYPE).copied().or(self.fallback);
    }

    let Some(commit_type) = parsed.commit_type else {
      return self.fallback;
    };
    if self.skip_types.contains(commit_type) {
      return None;
    }
    self.type_to_group.get(commit_type).copied().or(self.fallback)
  }

  /// Render one version's section body from attributed commits
  ///
  /// - `highlights`: change-file summaries, rendered verbatim before sections
  /// - `commits`: newest-first commit list for this crate's release range
  /// - `dependency_updates`: (name, version) pairs synthesized into the
  ///   "deps" section for crates released only through dependent closure
  pub fn render(
    &self,
    links: &LinkTemplates,
    highlights: &[String],
    commits: &[CommitRef<'_>],
    dependency_updates: &[(String, Version)],
  ) -> String {
    // Bucket commit indexes per section, preserving commit order.
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); self.groups.len()];
    for (idx, commit) in commits.iter().enumerate() {
      let parsed = parse_subject(commit.subject, commit.body);
      if let Some(group) = self.classify(&parsed) {
        buckets[group].push(idx);
      }
    }

    let deps_group = (!dependency_updates.is_empty())
      .then(|| self.type_to_group.get(DEPS_TYPE).copied().or(self.fallback))
      .flatten();

    let mut out = String::with_capacity(commits.len() * 96 + 64);

    for highlight in highlights {
      let trimmed = highlight.trim();
      if !trimmed.is_empty() {
        out.push_str(trimmed);
        out.push_str("\n\n");
      }
    }

    for (group_idx, group) in self.groups.iter().enumerate() {
      let bucket = &buckets[group_idx];
      let synthesized = deps_group == Some(group_idx);
      if bucket.is_empty() && !synthesized {
        continue;
      }

      self.write_section_header(&mut out, group);
      for &commit_idx in bucket {
        let commit = &commits[commit_idx];
        let parsed = parse_subject(commit.subject, commit.body);
        self.template.render(&mut out, &parsed, commit.sha, commit.body, links);
        out.push('\n');
      }
      if synthesized {
        for (name, version) in dependency_updates {
          let _ = writeln!(out, "- updated {} to {}", name, version);
        }
      }
      out.push('\n');
    }

    out
  }

  fn write_section_header(&self, out: &mut String, group: &GroupDef) {
    match (&group.emoji, self.emoji) {
      (Some(emoji), true) => {
        let _ = write!(out, "### {} {}\n\n", emoji, group.title);
      }
      _ => {
        let _ = write!(out, "### {}\n\n", group.title);
      }
    }
  }

  /// Scan commits for conventional-commit problems
  ///
  /// Flags unparseable subjects and unknown type tokens with a near-miss
  /// suggestion where one exists.
  pub fn diagnose(&self, commits: &[CommitRef<'_>]) -> Vec<CommitDiagnostic> {
    let mut diagnostics = Vec::new();
    for commit in commits {
      let parsed = parse_subject(commit.subject, commit.body);
      let issue = match parsed.commit_type {
        None => Some(CommitIssue::NotConventional),
        Some(commit_type) if !self.known_types.iter().any(|t| &**t == commit_type) => Some(CommitIssue::UnknownType {
          found: commit_type.to_string(),
          suggestion: suggest_type(commit_type, self.known_types.iter().map(|t| &**t)).map(str::to_string),
        }),
        Some(_) => None,
      };
      if let Some(issue) = issue {
        diagnostics.push(CommitDiagnostic {
          sha: commit.sha.get(..7).unwrap_or(commit.sha).to_string(),
          subject: commit.subject.to_string(),
          issue,
        });
      }
    }
    diagnostics
  }

  /// Structured entries for machine output (`release plan --format json`)
  pub fn entries_json(&self, commits: &[CommitRef<'_>]) -> Vec<serde_json::Value> {
    commits
      .iter()
      .filter_map(|commit| {
        let parsed = parse_subject(commit.subject, commit.body);
        self.classify(&parsed).map(|group| {
          serde_json::json!({
            "sha": commit.sha,
            "type": parsed.commit_type.unwrap_or("other"),
            "scope": parsed.scope,
            "breaking": parsed.breaking,
            "description": parsed.description,
            "section": self.groups[group].title,
          })
        })
      })
      .collect()
  }
}

/// Build link templates from workspace/per-crate config, falling back to GitHub remote detection
pub fn resolve_links(
  shape: &ChangelogShape,
  overrides: Option<&ChangelogConfig>,
  workspace_root: &Path,
) -> RailResult<LinkTemplates> {
  let commit_url = overrides
    .and_then(|o| o.commit_url.as_deref())
    .or(shape.commit_url.as_deref());
  let pr_url = overrides.and_then(|o| o.pr_url.as_deref()).or(shape.pr_url.as_deref());

  if commit_url.is_some() || pr_url.is_some() {
    return LinkTemplates::new(commit_url, pr_url).map_err(|reason| {
      RailError::Config(ConfigError::InvalidField {
        field: "release.changelog.commit_url".to_string(),
        reason,
      })
    });
  }

  Ok(match detect_github_repo(workspace_root) {
    Some((org, repo)) => LinkTemplates::for_github(&org, &repo),
    None => LinkTemplates::default(),
  })
}

/// Detect GitHub repository from the git remote
pub fn detect_github_repo(workspace_root: &Path) -> Option<(String, String)> {
  let output =
    crate::release::process::run("git", &["config", "--get", "remote.origin.url"], Some(workspace_root)).ok()?;

  if !output.status.success() {
    return None;
  }

  let url = String::from_utf8_lossy(&output.stdout);
  parse_github_remote(&url)
}

/// Parse a GitHub remote URL into (org, repo)
fn parse_github_remote(url: &str) -> Option<(String, String)> {
  let trimmed = url.trim().trim_end_matches('/');
  let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);

  let repo_part = if let Some(ssh) = trimmed.strip_prefix("git@github.com:") {
    ssh
  } else if let Some(ssh) = trimmed.strip_prefix("ssh://git@github.com/") {
    ssh
  } else {
    trimmed.strip_prefix("https://github.com/")?
  };

  let (org, repo) = repo_part.split_once('/')?;
  if org.is_empty() || repo.is_empty() || repo.contains('/') || org.contains(['?', '#']) || repo.contains(['?', '#']) {
    return None;
  }

  Some((org.to_string(), repo.to_string()))
}

fn to_boxed_set(values: &[String]) -> FxHashSet<Box<str>> {
  values.iter().map(|v| Box::from(v.as_str())).collect()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::ChangelogShape;

  fn default_spec() -> ChangelogSpec {
    ChangelogSpec::resolve(&ChangelogShape::default(), None).unwrap()
  }

  fn commit<'a>(sha: &'a str, subject: &'a str) -> CommitRef<'a> {
    CommitRef {
      sha,
      subject,
      body: None,
    }
  }

  #[test]
  fn default_render_matches_legacy_shape() {
    let spec = default_spec();
    let links = LinkTemplates::for_github("org", "repo");
    let commits = [
      commit("aaaaaaa1111111", "feat: add planner"),
      commit("bbbbbbb2222222", "fix(core): null deref"),
      commit("ccccccc3333333", "feat!: drop old API"),
      commit("ddddddd4444444", "Update README"),
    ];

    let body = spec.render(&links, &[], &commits, &[]);

    let breaking_pos = body.find("### ⚠️ BREAKING CHANGES").unwrap();
    let features_pos = body.find("### ✨ Features").unwrap();
    let fixes_pos = body.find("### 🐛 Bug Fixes").unwrap();
    let other_pos = body.find("### 📦 Other Changes").unwrap();
    assert!(breaking_pos < features_pos);
    assert!(features_pos < fixes_pos);
    assert!(fixes_pos < other_pos);

    assert!(
      body.contains("- [**breaking**] drop old API ([ccccccc](https://github.com/org/repo/commit/ccccccc3333333))")
    );
    assert!(body.contains("- **core**: null deref"));
    assert!(body.contains("- Update README"));
  }

  #[test]
  fn custom_group_and_order() {
    let mut shape = ChangelogShape::default();
    shape.groups.push(GroupSpec {
      types: vec!["sec".to_string(), "security".to_string()],
      title: "Security".to_string(),
      emoji: Some("🔒".to_string()),
    });
    shape.group_order = vec!["sec".to_string(), "feat".to_string(), "other".to_string()];
    let spec = ChangelogSpec::resolve(&shape, None).unwrap();

    let commits = [
      commit("aaaaaaa1111111", "feat: add planner"),
      commit("bbbbbbb2222222", "security: rotate keys"),
      commit("ccccccc3333333", "sec(auth): patch leak"),
    ];
    let body = spec.render(&LinkTemplates::default(), &[], &commits, &[]);

    let sec_pos = body.find("### 🔒 Security").unwrap();
    let feat_pos = body.find("### ✨ Features").unwrap();
    assert!(sec_pos < feat_pos);
    // Both aliases render into the shared section, once.
    assert_eq!(body.matches("### 🔒 Security").count(), 1);
    assert!(body.contains("- rotate keys"));
    assert!(body.contains("- **auth**: patch leak"));
  }

  #[test]
  fn unordered_types_fall_through_to_fallback() {
    let shape = ChangelogShape {
      group_order: vec!["feat".to_string(), "other".to_string()],
      ..ChangelogShape::default()
    };
    let spec = ChangelogSpec::resolve(&shape, None).unwrap();

    let commits = [commit("aaaaaaa1111111", "chore: tidy")];
    let body = spec.render(&LinkTemplates::default(), &[], &commits, &[]);
    assert!(body.contains("### 📦 Other Changes"));
    assert!(body.contains("- tidy"));
  }

  #[test]
  fn fallback_skip_drops_unlisted_types() {
    let shape = ChangelogShape {
      group_order: vec!["feat".to_string()],
      fallback: "skip".to_string(),
      ..ChangelogShape::default()
    };
    let spec = ChangelogSpec::resolve(&shape, None).unwrap();

    let commits = [
      commit("aaaaaaa1111111", "chore: tidy"),
      commit("bbbbbbb2222222", "Update README"),
      commit("ccccccc3333333", "feat: keep me"),
    ];
    let body = spec.render(&LinkTemplates::default(), &[], &commits, &[]);
    assert!(!body.contains("tidy"));
    assert!(!body.contains("README"));
    assert!(body.contains("- keep me"));
  }

  #[test]
  fn skip_types_never_drop_breaking_commits() {
    let mut shape = ChangelogShape::default();
    shape.filters.skip_types = vec!["feat".to_string()];
    let spec = ChangelogSpec::resolve(&shape, None).unwrap();

    let commits = [
      commit("aaaaaaa1111111", "feat: dropped"),
      commit("bbbbbbb2222222", "feat!: kept, breaking"),
    ];
    let body = spec.render(&LinkTemplates::default(), &[], &commits, &[]);
    assert!(!body.contains("dropped"));
    assert!(body.contains("[**breaking**] kept, breaking"));
  }

  #[test]
  fn skip_scopes_filter_commits() {
    let mut shape = ChangelogShape::default();
    shape.filters.skip_scopes = vec!["internal".to_string()];
    let spec = ChangelogSpec::resolve(&shape, None).unwrap();

    let commits = [
      commit("aaaaaaa1111111", "feat(internal): hidden"),
      commit("bbbbbbb2222222", "feat(api): visible"),
    ];
    let body = spec.render(&LinkTemplates::default(), &[], &commits, &[]);
    assert!(!body.contains("hidden"));
    assert!(body.contains("**api**: visible"));
  }

  #[test]
  fn emoji_off_renders_plain_headers() {
    let shape = ChangelogShape {
      emoji: false,
      ..ChangelogShape::default()
    };
    let spec = ChangelogSpec::resolve(&shape, None).unwrap();

    let commits = [commit("aaaaaaa1111111", "feat: add planner")];
    let body = spec.render(&LinkTemplates::default(), &[], &commits, &[]);
    assert!(body.contains("### Features\n"));
    assert!(!body.contains("✨"));
  }

  #[test]
  fn crate_overrides_win_over_workspace_shape() {
    let shape = ChangelogShape::default();
    let overrides = ChangelogConfig {
      emoji: Some(false),
      group_order: Some(vec!["fix".to_string(), "other".to_string()]),
      ..ChangelogConfig::default()
    };
    let spec = ChangelogSpec::resolve(&shape, Some(&overrides)).unwrap();

    let commits = [
      commit("aaaaaaa1111111", "feat: goes to other"),
      commit("bbbbbbb2222222", "fix: stays first"),
    ];
    let body = spec.render(&LinkTemplates::default(), &[], &commits, &[]);
    let fixes_pos = body.find("### Bug Fixes").unwrap();
    let other_pos = body.find("### Other Changes").unwrap();
    assert!(fixes_pos < other_pos);
    assert!(body.contains("- goes to other"));
  }

  #[test]
  fn synthesized_dependency_entries() {
    let spec = default_spec();
    let updates = vec![("rail-core".to_string(), Version::new(0, 5, 0))];
    let body = spec.render(&LinkTemplates::default(), &[], &[], &updates);
    assert!(body.contains("### 📦 Dependencies"));
    assert!(body.contains("- updated rail-core to 0.5.0"));
  }

  #[test]
  fn highlights_render_before_sections() {
    let spec = default_spec();
    let highlights = vec!["Added `--bump auto` for per-crate version inference.".to_string()];
    let commits = [commit("aaaaaaa1111111", "feat: add planner")];
    let body = spec.render(&LinkTemplates::default(), &highlights, &commits, &[]);
    let highlight_pos = body.find("Added `--bump auto`").unwrap();
    let section_pos = body.find("### ✨ Features").unwrap();
    assert!(highlight_pos < section_pos);
  }

  #[test]
  fn unknown_group_order_type_is_config_error() {
    let shape = ChangelogShape {
      group_order: vec!["sec".to_string()],
      ..ChangelogShape::default()
    };
    let err = ChangelogSpec::resolve(&shape, None).unwrap_err();
    assert!(err.to_string().contains("sec"));
  }

  #[test]
  fn bad_fallback_is_config_error() {
    let shape = ChangelogShape {
      fallback: "nope".to_string(),
      ..ChangelogShape::default()
    };
    let err = ChangelogSpec::resolve(&shape, None).unwrap_err();
    assert!(err.to_string().contains("fallback"));
  }

  #[test]
  fn diagnose_flags_typos_and_unconventional() {
    let spec = default_spec();
    let commits = [
      commit("aaaaaaa1111111", "hore(ci): avoid dry-run hooks"),
      commit("bbbbbbb2222222", "Update README"),
      commit("ccccccc3333333", "feat: fine"),
    ];
    let diagnostics = spec.diagnose(&commits);
    assert_eq!(diagnostics.len(), 2);
    assert_eq!(
      diagnostics[0].issue,
      CommitIssue::UnknownType {
        found: "hore".to_string(),
        suggestion: Some("chore".to_string()),
      }
    );
    assert_eq!(diagnostics[1].issue, CommitIssue::NotConventional);
    assert!(diagnostics[0].describe().contains("did you mean 'chore'"));
  }

  #[test]
  fn entries_json_is_structured() {
    let spec = default_spec();
    let commits = [commit("aaaaaaa1111111", "feat(api): add endpoint")];
    let entries = spec.entries_json(&commits);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["type"], "feat");
    assert_eq!(entries[0]["scope"], "api");
    assert_eq!(entries[0]["section"], "Features");
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
    assert_eq!(
      parse_github_remote("https://github.com/org/repo.git/"),
      Some(("org".to_string(), "repo".to_string()))
    );
    assert_eq!(parse_github_remote("git@gitlab.com:org/repo.git"), None);
  }

  #[test]
  fn parse_github_remote_rejects_non_repository_paths() {
    assert_eq!(parse_github_remote("https://github.com/org/repo/issues"), None);
    assert_eq!(parse_github_remote("https://github.com//repo.git"), None);
    assert_eq!(parse_github_remote("https://github.com/org/.git"), None);
    assert_eq!(parse_github_remote("https://github.com/org/repo?tab=readme"), None);
    assert_eq!(parse_github_remote("https://github.com/org/repo#readme"), None);
  }

  #[test]
  fn crate_link_overrides_win() {
    let shape = ChangelogShape::default();
    let overrides = ChangelogConfig {
      commit_url: Some("https://git.example/{sha}".to_string()),
      pr_url: Some("https://git.example/pr/{pr}".to_string()),
      ..ChangelogConfig::default()
    };
    let links = resolve_links(&shape, Some(&overrides), Path::new(".")).unwrap();
    let mut out = String::new();
    links.write_sha_link(&mut out, "abcdef1234");
    assert_eq!(out, "[abcdef1](https://git.example/abcdef1234)");
  }
}
