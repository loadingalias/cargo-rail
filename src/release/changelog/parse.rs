//! Zero-copy conventional-commit parsing
//!
//! Accepts any commit type token (open set) so custom types configured under
//! `[release.changelog]` parse without code changes. Classification into
//! sections happens against the resolved [`super::ChangelogSpec`].

use winnow::Parser;
use winnow::ascii::{space0, till_line_ending};
use winnow::combinator::{opt, preceded, terminated};
use winnow::token::{take_till, take_until};

// Winnow 0.7.x uses Result, not PResult
type PResult<T> = winnow::error::Result<T>;

/// A parsed conventional commit subject
///
/// Borrowed from the subject/body strings — parsing allocates nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParsedSubject<'a> {
  /// Commit type token (e.g. "feat", "fix", or any custom type).
  /// `None` when the subject is not a conventional commit.
  pub commit_type: Option<&'a str>,
  /// Optional scope
  pub scope: Option<&'a str>,
  /// Whether this is a breaking change (`!` marker or BREAKING CHANGE footer)
  pub breaking: bool,
  /// Commit description (the full subject for unconventional commits)
  pub description: &'a str,
}

impl<'a> ParsedSubject<'a> {
  /// Whether the subject parsed as a conventional commit
  pub fn is_conventional(&self) -> bool {
    self.commit_type.is_some()
  }
}

/// Parse the commit type token: any run of type characters before `(`, `!`, or `:`
fn parse_commit_type<'a>(input: &mut &'a str) -> PResult<&'a str> {
  take_till(1.., ['(', '!', ':'])
    .verify(|value: &str| {
      !value.is_empty()
        && value
          .chars()
          .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    })
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

/// Parse a conventional commit subject
///
/// Format: `type(scope)!: description`. Subjects that do not match return a
/// [`ParsedSubject`] with `commit_type: None` and the full subject as the
/// description — callers decide how to classify those.
pub fn parse_subject<'a>(subject: &'a str, body: Option<&str>) -> ParsedSubject<'a> {
  let has_breaking_body = body
    .map(|b| b.contains("BREAKING CHANGE:") || b.contains("BREAKING-CHANGE:"))
    .unwrap_or(false);

  let mut input = subject;
  let result = (parse_commit_type, parse_scope, parse_breaking, parse_description).parse_next(&mut input);

  match result {
    Ok((commit_type, scope, breaking_marker, description)) => ParsedSubject {
      commit_type: Some(commit_type),
      scope,
      breaking: breaking_marker || has_breaking_body,
      description,
    },
    Err(_) => ParsedSubject {
      commit_type: None,
      scope: None,
      breaking: has_breaking_body,
      description: subject,
    },
  }
}

/// Extract PR references (#123) from text
pub fn extract_pr_numbers(text: &str) -> Vec<u32> {
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

/// Suggest the closest known commit type for a typo'd token
///
/// Returns a known type within edit distance 1 (types up to 4 chars) or
/// 2 (longer types). Ties resolve to the first candidate in `known` order,
/// so callers should pass a deterministically ordered list.
pub fn suggest_type<'a>(unknown: &str, known: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
  let max_distance = 2;
  let mut best: Option<(&str, usize)> = None;

  for candidate in known {
    let distance = edit_distance(unknown, candidate, max_distance);
    if distance <= max_distance && best.is_none_or(|(_, d)| distance < d) {
      best = Some((candidate, distance));
    }
  }

  best.map(|(candidate, _)| candidate)
}

/// Bounded Levenshtein distance; returns `cap + 1` when the distance exceeds `cap`
fn edit_distance(a: &str, b: &str, cap: usize) -> usize {
  let a: Vec<char> = a.chars().collect();
  let b: Vec<char> = b.chars().collect();
  if a.len().abs_diff(b.len()) > cap {
    return cap + 1;
  }

  let mut prev: Vec<usize> = (0..=b.len()).collect();
  let mut curr = vec![0usize; b.len() + 1];

  for (i, ca) in a.iter().enumerate() {
    curr[0] = i + 1;
    let mut row_min = curr[0];
    for (j, cb) in b.iter().enumerate() {
      let cost = usize::from(ca != cb);
      curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
      row_min = row_min.min(curr[j + 1]);
    }
    if row_min > cap {
      return cap + 1;
    }
    std::mem::swap(&mut prev, &mut curr);
  }

  prev[b.len()]
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_feature() {
    let parsed = parse_subject("feat: add new feature", None);
    assert_eq!(parsed.commit_type, Some("feat"));
    assert_eq!(parsed.scope, None);
    assert!(!parsed.breaking);
    assert_eq!(parsed.description, "add new feature");
  }

  #[test]
  fn parses_scope() {
    let parsed = parse_subject("fix(parser): fix parsing bug", None);
    assert_eq!(parsed.commit_type, Some("fix"));
    assert_eq!(parsed.scope, Some("parser"));
    assert_eq!(parsed.description, "fix parsing bug");
  }

  #[test]
  fn parses_breaking_marker() {
    let parsed = parse_subject("feat!: breaking change", None);
    assert_eq!(parsed.commit_type, Some("feat"));
    assert!(parsed.breaking);
    assert_eq!(parsed.description, "breaking change");
  }

  #[test]
  fn parses_breaking_body_footer() {
    let parsed = parse_subject("feat: some feature", Some("BREAKING CHANGE: this breaks stuff"));
    assert!(parsed.breaking);
    assert_eq!(parsed.commit_type, Some("feat"));
  }

  #[test]
  fn parses_custom_type() {
    let parsed = parse_subject("sec(auth): patch token leak", None);
    assert_eq!(parsed.commit_type, Some("sec"));
    assert_eq!(parsed.scope, Some("auth"));
    assert_eq!(parsed.description, "patch token leak");
  }

  #[test]
  fn unconventional_subject_keeps_full_text() {
    let parsed = parse_subject("Update README", None);
    assert_eq!(parsed.commit_type, None);
    assert!(!parsed.is_conventional());
    assert_eq!(parsed.description, "Update README");
  }

  #[test]
  fn subject_with_spaces_before_colon_is_unconventional() {
    let parsed = parse_subject("this is: not conventional", None);
    assert_eq!(parsed.commit_type, None);
    assert_eq!(parsed.description, "this is: not conventional");
  }

  #[test]
  fn extract_pr_numbers_supports_common_patterns() {
    let prs = extract_pr_numbers("feat(auth): add login (#12) closes #34 and refs #34");
    assert_eq!(prs, vec![12, 34]);
  }

  #[test]
  fn suggests_near_miss_types() {
    let known = ["feat", "fix", "chore", "docs"];
    assert_eq!(suggest_type("hore", known), Some("chore"));
    assert_eq!(suggest_type("faet", known), Some("feat"));
    assert_eq!(suggest_type("refactor", known), None);
    assert_eq!(suggest_type("completely-different", known), None);
  }

  #[test]
  fn edit_distance_is_bounded() {
    assert_eq!(edit_distance("abc", "abc", 2), 0);
    assert_eq!(edit_distance("abc", "abd", 2), 1);
    assert_eq!(edit_distance("abc", "xyz", 2), 3); // capped at cap + 1
  }
}
