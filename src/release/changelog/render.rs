//! Entry rendering from pre-compiled templates
//!
//! `entry_format` is parsed once per release into [`EntryTemplate`] segments;
//! rendering an entry is a walk over segments writing into a shared buffer —
//! no per-entry parsing or intermediate allocation.

use super::parse::{ParsedSubject, extract_pr_numbers};
use std::fmt::Write;

/// A pre-parsed entry template segment
#[derive(Debug, Clone, PartialEq)]
enum Segment {
  /// Literal text between placeholders
  Literal(String),
  /// "**scope**: " when the commit has a scope
  Scope,
  /// "[**breaking**] " for breaking changes
  Breaking,
  /// Commit description
  Description,
  /// " #12 #34" (linked when a PR URL is known), empty when none
  Prs,
  /// Short commit SHA
  Sha,
  /// Short SHA linked to the commit when a commit URL is known
  ShaLink,
  /// Parsed commit type ("other" for unconventional commits)
  Type,
}

/// Pre-compiled `entry_format` template
#[derive(Debug, Clone)]
pub struct EntryTemplate {
  segments: Vec<Segment>,
}

impl EntryTemplate {
  /// Parse an `entry_format` string into segments
  ///
  /// Unknown placeholders are configuration errors; the message lists the
  /// valid set.
  pub fn parse(format: &str) -> Result<Self, String> {
    let mut segments = Vec::new();
    let mut rest = format;

    while let Some(open) = rest.find('{') {
      if !rest[..open].is_empty() {
        segments.push(Segment::Literal(rest[..open].to_string()));
      }
      let after_open = &rest[open + 1..];
      let Some(close) = after_open.find('}') else {
        return Err(format!("unclosed placeholder in entry_format: '{}'", &rest[open..]));
      };
      let name = &after_open[..close];
      let segment = match name {
        "scope" => Segment::Scope,
        "breaking" => Segment::Breaking,
        "description" => Segment::Description,
        "prs" => Segment::Prs,
        "sha" => Segment::Sha,
        "sha_link" => Segment::ShaLink,
        "type" => Segment::Type,
        other => {
          return Err(format!(
            "unknown placeholder '{{{}}}' in entry_format; valid: {{scope}}, {{breaking}}, {{description}}, {{prs}}, {{sha}}, {{sha_link}}, {{type}}",
            other
          ));
        }
      };
      segments.push(segment);
      rest = &after_open[close + 1..];
    }
    if !rest.is_empty() {
      segments.push(Segment::Literal(rest.to_string()));
    }

    Ok(Self { segments })
  }

  /// Render one entry line (without trailing newline) into `out`
  pub fn render(
    &self,
    out: &mut String,
    parsed: &ParsedSubject<'_>,
    sha: &str,
    body: Option<&str>,
    links: &LinkTemplates,
  ) {
    for segment in &self.segments {
      match segment {
        Segment::Literal(text) => out.push_str(text),
        Segment::Scope => {
          if let Some(scope) = parsed.scope {
            let _ = write!(out, "**{}**: ", scope);
          }
        }
        Segment::Breaking => {
          if parsed.breaking {
            out.push_str("[**breaking**] ");
          }
        }
        Segment::Description => out.push_str(parsed.description.trim()),
        Segment::Prs => render_prs(out, parsed, body, links),
        Segment::Sha => out.push_str(short_sha(sha)),
        Segment::ShaLink => links.write_sha_link(out, sha),
        Segment::Type => out.push_str(parsed.commit_type.unwrap_or("other")),
      }
    }
  }
}

fn render_prs(out: &mut String, parsed: &ParsedSubject<'_>, body: Option<&str>, links: &LinkTemplates) {
  let mut text = parsed.description.to_string();
  if let Some(body) = body {
    text.push(' ');
    text.push_str(body);
  }
  for pr in extract_pr_numbers(&text) {
    out.push(' ');
    links.write_pr_link(out, pr);
  }
}

fn short_sha(sha: &str) -> &str {
  sha.get(..7).unwrap_or(sha)
}

/// A URL template pre-split around its single placeholder
#[derive(Debug, Clone)]
pub struct UrlTemplate {
  prefix: String,
  suffix: String,
}

impl UrlTemplate {
  /// Split `template` around `placeholder`; errors when the placeholder is absent
  pub fn parse(template: &str, placeholder: &str) -> Result<Self, String> {
    let Some((prefix, suffix)) = template.split_once(placeholder) else {
      return Err(format!("URL template '{}' must contain {}", template, placeholder));
    };
    Ok(Self {
      prefix: prefix.to_string(),
      suffix: suffix.to_string(),
    })
  }

  fn write(&self, out: &mut String, value: impl std::fmt::Display) {
    let _ = write!(out, "{}{}{}", self.prefix, value, self.suffix);
  }
}

/// Resolved commit/PR link templates
///
/// Built from `[release.changelog]` config when set, otherwise inferred from
/// a GitHub `origin` remote. Absent templates render plain text.
#[derive(Debug, Clone, Default)]
pub struct LinkTemplates {
  commit: Option<UrlTemplate>,
  pr: Option<UrlTemplate>,
}

impl LinkTemplates {
  /// Build from explicit templates ({sha} / {pr} placeholders required)
  pub fn new(commit_url: Option<&str>, pr_url: Option<&str>) -> Result<Self, String> {
    Ok(Self {
      commit: commit_url.map(|t| UrlTemplate::parse(t, "{sha}")).transpose()?,
      pr: pr_url.map(|t| UrlTemplate::parse(t, "{pr}")).transpose()?,
    })
  }

  /// Build from a detected GitHub (org, repo)
  pub fn for_github(org: &str, repo: &str) -> Self {
    Self {
      commit: Some(UrlTemplate {
        prefix: format!("https://github.com/{}/{}/commit/", org, repo),
        suffix: String::new(),
      }),
      pr: Some(UrlTemplate {
        prefix: format!("https://github.com/{}/{}/pull/", org, repo),
        suffix: String::new(),
      }),
    }
  }

  pub(crate) fn write_sha_link(&self, out: &mut String, sha: &str) {
    match &self.commit {
      Some(template) => {
        out.push('[');
        out.push_str(short_sha(sha));
        out.push_str("](");
        template.write(out, sha);
        out.push(')');
      }
      None => out.push_str(short_sha(sha)),
    }
  }

  pub(crate) fn write_pr_link(&self, out: &mut String, pr: u32) {
    match &self.pr {
      Some(template) => {
        let _ = write!(out, "[#{}](", pr);
        template.write(out, pr);
        out.push(')');
      }
      None => {
        let _ = write!(out, "#{}", pr);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::release::changelog::parse::parse_subject;

  fn github_links() -> LinkTemplates {
    LinkTemplates::for_github("org", "repo")
  }

  fn render_default(subject: &str, sha: &str, body: Option<&str>, links: &LinkTemplates) -> String {
    let template = EntryTemplate::parse("- {scope}{breaking}{description}{prs} ({sha_link})").unwrap();
    let parsed = parse_subject(subject, body);
    let mut out = String::new();
    template.render(&mut out, &parsed, sha, body, links);
    out
  }

  #[test]
  fn renders_pr_and_commit_links() {
    let line = render_default(
      "feat(api): redesign REST endpoints (#123)",
      "abcdef1234567890",
      Some("closes #456"),
      &github_links(),
    );

    assert!(line.contains("[#123](https://github.com/org/repo/pull/123)"));
    assert!(line.contains("[#456](https://github.com/org/repo/pull/456)"));
    assert!(line.contains("**api**: redesign REST endpoints (#123)"));
    assert!(line.contains("[abcdef1](https://github.com/org/repo/commit/abcdef1234567890)"));
  }

  #[test]
  fn renders_breaking_marker_inline() {
    let line = render_default("feat!: change API", "1234567", None, &LinkTemplates::default());
    assert!(line.contains("[**breaking**] change API"));
  }

  #[test]
  fn renders_plain_refs_without_links() {
    let line = render_default(
      "feat: add feature (#12)",
      "abcdef1234567890",
      Some("follow-up #34"),
      &LinkTemplates::default(),
    );
    assert!(line.contains("#12 #34"));
    assert!(line.contains("(abcdef1)"));
  }

  #[test]
  fn custom_forge_templates() {
    let links = LinkTemplates::new(
      Some("https://gitlab.com/org/repo/-/commit/{sha}"),
      Some("https://gitlab.com/org/repo/-/merge_requests/{pr}"),
    )
    .unwrap();
    let line = render_default("fix: null deref (#7)", "abcdef1234567890", None, &links);
    assert!(line.contains("[#7](https://gitlab.com/org/repo/-/merge_requests/7)"));
    assert!(line.contains("[abcdef1](https://gitlab.com/org/repo/-/commit/abcdef1234567890)"));
  }

  #[test]
  fn rejects_unknown_placeholder() {
    let err = EntryTemplate::parse("- {desc}").unwrap_err();
    assert!(err.contains("{desc}"));
    assert!(err.contains("{description}"));
  }

  #[test]
  fn rejects_url_template_without_placeholder() {
    let err = UrlTemplate::parse("https://example.com/commit", "{sha}").unwrap_err();
    assert!(err.contains("{sha}"));
  }

  #[test]
  fn type_placeholder_renders_parsed_type() {
    let template = EntryTemplate::parse("{type}: {description}").unwrap();
    let parsed = parse_subject("perf(core): faster lookups", None);
    let mut out = String::new();
    template.render(&mut out, &parsed, "1234567", None, &LinkTemplates::default());
    assert_eq!(out, "perf: faster lookups");
  }
}
