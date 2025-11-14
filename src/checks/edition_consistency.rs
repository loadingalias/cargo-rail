//! Edition consistency check
//!
//! Validates that all workspace crates use a consistent Rust edition.
//! Optionally enforces a specific edition from policy configuration.

use super::trait_def::{Check, CheckContext, CheckResult, Severity};
use crate::core::config::RailConfig;
use crate::core::error::RailResult;
use cargo_metadata::MetadataCommand;
use std::collections::HashMap;

/// Check for edition consistency across workspace crates
pub struct EditionConsistencyCheck;

impl Check for EditionConsistencyCheck {
  fn name(&self) -> &'static str {
    "edition-consistency"
  }

  fn description(&self) -> &'static str {
    "Validate that all workspace crates use consistent Rust edition"
  }

  fn run(&self, ctx: &CheckContext) -> RailResult<CheckResult> {
    // Load cargo metadata
    let metadata = match MetadataCommand::new().current_dir(&ctx.workspace_root).exec() {
      Ok(m) => m,
      Err(e) => {
        return Ok(CheckResult::error(
          self.name(),
          format!("Failed to load workspace metadata: {}", e),
          Some("Run `cargo metadata` to check workspace validity"),
        ));
      }
    };

    // Try to load policy config (optional)
    let policy_edition = RailConfig::load(&ctx.workspace_root)
      .ok()
      .and_then(|c| c.policy.edition);

    // Collect editions used by workspace crates
    let mut edition_usage: HashMap<String, Vec<String>> = HashMap::new();

    for package in metadata.workspace_packages() {
      let edition = package.edition.to_string();
      edition_usage
        .entry(edition.clone())
        .or_default()
        .push(package.name.to_string());
    }

    // Check policy enforcement first
    if let Some(required_edition) = &policy_edition {
      let non_compliant: Vec<_> = edition_usage
        .iter()
        .filter(|(edition, _)| *edition != required_edition)
        .flat_map(|(_, crates)| crates)
        .cloned()
        .collect();

      if !non_compliant.is_empty() {
        return Ok(CheckResult {
          check_name: self.name().to_string(),
          passed: false,
          severity: Severity::Error,
          message: format!(
            "Policy requires edition '{}', but {} crate(s) use different editions",
            required_edition,
            non_compliant.len()
          ),
          suggestion: Some(format!(
            "Update Cargo.toml in non-compliant crates to use 'edition = \"{}\"'",
            required_edition
          )),
          details: Some(serde_json::json!({
            "required_edition": required_edition,
            "non_compliant_crates": non_compliant,
            "edition_usage": edition_usage,
          })),
        });
      }
    }

    // Check for consistency (no policy, just uniformity)
    if edition_usage.len() > 1 {
      let editions: Vec<_> = edition_usage.keys().cloned().collect();
      let most_common_edition = edition_usage
        .iter()
        .max_by_key(|(_, crates)| crates.len())
        .map(|(edition, _)| edition.clone())
        .unwrap_or_default();

      Ok(CheckResult {
        check_name: self.name().to_string(),
        passed: false,
        severity: Severity::Warning,
        message: format!(
          "Found {} different Rust editions in workspace: {}",
          edition_usage.len(),
          editions.join(", ")
        ),
        suggestion: Some(format!(
          "Consider standardizing on edition '{}' (used by most crates). Configure in rail.toml: [policy] edition = \"{}\"",
          most_common_edition, most_common_edition
        )),
        details: Some(serde_json::json!({
          "edition_usage": edition_usage,
          "most_common": most_common_edition,
        })),
      })
    } else {
      let edition = edition_usage.keys().next().map(|s| s.as_str()).unwrap_or("unknown");
      Ok(CheckResult::pass(
        self.name(),
        format!("All workspace crates use edition '{}'", edition),
      ))
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;

  #[test]
  fn test_check_name() {
    let check = EditionConsistencyCheck;
    assert_eq!(check.name(), "edition-consistency");
  }

  #[test]
  fn test_check_on_cargo_rail() {
    // Test on cargo-rail itself (should be consistent)
    let ctx = CheckContext {
      workspace_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
      crate_name: None,
      thorough: false,
    };

    let check = EditionConsistencyCheck;
    let result = check.run(&ctx).unwrap();

    // cargo-rail should have consistent edition
    assert!(result.passed, "cargo-rail workspace should have consistent edition");
  }
}
