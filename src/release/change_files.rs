//! Intent-based release change files.
//!
//! Pending change files live in `.changes/*.md` by default and use TOML
//! frontmatter to map crate names to bump levels.

use crate::error::{RailError, RailResult};
use crate::release::version::BumpLevel;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Default directory containing pending change files.
pub const DEFAULT_CHANGE_DIR: &str = ".changes";

/// Deprecated pre-v0.15 change-file directory. This is a migration guard only.
pub const LEGACY_CHANGE_DIR: &str = ".rail/changes";

const LEGACY_CHANGE_DIR_HINT: &str = "move files to .changes/ (git mv .rail/changes .changes)";

/// Reviewed release intent for one crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeBump {
  /// The code change is intentionally not release-worthy.
  None,
  /// Release a patch version.
  Patch,
  /// Release a minor version.
  Minor,
  /// Release a major version.
  Major,
}

impl ChangeBump {
  /// Convert release-worthy intent into its semantic bump level.
  pub const fn release_level(self) -> Option<BumpLevel> {
    match self {
      Self::None => None,
      Self::Patch => Some(BumpLevel::Patch),
      Self::Minor => Some(BumpLevel::Minor),
      Self::Major => Some(BumpLevel::Major),
    }
  }

  /// Canonical spelling used by change files and command output.
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::None => "none",
      Self::Patch => "patch",
      Self::Minor => "minor",
      Self::Major => "major",
    }
  }
}

impl std::str::FromStr for ChangeBump {
  type Err = RailError;

  fn from_str(value: &str) -> Result<Self, Self::Err> {
    match value.to_ascii_lowercase().as_str() {
      "none" | "no-release" => Ok(Self::None),
      "patch" => Ok(Self::Patch),
      "minor" => Ok(Self::Minor),
      "major" => Ok(Self::Major),
      _ => Err(RailError::with_help(
        format!("invalid change intent: {}", value),
        "use 'none', 'patch', 'minor', or 'major'",
      )),
    }
  }
}

impl std::fmt::Display for ChangeBump {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    formatter.write_str(self.as_str())
  }
}

/// One crate intent from a change file.
#[derive(Debug, Clone, Serialize)]
pub struct ChangeIntent {
  /// Source change file path.
  pub path: PathBuf,
  /// Crate covered by this intent.
  pub crate_name: String,
  /// Requested bump level.
  pub bump: ChangeBump,
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

/// Effective pending bump for one crate.
#[derive(Debug, Clone, Serialize)]
pub struct CrateChangeSummary {
  /// Crate covered by one or more pending files.
  pub crate_name: String,
  /// Maximum pending bump for the crate.
  pub bump: ChangeBump,
  /// Number of pending files that mention this crate.
  pub file_count: usize,
}

/// All pending change files indexed by crate.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PendingChangeSet {
  /// Parsed files in deterministic path order.
  pub files: Vec<ChangeFile>,
  by_crate: FxHashMap<String, Vec<ChangeIntent>>,
}

/// Exact change-file mutations needed to consume one release plan.
pub(crate) struct ChangeFileConsumption {
  /// Files whose intents are all consumed by the release.
  pub delete: Vec<PathBuf>,
  /// Files rewritten to retain no-release intents for unreleased crates.
  pub update: Vec<(PathBuf, String)>,
}

impl PendingChangeSet {
  /// Load pending change files from the configured directory.
  pub fn load(workspace_root: &Path, change_dir: &str, workspace_members: &[String]) -> RailResult<Self> {
    assert_no_legacy_change_files(workspace_root)?;
    validate_change_dir_value(change_dir)?;
    let dir = workspace_root.join(change_dir);
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

  /// Effective bump per crate, folding multiple files by maximum bump level.
  pub fn crate_summaries(&self) -> Vec<CrateChangeSummary> {
    let mut summaries: Vec<_> = self
      .by_crate
      .iter()
      .filter_map(|(crate_name, intents)| {
        intents
          .iter()
          .map(|intent| intent.bump)
          .max()
          .map(|bump| CrateChangeSummary {
            crate_name: crate_name.clone(),
            bump,
            file_count: intents.len(),
          })
      })
      .collect();
    summaries.sort_by(|a, b| a.crate_name.cmp(&b.crate_name));
    summaries
  }

  /// Plan lossless consumption for change files touched by a release.
  ///
  /// Release-worthy intents are atomic: a shared file cannot be consumed
  /// unless every release-worthy crate it names is in the plan. No-release
  /// intents for other crates are retained by rewriting the frontmatter.
  pub(crate) fn plan_consumption(
    &self,
    consumed: &[PathBuf],
    planned: &std::collections::HashSet<&str>,
  ) -> RailResult<ChangeFileConsumption> {
    let mut partial: Vec<String> = Vec::new();
    let mut delete = Vec::new();
    let mut update = Vec::new();
    for file in &self.files {
      if !consumed.contains(&file.path) {
        continue;
      }
      let mut missing: Vec<&str> = file
        .intents
        .iter()
        .filter(|intent| intent.bump.release_level().is_some())
        .map(|intent| intent.crate_name.as_str())
        .filter(|name| !planned.contains(name))
        .collect();
      if !missing.is_empty() {
        missing.sort_unstable();
        partial.push(format!("{} (missing: {})", file.path.display(), missing.join(", ")));
        continue;
      }

      let retained: BTreeMap<_, _> = file
        .intents
        .iter()
        .filter(|intent| !planned.contains(intent.crate_name.as_str()))
        .map(|intent| (intent.crate_name.clone(), intent.bump))
        .collect();
      if retained.is_empty() {
        delete.push(file.path.clone());
      } else {
        update.push((file.path.clone(), render_change_content(&retained, &file.body)));
      }
    }

    if partial.is_empty() {
      return Ok(ChangeFileConsumption { delete, update });
    }

    Err(RailError::with_help(
      format!(
        "release would consume change file(s) naming crates outside this plan:\n  {}",
        partial.join("\n  ")
      ),
      "release every release-worthy crate the change file names together, or split it into one file per release group",
    ))
  }
}

/// Fail if the removed `.rail/changes` directory still contains Markdown files.
pub fn assert_no_legacy_change_files(workspace_root: &Path) -> RailResult<()> {
  let legacy_dir = workspace_root.join(LEGACY_CHANGE_DIR);
  if !directory_contains_markdown(&legacy_dir)? {
    return Ok(());
  }

  Err(RailError::with_help(
    format!("legacy change files found in {}", legacy_dir.display()),
    LEGACY_CHANGE_DIR_HINT,
  ))
}

/// Write a new pending change file using deterministic `{slug}-{hash4}.md` naming.
pub fn write_change_file(
  workspace_root: &Path,
  change_dir: &str,
  workspace_members: &[String],
  intents: &BTreeMap<String, ChangeBump>,
  body: &str,
  name_override: Option<&str>,
) -> RailResult<ChangeFile> {
  assert_no_legacy_change_files(workspace_root)?;
  validate_change_dir_value(change_dir)?;

  let member_set: FxHashSet<&str> = workspace_members.iter().map(String::as_str).collect();
  validate_intents(intents, &member_set)?;

  let body = body.trim();
  if body.is_empty() {
    return Err(RailError::message("change file body cannot be empty"));
  }

  let content = render_change_content(intents, body);
  let slug = match name_override {
    Some(name) => slugify_name(name)?,
    None => slugify_message(body),
  };
  let hash = hash4(&content);
  let dir = workspace_root.join(change_dir);
  fs::create_dir_all(&dir).map_err(|e| RailError::message(format!("failed to create {}: {}", dir.display(), e)))?;
  let path = dir.join(format!("{}-{}.md", slug, hash));

  if path.exists() {
    let existing =
      fs::read_to_string(&path).map_err(|e| RailError::message(format!("failed to read {}: {}", path.display(), e)))?;
    if existing == content {
      return Err(RailError::message(format!(
        "change file already exists: {}",
        path.display()
      )));
    }
    return Err(RailError::with_help(
      format!("change file name collision: {}", path.display()),
      "choose a different --name so cargo-rail does not overwrite unrelated release intent",
    ));
  }

  fs::write(&path, &content).map_err(|e| RailError::message(format!("failed to write {}: {}", path.display(), e)))?;
  parse_change_file(&path, &content, &member_set)
}

/// Render canonical change-file content from validated intents and body.
pub fn render_change_content(intents: &BTreeMap<String, ChangeBump>, body: &str) -> String {
  let mut content = String::from("---\n");
  for (crate_name, bump) in intents {
    content.push_str(&format!("\"{}\" = \"{}\"\n", crate_name, bump.as_str()));
  }
  content.push_str("---\n\n");
  content.push_str(body.trim());
  content.push('\n');
  content
}

pub(crate) fn parse_change_file(
  path: &Path,
  content: &str,
  workspace_members: &FxHashSet<&str>,
) -> RailResult<ChangeFile> {
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

fn validate_intents(intents: &BTreeMap<String, ChangeBump>, workspace_members: &FxHashSet<&str>) -> RailResult<()> {
  if intents.is_empty() {
    return Err(RailError::with_help(
      "change add requires at least one crate",
      "usage: cargo rail change add <CRATE...> --bump <none|patch|minor|major> --message \"reviewed intent\"",
    ));
  }

  let mut unknown = Vec::new();
  for crate_name in intents.keys() {
    if !workspace_members.contains(crate_name.as_str()) {
      unknown.push(crate_name.as_str());
    }
  }
  if unknown.is_empty() {
    return Ok(());
  }

  unknown.sort_unstable();
  let mut available: Vec<_> = workspace_members.iter().copied().collect();
  available.sort_unstable();
  Err(RailError::with_help(
    format!("unknown crate(s): {}", unknown.join(", ")),
    format!("available: {}", available.join(", ")),
  ))
}

fn directory_contains_markdown(dir: &Path) -> RailResult<bool> {
  if !dir.exists() {
    return Ok(false);
  }
  for entry in fs::read_dir(dir).map_err(|e| RailError::message(format!("failed to read {}: {}", dir.display(), e)))? {
    let entry = entry.map_err(|e| RailError::message(format!("failed to read {}: {}", dir.display(), e)))?;
    let path = entry.path();
    if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
      return Ok(true);
    }
  }
  Ok(false)
}

pub(crate) fn change_dir_validation_error(change_dir: &str) -> Option<&'static str> {
  let path = Path::new(change_dir);
  if change_dir.trim().is_empty() {
    return Some("change_dir cannot be empty");
  }
  if path.is_absolute()
    || path.components().any(|component| {
      matches!(
        component,
        Component::ParentDir | Component::RootDir | Component::Prefix(_)
      )
    })
  {
    return Some("change_dir must be a workspace-relative path");
  }
  if is_legacy_change_dir(change_dir) {
    return Some("change_dir = \".rail/changes\" is removed; use \".changes\" or \"changes\"");
  }
  None
}

fn validate_change_dir_value(change_dir: &str) -> RailResult<()> {
  let Some(reason) = change_dir_validation_error(change_dir) else {
    return Ok(());
  };

  if is_legacy_change_dir(change_dir) {
    return Err(RailError::with_help(
      "change_dir = \".rail/changes\" is removed",
      LEGACY_CHANGE_DIR_HINT,
    ));
  }

  Err(RailError::message(format!("invalid release.change_dir: {}", reason)))
}

fn is_legacy_change_dir(change_dir: &str) -> bool {
  change_dir.trim_end_matches(['/', '\\']) == LEGACY_CHANGE_DIR
}

fn slugify_message(value: &str) -> String {
  let words = ascii_words(value).take(4);
  truncate_slug(words.collect::<Vec<_>>().join("-"))
}

fn slugify_name(value: &str) -> RailResult<String> {
  let slug = truncate_slug(ascii_words(value).collect::<Vec<_>>().join("-"));
  if slug == "change" && value.chars().all(|c| !c.is_ascii_alphanumeric()) {
    return Err(RailError::with_help(
      "change file --name must contain at least one ASCII letter or digit",
      "use a short kebab-case name such as --name fix-wasm-parser",
    ));
  }
  Ok(slug)
}

fn ascii_words(value: &str) -> impl Iterator<Item = String> + '_ {
  value
    .split(|c: char| !c.is_ascii_alphanumeric())
    .filter(|part| !part.is_empty())
    .map(|part| part.to_ascii_lowercase())
}

fn truncate_slug(mut slug: String) -> String {
  if slug.is_empty() {
    return "change".to_string();
  }
  if slug.len() > 32 {
    slug.truncate(32);
  }
  while slug.ends_with('-') {
    slug.pop();
  }
  if slug.is_empty() { "change".to_string() } else { slug }
}

fn hash4(content: &str) -> String {
  let hash = crate::utils::fnv1a64(content.as_bytes());
  format!("{:04x}", hash & 0xffff)
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
    let path = PathBuf::from(".changes/add-auto.md");
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
    assert_eq!(file.intents[0].bump, ChangeBump::Patch);
    assert_eq!(file.intents[1].bump, ChangeBump::Minor);
    assert_eq!(file.body, "Added auto bump planning.");
  }

  #[test]
  fn parses_explicit_no_release_intent() {
    let members = members();
    let member_set: FxHashSet<&str> = members.iter().map(String::as_str).collect();
    let file = parse_change_file(
      Path::new(".changes/no-release.md"),
      "---\n\"rail-core\" = \"none\"\n---\n\nInternal refactor with no shipped behavior change.\n",
      &member_set,
    )
    .unwrap();

    assert_eq!(file.intents[0].bump, ChangeBump::None);
    assert_eq!(file.intents[0].bump.release_level(), None);
  }

  #[test]
  fn rejects_unknown_crate() {
    let path = PathBuf::from(".changes/bad.md");
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

  #[test]
  fn slug_uses_first_four_words_and_truncates() {
    assert_eq!(
      slugify_message("Added --bump auto for workspace releases"),
      "added-bump-auto-for"
    );
    assert_eq!(
      slugify_message("Supercalifragilisticexpialidocious parser cleanup"),
      "supercalifragilisticexpialidocio"
    );
    assert_eq!(slugify_message("!!!"), "change");
  }

  #[test]
  fn name_override_is_sanitized_and_truncated() {
    assert_eq!(
      slugify_name("Fixed ECDSA oracle compatibility with p256 0.14").unwrap(),
      "fixed-ecdsa-oracle-compatibility"
    );
    assert!(slugify_name("../").is_err());
  }

  #[test]
  fn content_hash_is_stable() {
    let mut intents = BTreeMap::new();
    intents.insert("rail-core".to_string(), ChangeBump::Minor);
    let content = render_change_content(&intents, "Added reviewed release intent.");
    assert_eq!(hash4(&content), hash4(&content));
    assert_ne!(hash4(&content), hash4(&content.replace("Added", "Changed")));
  }
}
