//! Intent-based release change files.
//!
//! Pending change files live in `.rail/changes/*.md` and use TOML
//! frontmatter to map crate names to bump levels.

use crate::error::{RailError, RailResult};
use crate::release::version::BumpLevel;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

/// Directory containing pending change files.
pub const CHANGE_DIR: &str = ".rail/changes";

/// One crate intent from a change file.
#[derive(Debug, Clone, Serialize)]
pub struct ChangeIntent {
  /// Source change file path.
  pub path: PathBuf,
  /// Crate covered by this intent.
  pub crate_name: String,
  /// Requested bump level.
  pub bump: BumpLevel,
  /// User-facing changelog body.
  pub body: String,
}

/// One parsed change file.
#[derive(Debug, Clone, Serialize)]
pub struct ChangeFile {
  /// File path.
  pub path: PathBuf,
  /// Intent entries in deterministic crate-name order.
  pub intents: Vec<ChangeIntent>,
  /// User-facing body shared by the file's intents.
  pub body: String,
}

/// All pending change files indexed by crate.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PendingChangeSet {
  /// Parsed files in deterministic path order.
  pub files: Vec<ChangeFile>,
  by_crate: FxHashMap<String, Vec<ChangeIntent>>,
}

impl PendingChangeSet {
  /// Load pending change files from `.rail/changes`.
  pub fn load(workspace_root: &Path, workspace_members: &[String]) -> RailResult<Self> {
    let dir = workspace_root.join(CHANGE_DIR);
    if !dir.exists() {
      return Ok(Self::default());
    }

    let member_set: FxHashSet<&str> = workspace_members.iter().map(String::as_str).collect();
    let mut paths = Vec::new();
    for entry in
      fs::read_dir(&dir).map_err(|e| RailError::message(format!("failed to read {}: {}", dir.display(), e)))?
    {
      let entry = entry.map_err(|e| RailError::message(format!("failed to read {}: {}", dir.display(), e)))?;
      let path = entry.path();
      if path.extension().is_some_and(|ext| ext == "md") {
        paths.push(path);
      }
    }
    paths.sort();

    let mut files = Vec::with_capacity(paths.len());
    let mut by_crate: FxHashMap<String, Vec<ChangeIntent>> = FxHashMap::default();
    for path in paths {
      let content = fs::read_to_string(&path)
        .map_err(|e| RailError::message(format!("failed to read {}: {}", path.display(), e)))?;
      let file = parse_change_file(&path, &content, &member_set)?;
      for intent in &file.intents {
        by_crate
          .entry(intent.crate_name.clone())
          .or_default()
          .push(intent.clone());
      }
      files.push(file);
    }

    Ok(Self { files, by_crate })
  }

  /// Pending intents for one crate.
  pub fn for_crate(&self, crate_name: &str) -> &[ChangeIntent] {
    self.by_crate.get(crate_name).map(Vec::as_slice).unwrap_or(&[])
  }

  /// Whether one crate has at least one pending intent.
  pub fn covers(&self, crate_name: &str) -> bool {
    self.by_crate.contains_key(crate_name)
  }

  /// Whether there are no pending change files.
  pub fn is_empty(&self) -> bool {
    self.files.is_empty()
  }
}

fn parse_change_file(path: &Path, content: &str, workspace_members: &FxHashSet<&str>) -> RailResult<ChangeFile> {
  let Some(rest) = content.strip_prefix("---\n") else {
    return Err(RailError::with_help(
      format!("invalid change file {}: missing TOML frontmatter", path.display()),
      "start the file with --- followed by crate bump entries",
    ));
  };
  let Some((frontmatter, body)) = rest.split_once("\n---\n") else {
    return Err(RailError::with_help(
      format!("invalid change file {}: unterminated TOML frontmatter", path.display()),
      "close the frontmatter with --- on its own line",
    ));
  };

  let body = body.trim();
  if body.is_empty() {
    return Err(RailError::message(format!(
      "invalid change file {}: body cannot be empty",
      path.display()
    )));
  }

  let raw: std::collections::BTreeMap<String, String> = toml_edit::de::from_str(frontmatter)
    .map_err(|e| RailError::message(format!("invalid change file {} frontmatter: {}", path.display(), e)))?;
  if raw.is_empty() {
    return Err(RailError::message(format!(
      "invalid change file {}: frontmatter must name at least one crate",
      path.display()
    )));
  }

  let mut intents = Vec::with_capacity(raw.len());
  for (crate_name, bump) in raw {
    if !workspace_members.contains(crate_name.as_str()) {
      return Err(RailError::with_help(
        format!("invalid change file {}: unknown crate '{}'", path.display(), crate_name),
        "use a workspace crate name in the frontmatter",
      ));
    }
    intents.push(ChangeIntent {
      path: path.to_path_buf(),
      crate_name,
      bump: bump.parse()?,
      body: body.to_string(),
    });
  }
  intents.sort_by(|a, b| a.crate_name.cmp(&b.crate_name));

  Ok(ChangeFile {
    path: path.to_path_buf(),
    intents,
    body: body.to_string(),
  })
}

/// Render a change-file body as a changelog entry.
pub fn render_change_body(body: &str) -> String {
  let trimmed = body.trim();
  if trimmed.starts_with('-') {
    trimmed.to_string()
  } else {
    format!("- {}", trimmed.replace('\n', "\n  "))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn members() -> Vec<String> {
    vec!["rail-core".to_string(), "rail-cli".to_string()]
  }

  #[test]
  fn parses_change_file_frontmatter() {
    let path = PathBuf::from(".rail/changes/add-auto.md");
    let content = r#"---
"rail-core" = "minor"
"rail-cli" = "patch"
---

Added auto bump planning.
"#;
    let members = members();
    let member_set: FxHashSet<&str> = members.iter().map(String::as_str).collect();
    let file = parse_change_file(&path, content, &member_set).unwrap();
    assert_eq!(file.intents.len(), 2);
    assert_eq!(file.intents[0].crate_name, "rail-cli");
    assert_eq!(file.intents[0].bump, BumpLevel::Patch);
    assert_eq!(file.intents[1].bump, BumpLevel::Minor);
    assert_eq!(file.body, "Added auto bump planning.");
  }

  #[test]
  fn rejects_unknown_crate() {
    let path = PathBuf::from(".rail/changes/bad.md");
    let content = "---\nghost = \"patch\"\n---\n\nMessage\n";
    let members = members();
    let member_set: FxHashSet<&str> = members.iter().map(String::as_str).collect();
    let err = parse_change_file(&path, content, &member_set).unwrap_err();
    assert!(err.to_string().contains("unknown crate 'ghost'"));
  }

  #[test]
  fn renders_body_as_entry() {
    assert_eq!(render_change_body("Added a feature."), "- Added a feature.");
    assert_eq!(render_change_body("- Already listed."), "- Already listed.");
  }
}
