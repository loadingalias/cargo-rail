//! Minimum Supported Rust Version (MSRV) check
//!
//! Validates that workspace crates specify rust-version and comply with policy MSRV.

use super::trait_def::{Check, CheckContext, CheckResult, Severity};
use crate::core::config::RailConfig;
use crate::core::error::RailResult;
use cargo_metadata::MetadataCommand;
use semver::Version;
use std::collections::HashMap;

/// Check for MSRV compliance
pub struct MSRVCheck;

impl Check for MSRVCheck {
  fn name(&self) -> &'static str {
    "msrv-compliance"
  }

  fn description(&self) -> &'static str {
    "Validate rust-version against policy MSRV"
  }

  fn run(&self, ctx: &CheckContext) -> RailResult<CheckResult> {
    // Load policy config
    let policy_msrv = match RailConfig::load(&ctx.workspace_root) {
      Ok(config) => config.policy.msrv,
      Err(_) => None, // No config or policy is fine
    };

    // If no policy MSRV configured, pass
    let Some(required_msrv_str) = policy_msrv else {
      return Ok(CheckResult::pass(self.name(), "No MSRV policy configured (skipped)"));
    };

    // Parse policy MSRV
    let required_msrv = match Version::parse(&required_msrv_str) {
      Ok(v) => v,
      Err(e) => {
        return Ok(CheckResult::error(
          self.name(),
          format!("Invalid MSRV in policy: '{}' ({})", required_msrv_str, e),
          Some("Fix the MSRV format in rail.toml [policy] section"),
        ));
      }
    };

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

    // Check each workspace package
    let mut issues: HashMap<String, MSRVIssue> = HashMap::new();

    for package in metadata.workspace_packages() {
      if let Some(rust_version) = &package.rust_version {
        // Parse the crate's rust-version
        match Version::parse(&rust_version.to_string()) {
          Ok(crate_msrv) => {
            // Check if crate MSRV is >= policy MSRV
            if crate_msrv < required_msrv {
              issues.insert(
                package.name.to_string(),
                MSRVIssue {
                  crate_name: package.name.to_string(),
                  specified_msrv: rust_version.to_string(),
                  required_msrv: required_msrv_str.clone(),
                  reason: "rust-version is lower than policy MSRV".to_string(),
                },
              );
            }
          }
          Err(_) => {
            issues.insert(
              package.name.to_string(),
              MSRVIssue {
                crate_name: package.name.to_string(),
                specified_msrv: rust_version.to_string(),
                required_msrv: required_msrv_str.clone(),
                reason: "invalid rust-version format".to_string(),
              },
            );
          }
        }
      } else {
        // No rust-version specified
        issues.insert(
          package.name.to_string(),
          MSRVIssue {
            crate_name: package.name.to_string(),
            specified_msrv: "none".to_string(),
            required_msrv: required_msrv_str.clone(),
            reason: "rust-version not specified".to_string(),
          },
        );
      }
    }

    if issues.is_empty() {
      Ok(CheckResult::pass(
        self.name(),
        format!("All crates comply with MSRV policy (>= {})", required_msrv_str),
      ))
    } else {
      let issue_list: Vec<_> = issues.values().cloned().collect();

      Ok(CheckResult {
        check_name: self.name().to_string(),
        passed: false,
        severity: Severity::Error,
        message: format!(
          "{} crate(s) do not comply with MSRV policy (>= {})",
          issues.len(),
          required_msrv_str
        ),
        suggestion: Some(format!(
          "Add or update 'rust-version = \"{}\"' in Cargo.toml for non-compliant crates",
          required_msrv_str
        )),
        details: Some(serde_json::json!({
          "required_msrv": required_msrv_str,
          "issues": issue_list,
        })),
      })
    }
  }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct MSRVIssue {
  crate_name: String,
  specified_msrv: String,
  required_msrv: String,
  reason: String,
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;

  #[test]
  fn test_check_name() {
    let check = MSRVCheck;
    assert_eq!(check.name(), "msrv-compliance");
  }

  #[test]
  fn test_check_without_policy() {
    // Test on workspace without MSRV policy (should pass)
    let ctx = CheckContext {
      workspace_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
      crate_name: None,
      thorough: false,
    };

    let check = MSRVCheck;
    let result = check.run(&ctx).unwrap();

    // Should pass when no policy configured
    assert!(result.passed, "Check should pass without MSRV policy");
  }

  #[test]
  fn test_version_parsing() {
    // Test that we can parse standard Rust version strings
    assert!(Version::parse("1.76.0").is_ok());
    assert!(Version::parse("1.80.0").is_ok());
    assert!(Version::parse("1.80.1").is_ok());
  }
}
