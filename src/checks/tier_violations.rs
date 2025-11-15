//! Tier violation checks for visibility-based release tiers
//!
//! Detects violations where lower-tier releases (e.g., OSS) depend on
//! higher-tier crates (e.g., enterprise), which would break the release.

use crate::checks::trait_def::{Check, CheckContext, CheckResult, Severity};
use crate::core::config::{RailConfig, Visibility};
use crate::core::error::RailResult;
use crate::graph::workspace_graph::WorkspaceGraph;
use std::collections::HashSet;

/// Check for tier violations in release configurations.
///
/// A tier violation occurs when:
/// - An OSS release includes or depends on an internal/enterprise crate
/// - An internal release depends on an enterprise crate
///
/// This ensures releases can be published without exposing higher-tier code.
pub struct TierViolationCheck;

impl Check for TierViolationCheck {
  fn name(&self) -> &str {
    "tier-violations"
  }

  fn description(&self) -> &str {
    "Checks for tier violations (OSS depending on internal/enterprise)"
  }

  fn run(&self, ctx: &CheckContext) -> RailResult<CheckResult> {
    // Load config
    let config = match RailConfig::load(&ctx.workspace_root) {
      Ok(c) => c,
      Err(_) => {
        // No config = no releases = no tier violations
        return Ok(CheckResult {
          check_name: self.name().to_string(),
          passed: true,
          message: "No rail.toml found (tier checks require releases config)".to_string(),
          suggestion: None,
          severity: Severity::Info,
          details: None,
        });
      }
    };

    // If no releases configured, skip check
    if config.releases.is_empty() {
      return Ok(CheckResult {
        check_name: self.name().to_string(),
        passed: true,
        message: "No releases configured (tier checks require releases)".to_string(),
        suggestion: None,
        severity: Severity::Info,
        details: None,
      });
    }

    // Load graph with config to get visibility annotations
    let graph = WorkspaceGraph::load_with_config(&ctx.workspace_root, Some(&config))?;

    let mut violations = Vec::new();

    // Check each release for tier violations
    for release in &config.releases {
      // Get the primary crate for this release
      let primary_crate = release
        .crate_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&release.name);

      // Collect all crates in this release (primary + includes)
      let mut release_crates = HashSet::new();
      release_crates.insert(primary_crate.to_string());
      for included in &release.includes {
        release_crates.insert(included.clone());
      }

      // For each crate in the release, check its dependencies
      for crate_name in &release_crates {
        // Get all transitive dependencies (what this crate depends on)
        let mut to_check = vec![crate_name.clone()];
        let mut checked = HashSet::new();
        let mut all_deps = HashSet::new();

        while let Some(current) = to_check.pop() {
          if checked.contains(&current) {
            continue;
          }
          checked.insert(current.clone());

          // Get direct dependencies
          if let Ok(deps) = graph.direct_dependencies(&current) {
            for dep in deps {
              if !all_deps.contains(&dep) && graph.workspace_members().contains(&dep) {
                all_deps.insert(dep.clone());
                to_check.push(dep);
              }
            }
          }
        }

        // Check each transitive dependency's visibility
        for dep in all_deps {
          let dep_visibilities = graph.crate_visibilities(&dep);

          // Skip if dependency has no visibility (not in any release)
          if dep_visibilities.is_empty() {
            continue;
          }

          // Skip self-references
          if dep == *crate_name {
            continue;
          }

          // Check for violations based on release visibility
          match release.visibility {
            Visibility::Oss => {
              // OSS can't depend on internal or enterprise
              if dep_visibilities.contains(&Visibility::Internal) || dep_visibilities.contains(&Visibility::Enterprise)
              {
                violations.push(format!(
                  "Release '{}' (OSS) includes '{}' which depends on '{}' ({:?})",
                  release.name, crate_name, dep, dep_visibilities
                ));
              }
            }
            Visibility::Internal => {
              // Internal can't depend on enterprise
              if dep_visibilities.contains(&Visibility::Enterprise) {
                violations.push(format!(
                  "Release '{}' (internal) includes '{}' which depends on '{}' (enterprise)",
                  release.name, crate_name, dep
                ));
              }
            }
            Visibility::Enterprise => {
              // Enterprise can depend on anything
            }
          }
        }
      }
    }

    if violations.is_empty() {
      Ok(CheckResult {
        check_name: self.name().to_string(),
        passed: true,
        message: "No tier violations found".to_string(),
        suggestion: None,
        severity: Severity::Info,
        details: None,
      })
    } else {
      Ok(CheckResult {
        check_name: self.name().to_string(),
        passed: false,
        message: format!(
          "Found {} tier violation(s):\n  - {}",
          violations.len(),
          violations.join("\n  - ")
        ),
        suggestion: Some(
          "Review release configurations and remove higher-tier dependencies, or adjust release visibility".to_string(),
        ),
        severity: Severity::Error,
        details: None,
      })
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_check_name() {
    let check = TierViolationCheck;
    assert_eq!(check.name(), "tier-violations");
  }

  #[test]
  fn test_check_without_config() {
    use std::env;

    let temp_dir = env::temp_dir().join("cargo-rail-test-tier-no-config");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).unwrap();

    let ctx = CheckContext {
      workspace_root: temp_dir.clone(),
      crate_name: None,
      thorough: false,
    };

    let check = TierViolationCheck;
    let result = check.run(&ctx).unwrap();

    assert!(result.passed);
    assert!(result.message.contains("No rail.toml"));

    let _ = std::fs::remove_dir_all(&temp_dir);
  }
}
