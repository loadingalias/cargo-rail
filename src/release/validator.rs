//! Pre-release validation checks

use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;
use std::process::Command;

/// Pre-release validator
pub struct ReleaseValidator<'a> {
  ctx: &'a WorkspaceContext,
}

impl<'a> ReleaseValidator<'a> {
  pub fn new(ctx: &'a WorkspaceContext) -> Self {
    Self { ctx }
  }

  /// Validate release readiness for crate(s)
  pub fn validate(&self, crate_names: &[String], require_clean: bool) -> RailResult<()> {
    // 1. Check working directory is clean
    if require_clean {
      self.check_clean_working_directory()?;
    }

    // 2. Validate crates exist in workspace
    let workspace_members = self.ctx.graph.workspace_members();
    for crate_name in crate_names {
      if !workspace_members.contains(crate_name) {
        return Err(RailError::with_help(
          format!("Crate '{}' not found in workspace", crate_name),
          format!("Available crates: {}", workspace_members.join(", ")),
        ));
      }
    }

    // 3. Check for uncommitted changes in crate directories
    if require_clean {
      for crate_name in crate_names {
        self.check_crate_uncommitted_changes(crate_name)?;
      }
    }

    Ok(())
  }

  /// Check if working directory is clean (no uncommitted changes)
  fn check_clean_working_directory(&self) -> RailResult<()> {
    let output = Command::new("git")
      .current_dir(self.ctx.workspace_root())
      .args(["status", "--porcelain"])
      .output()
      .map_err(|e| RailError::message(format!("Failed to run git status: {}", e)))?;

    if !output.status.success() {
      return Err(RailError::message("git status failed"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
      return Err(RailError::with_help(
        "Working directory has uncommitted changes",
        "Commit or stash your changes before releasing. Use --no-require-clean to bypass.",
      ));
    }

    Ok(())
  }

  /// Check for uncommitted changes in specific crate directory
  fn check_crate_uncommitted_changes(&self, crate_name: &str) -> RailResult<()> {
    // Get crate manifest path
    let metadata = self.ctx.cargo.metadata();
    let package = metadata
      .get_package(crate_name)
      .ok_or_else(|| RailError::message(format!("Crate '{}' not found", crate_name)))?;

    let crate_dir = package
      .manifest_path
      .parent()
      .ok_or_else(|| RailError::message("Invalid manifest path"))?;

    // Check for changes in this directory
    let output = Command::new("git")
      .current_dir(self.ctx.workspace_root())
      .args(["status", "--porcelain", "--"])
      .arg(crate_dir)
      .output()
      .map_err(|e| RailError::message(format!("Failed to run git status: {}", e)))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
      return Err(RailError::with_help(
        format!("Crate '{}' has uncommitted changes", crate_name),
        "Commit changes before releasing",
      ));
    }

    Ok(())
  }

  /// Validate that crate can be published to crates.io
  pub fn validate_publishable(&self, crate_name: &str) -> RailResult<()> {
    let metadata = self.ctx.cargo.metadata();
    let package = metadata
      .get_package(crate_name)
      .ok_or_else(|| RailError::message(format!("Crate '{}' not found", crate_name)))?;

    // Check if publish = false
    if let Some(publish) = &package.publish
      && publish.is_empty()
    {
      return Err(RailError::with_help(
        format!("Crate '{}' has publish = false in Cargo.toml", crate_name),
        "Remove 'publish = false' or exclude this crate from the release",
      ));
    }

    Ok(())
  }
}
