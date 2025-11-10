//! Workspace validity checks

use super::trait_def::{Check, CheckContext, CheckResult};
use crate::core::config::RailConfig;
use anyhow::Result;
use std::path::Path;

/// Check that validates workspace structure and configuration
pub struct WorkspaceValidityCheck;

impl Check for WorkspaceValidityCheck {
  fn name(&self) -> &str {
    "workspace-validity"
  }

  fn description(&self) -> &str {
    "Validates workspace structure and rail.toml configuration"
  }

  fn run(&self, ctx: &CheckContext) -> Result<CheckResult> {
    // Check if rail.toml exists
    if !RailConfig::exists(&ctx.workspace_root) {
      return Ok(CheckResult::error(
        self.name(),
        "No rail.toml found",
        Some("Run `cargo rail init` to create a configuration"),
      ));
    }

    // Try to load and validate config
    match RailConfig::load(&ctx.workspace_root) {
      Ok(config) => {
        // Check if workspace root exists
        if !config.workspace.root.exists() {
          return Ok(CheckResult::error(
            self.name(),
            format!("Workspace root does not exist: {}", config.workspace.root.display()),
            Some("Update the workspace.root path in rail.toml"),
          ));
        }

        // Check if Cargo.toml exists in workspace root
        let cargo_toml = config.workspace.root.join("Cargo.toml");
        if !cargo_toml.exists() {
          return Ok(CheckResult::error(
            self.name(),
            format!(
              "No Cargo.toml found in workspace root: {}",
              config.workspace.root.display()
            ),
            Some("Ensure workspace.root points to a valid Cargo workspace"),
          ));
        }

        // Validate each split configuration
        for split in &config.splits {
          if let Err(err) = split.validate() {
            return Ok(CheckResult::error(
              self.name(),
              format!("Invalid split configuration for '{}': {}", split.name, err),
              Some("Fix the configuration in rail.toml"),
            ));
          }

          // Check if crate paths exist
          for path in split.get_paths() {
            if !path.exists() {
              return Ok(CheckResult::error(
                self.name(),
                format!("Crate path does not exist: {} (for '{}')", path.display(), split.name),
                Some("Update the path in rail.toml or create the missing directory"),
              ));
            }
          }
        }

        Ok(CheckResult::pass(
          self.name(),
          format!(
            "Workspace configuration valid ({} crates configured)",
            config.splits.len()
          ),
        ))
      }
      Err(err) => Ok(CheckResult::error(
        self.name(),
        format!("Failed to load rail.toml: {}", err),
        Some("Check the syntax of your rail.toml file"),
      )),
    }
  }

  fn is_expensive(&self) -> bool {
    false
  }

  fn requires_crate(&self) -> bool {
    false
  }
}

/// Helper to check if a path looks like a valid Cargo workspace
#[allow(dead_code)]
fn is_cargo_workspace(path: &Path) -> bool {
  let cargo_toml = path.join("Cargo.toml");
  if !cargo_toml.exists() {
    return false;
  }

  // Check if Cargo.toml contains [workspace]
  if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
    content.contains("[workspace]")
  } else {
    false
  }
}
