//! Git-notes integrity checks

use super::trait_def::{Check, CheckContext, CheckResult};
use crate::core::config::RailConfig;
use anyhow::Result;
use std::process::Command;

/// Check that validates git-notes mappings
pub struct GitNotesCheck;

impl Check for GitNotesCheck {
  fn name(&self) -> &str {
    "git-notes"
  }

  fn description(&self) -> &str {
    "Validates git-notes mappings integrity"
  }

  fn run(&self, ctx: &CheckContext) -> Result<CheckResult> {
    // Load config to get crate information
    let config = match RailConfig::load(&ctx.workspace_root) {
      Ok(c) => c,
      Err(_) => {
        // If config doesn't exist, we can't check git-notes
        return Ok(CheckResult::pass(
          self.name(),
          "No rail.toml found, skipping git-notes check",
        ));
      }
    };

    let mut issues = Vec::new();
    let mut total_notes = 0;

    // Check git-notes for each configured crate
    let crates_to_check = if let Some(ref crate_name) = ctx.crate_name {
      config
        .splits
        .iter()
        .filter(|s| &s.name == crate_name)
        .collect::<Vec<_>>()
    } else {
      config.splits.iter().collect::<Vec<_>>()
    };

    for split_config in crates_to_check {
      let notes_ref = format!("refs/notes/rail/{}", split_config.name);

      // Check if notes ref exists
      let output = Command::new("git")
        .arg("show-ref")
        .arg(&notes_ref)
        .current_dir(&ctx.workspace_root)
        .output()?;

      if !output.status.success() {
        issues.push(format!(
          "No git-notes found for '{}' (ref: {})",
          split_config.name, notes_ref
        ));
        continue;
      }

      // Count notes
      let count_output = Command::new("git")
        .arg("notes")
        .arg("--ref")
        .arg(&notes_ref)
        .arg("list")
        .current_dir(&ctx.workspace_root)
        .output()?;

      if count_output.status.success() {
        let count = String::from_utf8_lossy(&count_output.stdout).lines().count();
        total_notes += count;

        // In thorough mode, validate that all noted commits still exist
        if ctx.thorough && count > 0 {
          let notes_list = String::from_utf8_lossy(&count_output.stdout);
          for line in notes_list.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
              let commit_sha = parts[1];

              // Check if commit exists
              let commit_check = Command::new("git")
                .arg("cat-file")
                .arg("-e")
                .arg(commit_sha)
                .current_dir(&ctx.workspace_root)
                .output()?;

              if !commit_check.status.success() {
                issues.push(format!(
                  "Orphaned git-note for '{}': commit {} no longer exists",
                  split_config.name, commit_sha
                ));
              }
            }
          }
        }
      }
    }

    if !issues.is_empty() {
      Ok(CheckResult::warning(
        self.name(),
        format!("Git-notes issues found:\n{}", issues.join("\n")),
        Some("This may indicate incomplete sync operations"),
      ))
    } else if total_notes == 0 {
      Ok(CheckResult::pass(
        self.name(),
        "No git-notes found (expected if no splits/syncs have been performed)",
      ))
    } else {
      Ok(CheckResult::pass(
        self.name(),
        format!("Git-notes mappings valid ({} total notes)", total_notes),
      ))
    }
  }

  fn is_expensive(&self) -> bool {
    true // Checking all notes can be slow for large repos
  }

  fn requires_crate(&self) -> bool {
    false // Can check all crates or specific crate
  }
}
