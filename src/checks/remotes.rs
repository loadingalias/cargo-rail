//! Remote repository accessibility checks

use super::trait_def::{Check, CheckContext, CheckResult};
use crate::core::config::RailConfig;
use crate::core::error::RailResult;
use crate::ui::progress::FileProgress;
use crate::utils;
use std::process::Command;

/// Check that validates remote repository accessibility
pub struct RemoteAccessCheck;

impl Check for RemoteAccessCheck {
  fn name(&self) -> &str {
    "remote-access"
  }

  fn description(&self) -> &str {
    "Validates remote repository accessibility"
  }

  fn run(&self, ctx: &CheckContext) -> RailResult<CheckResult> {
    // This is an expensive check, only run in thorough mode
    if !ctx.thorough {
      return Ok(CheckResult::pass(
        self.name(),
        "Skipped (use --thorough to test remote connectivity)",
      ));
    }

    // Load config to get remote URLs
    let config = match RailConfig::load(&ctx.workspace_root) {
      Ok(c) => c,
      Err(_) => {
        return Ok(CheckResult::pass(
          self.name(),
          "No rail.toml found, skipping remote access check",
        ));
      }
    };

    let mut issues = Vec::new();
    let mut checked = 0;

    // Check each configured remote
    let crates_to_check = if let Some(ref crate_name) = ctx.crate_name {
      config
        .splits
        .iter()
        .filter(|s| &s.name == crate_name)
        .collect::<Vec<_>>()
    } else {
      config.splits.iter().collect::<Vec<_>>()
    };

    // Show progress bar for remote access checks (network operations are slow)
    let mut progress = if !crates_to_check.is_empty() {
      Some(FileProgress::new(
        crates_to_check.len(),
        format!("Checking access to {} remotes", crates_to_check.len()),
      ))
    } else {
      None
    };

    for split_config in crates_to_check {
      checked += 1;

      // Validate remote URL format
      if !is_valid_remote_url(&split_config.remote) {
        issues.push(format!(
          "'{}': Invalid remote URL format: {}",
          split_config.name, split_config.remote
        ));
        if let Some(ref mut p) = progress {
          p.inc();
        }
        continue;
      }

      // Test connectivity
      match test_remote_access(&split_config.remote) {
        Ok(true) => {
          // Remote is accessible
        }
        Ok(false) => {
          issues.push(format!(
            "'{}': Cannot access remote: {}",
            split_config.name, split_config.remote
          ));
        }
        Err(err) => {
          issues.push(format!(
            "'{}': Error testing remote {}: {}",
            split_config.name, split_config.remote, err
          ));
        }
      }

      if let Some(ref mut p) = progress {
        p.inc();
      }
    }

    if !issues.is_empty() {
      Ok(CheckResult::error(
        self.name(),
        format!("Remote access issues:\n{}", issues.join("\n")),
        Some("Verify remote URLs are correct and you have network access"),
      ))
    } else {
      Ok(CheckResult::pass(
        self.name(),
        format!("All {} remote(s) accessible", checked),
      ))
    }
  }

  fn is_expensive(&self) -> bool {
    true // Network operations
  }

  fn requires_crate(&self) -> bool {
    false // Can check all crates or specific crate
  }
}

/// Check if a URL looks like a valid Git remote URL
fn is_valid_remote_url(url: &str) -> bool {
  // SSH format: git@github.com:user/repo.git
  if url.starts_with("git@") || url.starts_with("ssh://") {
    return true;
  }

  // HTTPS format: https://github.com/user/repo.git
  if url.starts_with("https://") || url.starts_with("http://") {
    return true;
  }

  // Local path (absolute or relative)
  if utils::is_local_path(url) {
    return true;
  }

  false
}

/// Test if we can access a remote repository
fn test_remote_access(url: &str) -> RailResult<bool> {
  // For local paths, just check if directory exists
  if utils::is_local_path(url) {
    let path = std::path::Path::new(url);
    return Ok(path.exists());
  }

  // For remote URLs, use git ls-remote
  let output = Command::new("git").arg("ls-remote").arg("--heads").arg(url).output()?;

  Ok(output.status.success())
}
