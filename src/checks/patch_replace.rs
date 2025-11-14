//! Patch/Replace section check
//!
//! Detects `[patch]` and `[replace]` sections in workspace member Cargo.toml files.
//! These sections can cause issues when publishing crates, as they're ignored by crates.io.
//!
//! This is primarily a publishing footgun detector - if you're using `[patch]` or `[replace]`
//! in workspace members (not just the root), those patches won't be active when someone
//! depends on your published crate.

use super::trait_def::{Check, CheckContext, CheckResult, Severity};
use crate::core::config::RailConfig;
use crate::core::error::RailResult;
use cargo_metadata::MetadataCommand;
use std::collections::HashMap;
use toml_edit::DocumentMut;

/// Check for `[patch]` or `[replace]` sections in workspace crates
pub struct PatchReplaceCheck;

impl Check for PatchReplaceCheck {
  fn name(&self) -> &'static str {
    "patch-replace-usage"
  }

  fn description(&self) -> &'static str {
    "Detect [patch]/[replace] usage in workspace crates (publishing footgun)"
  }

  fn run(&self, ctx: &CheckContext) -> RailResult<CheckResult> {
    // Check if policy forbids patch/replace
    let policy_forbids = RailConfig::load(&ctx.workspace_root)
      .ok()
      .map(|c| c.policy.forbid_patch_replace)
      .unwrap_or(false);

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

    let workspace_root = &metadata.workspace_root;
    let mut issues: HashMap<String, PatchReplaceIssue> = HashMap::new();

    // Check each workspace package (excluding workspace root)
    for package in metadata.workspace_packages() {
      let manifest_path = package.manifest_path.as_std_path();

      // Skip if this is the workspace root Cargo.toml
      if manifest_path == workspace_root.join("Cargo.toml").as_std_path() {
        continue;
      }

      // Read and parse the manifest
      let content = match std::fs::read_to_string(manifest_path) {
        Ok(c) => c,
        Err(_) => continue, // Skip if can't read
      };

      let doc = match content.parse::<DocumentMut>() {
        Ok(d) => d,
        Err(_) => continue, // Skip if can't parse
      };

      // Check for [patch] section
      let has_patch = doc.get("patch").and_then(|p| p.as_table()).is_some();

      // Check for [replace] section
      let has_replace = doc.get("replace").and_then(|r| r.as_table()).is_some();

      if has_patch || has_replace {
        let sections = match (has_patch, has_replace) {
          (true, true) => vec!["patch".to_string(), "replace".to_string()],
          (true, false) => vec!["patch".to_string()],
          (false, true) => vec!["replace".to_string()],
          (false, false) => vec![],
        };

        issues.insert(
          package.name.to_string(),
          PatchReplaceIssue {
            crate_name: package.name.to_string(),
            manifest_path: manifest_path.display().to_string(),
            sections,
            is_publishable: package.publish.is_none()
              || !package.publish.as_ref().map(|p| p.is_empty()).unwrap_or(false),
          },
        );
      }
    }

    if issues.is_empty() {
      Ok(CheckResult::pass(
        self.name(),
        "No [patch] or [replace] sections found in workspace crates",
      ))
    } else {
      let severity = if policy_forbids {
        Severity::Error
      } else {
        Severity::Warning
      };

      let publishable_count = issues.values().filter(|i| i.is_publishable).count();

      let message = if publishable_count > 0 {
        format!(
          "Found [patch]/[replace] in {} crate(s) ({} publishable)",
          issues.len(),
          publishable_count
        )
      } else {
        format!(
          "Found [patch]/[replace] in {} crate(s) (none publishable)",
          issues.len()
        )
      };

      let suggestion = if policy_forbids {
        Some("Policy forbids [patch]/[replace]. Remove these sections or set publish = false".to_string())
      } else {
        Some(
          "Consider moving [patch]/[replace] to workspace root. These sections are ignored when crates are published"
            .to_string(),
        )
      };

      let issue_list: Vec<_> = issues.values().cloned().collect();

      Ok(CheckResult {
        check_name: self.name().to_string(),
        passed: false,
        severity,
        message,
        suggestion,
        details: Some(serde_json::json!({
          "issues": issue_list,
          "policy_forbids": policy_forbids,
        })),
      })
    }
  }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PatchReplaceIssue {
  crate_name: String,
  manifest_path: String,
  sections: Vec<String>,
  is_publishable: bool,
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;

  #[test]
  fn test_check_name() {
    let check = PatchReplaceCheck;
    assert_eq!(check.name(), "patch-replace-usage");
  }

  #[test]
  fn test_check_on_cargo_rail() {
    // Test on cargo-rail itself (should have no patch/replace in workspace members)
    let ctx = CheckContext {
      workspace_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
      crate_name: None,
      thorough: false,
    };

    let check = PatchReplaceCheck;
    let result = check.run(&ctx).unwrap();

    // cargo-rail should not have patch/replace in members
    assert!(
      result.passed,
      "cargo-rail workspace members should not have [patch]/[replace]"
    );
  }
}
