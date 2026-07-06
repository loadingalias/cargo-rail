//! `cargo rail change` - intent-file management.

use crate::error::{RailError, RailResult};
use crate::release::change_files::{CHANGE_DIR, PendingChangeSet};
use crate::release::version::BumpLevel;
use crate::workspace::WorkspaceContext;
use chrono::Local;
use std::fs;
use std::path::PathBuf;

/// Add a pending change file.
pub fn run_change_add(
  ctx: &WorkspaceContext,
  crate_names: Vec<String>,
  bump: String,
  message: Option<String>,
) -> RailResult<()> {
  if crate_names.is_empty() {
    return Err(RailError::with_help(
      "change add requires at least one crate",
      "usage: cargo rail change add <CRATE...> --bump patch --message \"user-facing change\"",
    ));
  }
  let bump = bump.parse::<BumpLevel>()?;
  let message = message
    .map(|msg| msg.trim().to_string())
    .filter(|msg| !msg.is_empty())
    .ok_or_else(|| {
      RailError::with_help(
        "change add requires --message",
        "write the user-facing changelog entry with --message",
      )
    })?;

  let workspace_members = ctx.graph.workspace_members();
  for crate_name in &crate_names {
    if !workspace_members.contains(crate_name) {
      return Err(RailError::with_help(
        format!("crate '{}' not found", crate_name),
        format!("available: {}", workspace_members.join(", ")),
      ));
    }
  }

  let dir = ctx.workspace_root().join(CHANGE_DIR);
  fs::create_dir_all(&dir).map_err(|e| RailError::message(format!("failed to create {}: {}", dir.display(), e)))?;
  let path = unique_change_path(&dir, &message);
  let mut content = String::from("---\n");
  let mut sorted = crate_names;
  sorted.sort();
  sorted.dedup();
  for crate_name in &sorted {
    content.push_str(&format!("\"{}\" = \"{}\"\n", crate_name, bump.as_str()));
  }
  content.push_str("---\n\n");
  content.push_str(&message);
  content.push('\n');

  fs::write(&path, content).map_err(|e| RailError::message(format!("failed to write {}: {}", path.display(), e)))?;
  println!("{}", path.display());
  Ok(())
}

/// Print pending change-file status.
pub fn run_change_status(ctx: &WorkspaceContext) -> RailResult<()> {
  let workspace_members = ctx.graph.workspace_members();
  let pending = PendingChangeSet::load(ctx.workspace_root(), workspace_members)?;
  if pending.is_empty() {
    println!("no pending change files");
    return Ok(());
  }

  println!("pending change files:");
  for file in &pending.files {
    println!("  {}", file.path.display());
    for intent in &file.intents {
      println!("    {}: {}", intent.crate_name, intent.bump);
    }
    if let Some(first_line) = file.body.lines().find(|line| !line.trim().is_empty()) {
      println!("    {}", first_line.trim());
    }
  }
  Ok(())
}

fn unique_change_path(dir: &std::path::Path, message: &str) -> PathBuf {
  let timestamp = Local::now().format("%Y%m%d%H%M%S");
  let slug = slugify(message);
  let mut path = dir.join(format!("{}-{}.md", timestamp, slug));
  let mut n = 2;
  while path.exists() {
    path = dir.join(format!("{}-{}-{}.md", timestamp, slug, n));
    n += 1;
  }
  path
}

fn slugify(value: &str) -> String {
  let mut out = String::new();
  let mut last_dash = false;
  for c in value.chars() {
    if c.is_ascii_alphanumeric() {
      out.push(c.to_ascii_lowercase());
      last_dash = false;
    } else if !last_dash && !out.is_empty() {
      out.push('-');
      last_dash = true;
    }
    if out.len() >= 48 {
      break;
    }
  }
  while out.ends_with('-') {
    out.pop();
  }
  if out.is_empty() { "change".to_string() } else { out }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn slugify_is_stable_and_ascii() {
    assert_eq!(slugify("Added --bump auto!"), "added-bump-auto");
    assert_eq!(slugify("!!!"), "change");
  }
}
