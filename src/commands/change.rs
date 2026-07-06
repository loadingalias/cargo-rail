//! `cargo rail change` - intent-file management.

use crate::commands::common::OutputFormat;
use crate::error::{RailError, RailResult};
use crate::release::change_files::{PendingChangeSet, parse_change_file, render_change_content, write_change_file};
use crate::release::version::BumpLevel;
use crate::workspace::WorkspaceContext;
use rustc_hash::FxHashSet;
use std::collections::BTreeMap;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Add a pending change file.
pub fn run_change_add(
  ctx: &WorkspaceContext,
  crate_names: Vec<String>,
  bump: String,
  message: Option<String>,
  name: Option<String>,
  format: OutputFormat,
) -> RailResult<()> {
  if format.is_json() {
    crate::output::set_json_mode(true);
  }
  if crate_names.is_empty() {
    return Err(RailError::with_help(
      "change add requires at least one crate",
      "usage: cargo rail change add <CRATE...> --bump patch --message \"user-facing change\"",
    ));
  }
  let bump = bump.parse::<BumpLevel>()?;

  let workspace_members = ctx.graph.workspace_members();
  for crate_name in &crate_names {
    if !workspace_members.contains(crate_name) {
      return Err(RailError::with_help(
        format!("crate '{}' not found", crate_name),
        format!("available: {}", workspace_members.join(", ")),
      ));
    }
  }

  let change_dir = configured_change_dir(ctx);
  let intents = base_intents(crate_names, bump);
  let file = if let Some(message) = message.map(|msg| msg.trim().to_string()).filter(|msg| !msg.is_empty()) {
    write_change_file(
      ctx.workspace_root(),
      change_dir,
      workspace_members,
      &intents,
      &message,
      name.as_deref(),
    )?
  } else if std::io::stdin().is_terminal() {
    let edited = edit_change_file(ctx.workspace_root(), change_dir, workspace_members, &intents)?;
    write_change_file(
      ctx.workspace_root(),
      change_dir,
      workspace_members,
      &edited.intents,
      &edited.body,
      name.as_deref(),
    )?
  } else {
    return Err(RailError::with_help(
      "change add requires --message in non-interactive mode",
      "pass --message, or run from a terminal with VISUAL or EDITOR set",
    ));
  };

  if format.is_json() {
    let payload = serde_json::json!({
      "action": "add",
      "path": &file.path,
      "crates": file.intents.iter().map(|intent| intent.crate_name.clone()).collect::<Vec<_>>(),
      "bump": file.intents.iter().map(|intent| intent.bump).max().unwrap_or(bump).as_str(),
    });
    let envelope = crate::output::machine_json_envelope("change", "add", "success", 0, payload);
    println!("{}", serde_json::to_string_pretty(&envelope)?);
  } else {
    // Bare path on stdout so shells can capture it directly.
    println!("{}", file.path.display());
  }
  Ok(())
}

/// Print pending change-file status.
pub fn run_change_status(ctx: &WorkspaceContext, format: OutputFormat) -> RailResult<()> {
  if format.is_json() {
    crate::output::set_json_mode(true);
  }

  let workspace_members = ctx.graph.workspace_members();
  let pending = PendingChangeSet::load(ctx.workspace_root(), configured_change_dir(ctx), workspace_members)?;
  let summaries = pending.crate_summaries();

  if format.is_json() {
    let files: Vec<_> = pending
      .files
      .iter()
      .map(|file| {
        serde_json::json!({
          "path": file.path,
          "intents": file.intents.iter().map(|intent| {
            serde_json::json!({ "crate": intent.crate_name, "bump": intent.bump })
          }).collect::<Vec<_>>(),
          "body": file.body,
        })
      })
      .collect();
    let payload = serde_json::json!({
      "action": "status",
      "files": files,
      "crates": summaries,
      "count": pending.files.len(),
    });
    let envelope = crate::output::machine_json_envelope("change", "status", "success", 0, payload);
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    return Ok(());
  }

  if pending.is_empty() {
    println!("no pending change files");
    return Ok(());
  }

  println!("resulting bumps:");
  for summary in &summaries {
    let suffix = if summary.file_count == 1 {
      "1 file".to_string()
    } else {
      format!("{} files", summary.file_count)
    };
    println!("  {}: {} ({})", summary.crate_name, summary.bump, suffix);
  }
  println!();
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

struct EditedChange {
  intents: BTreeMap<String, BumpLevel>,
  body: String,
}

fn base_intents(crate_names: Vec<String>, bump: BumpLevel) -> BTreeMap<String, BumpLevel> {
  crate_names.into_iter().map(|crate_name| (crate_name, bump)).collect()
}

fn configured_change_dir(ctx: &WorkspaceContext) -> &str {
  ctx
    .config
    .as_ref()
    .map(|config| config.release.change_dir.as_str())
    .unwrap_or(crate::release::change_files::DEFAULT_CHANGE_DIR)
}

fn edit_change_file(
  workspace_root: &Path,
  change_dir: &str,
  workspace_members: &[String],
  intents: &BTreeMap<String, BumpLevel>,
) -> RailResult<EditedChange> {
  let editor = std::env::var("VISUAL")
    .ok()
    .filter(|value| !value.trim().is_empty())
    .or_else(|| std::env::var("EDITOR").ok().filter(|value| !value.trim().is_empty()))
    .ok_or_else(|| {
      RailError::with_help(
        "change add requires VISUAL or EDITOR when --message is omitted",
        "set EDITOR=vim, or pass --message for non-interactive use",
      )
    })?;
  edit_change_file_with_editor(workspace_root, change_dir, workspace_members, intents, &editor)
}

fn edit_change_file_with_editor(
  workspace_root: &Path,
  change_dir: &str,
  workspace_members: &[String],
  intents: &BTreeMap<String, BumpLevel>,
  editor: &str,
) -> RailResult<EditedChange> {
  let dir = workspace_root.join(change_dir);
  fs::create_dir_all(&dir).map_err(|e| RailError::message(format!("failed to create {}: {}", dir.display(), e)))?;
  let draft_path = unique_draft_path(&dir);
  let template = editor_template(intents);
  fs::write(&draft_path, template)
    .map_err(|e| RailError::message(format!("failed to write {}: {}", draft_path.display(), e)))?;

  let edit_result = run_editor(editor, &draft_path);
  if let Err(err) = edit_result {
    let _ = fs::remove_file(&draft_path);
    return Err(err);
  }

  let edited = fs::read_to_string(&draft_path)
    .map_err(|e| RailError::message(format!("failed to read {}: {}", draft_path.display(), e)))?;
  let _ = fs::remove_file(&draft_path);

  let normalized = strip_editor_comments(&edited);
  let member_set: FxHashSet<&str> = workspace_members.iter().map(String::as_str).collect();
  let parsed = parse_change_file(&PathBuf::from("<editor>"), &normalized, &member_set)?;
  let intents = parsed
    .intents
    .into_iter()
    .map(|intent| (intent.crate_name, intent.bump))
    .collect();
  Ok(EditedChange {
    intents,
    body: parsed.body,
  })
}

fn editor_template(intents: &BTreeMap<String, BumpLevel>) -> String {
  let body = "# Write the user-facing changelog entry below.\n# Lines starting with # are ignored.\n";
  render_change_content(intents, body)
}

fn strip_editor_comments(content: &str) -> String {
  let Some(rest) = content.strip_prefix("---\n") else {
    return content.to_string();
  };
  let Some((frontmatter, body)) = rest.split_once("\n---\n") else {
    return content.to_string();
  };

  let body = body
    .lines()
    .filter(|line| !line.trim_start().starts_with('#'))
    .collect::<Vec<_>>()
    .join("\n");
  format!("---\n{}\n---\n\n{}\n", frontmatter.trim_end(), body.trim())
}

fn unique_draft_path(dir: &Path) -> PathBuf {
  let mut path = dir.join(format!(".change-edit-{}.md", std::process::id()));
  let mut n = 2;
  while path.exists() {
    path = dir.join(format!(".change-edit-{}-{}.md", std::process::id(), n));
    n += 1;
  }
  path
}

fn run_editor(editor: &str, path: &Path) -> RailResult<()> {
  #[cfg(unix)]
  let status = Command::new("sh")
    .arg("-c")
    .arg("$RAIL_EDITOR \"$1\"")
    .arg("cargo-rail-editor")
    .arg(path)
    .env("RAIL_EDITOR", editor)
    .status();

  #[cfg(not(unix))]
  let status = {
    let mut parts = editor.split_whitespace();
    let program = parts
      .next()
      .ok_or_else(|| RailError::message("EDITOR cannot be empty"))?;
    Command::new(program).args(parts).arg(path).status()
  };

  let status = status.map_err(|e| RailError::message(format!("failed to launch editor '{}': {}", editor, e)))?;
  if status.success() {
    Ok(())
  } else {
    Err(RailError::message(format!(
      "editor '{}' exited with {}",
      editor, status
    )))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn editor_comment_lines_are_removed() {
    let content = "---\n\"rail-core\" = \"minor\"\n---\n\n# comment\nEdited body.\n";
    assert_eq!(
      strip_editor_comments(content),
      "---\n\"rail-core\" = \"minor\"\n---\n\nEdited body.\n"
    );
  }

  #[cfg(unix)]
  #[test]
  fn editor_path_parses_edited_body() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::TempDir::new().unwrap();
    let editor = root.path().join("editor.sh");
    fs::write(
      &editor,
      r#"#!/bin/sh
printf '\nEdited body from editor.\n' >> "$1"
"#,
    )
    .unwrap();
    let mut perms = fs::metadata(&editor).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&editor, perms).unwrap();

    let mut intents = BTreeMap::new();
    intents.insert("rail-core".to_string(), BumpLevel::Minor);
    let members = vec!["rail-core".to_string()];
    let edited = edit_change_file_with_editor(
      root.path(),
      crate::release::change_files::DEFAULT_CHANGE_DIR,
      &members,
      &intents,
      editor.to_str().unwrap(),
    )
    .unwrap();

    assert_eq!(edited.body, "Edited body from editor.");
    assert_eq!(edited.intents.get("rail-core"), Some(&BumpLevel::Minor));
  }
}
