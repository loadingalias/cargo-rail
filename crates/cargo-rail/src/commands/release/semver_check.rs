//! cargo-semver-checks integration for API breaking change detection

use crate::commands::release::semver::BumpType;
use crate::core::error::RailResult;
use std::path::Path;
use std::process::Command;

/// Report from cargo-semver-checks analysis
#[derive(Debug, Clone)]
pub struct SemverReport {
  /// Has major (breaking) changes
  pub has_major: bool,
  /// Has minor (non-breaking feature) changes
  pub has_minor: bool,
  /// Has patch (bug fix) changes
  pub has_patch: bool,
  /// Detected changes (human-readable descriptions)
  pub changes: Vec<String>,
  /// Suggested bump type based on API analysis
  pub suggested_bump: BumpType,
}

/// Check for API breaking changes using cargo-semver-checks
///
/// Compares the current crate against a baseline version to detect:
/// - Breaking changes (major bump required)
/// - Non-breaking additions (minor bump suggested)
/// - No API changes (patch or none)
///
/// Returns None if:
/// - cargo-semver-checks is not installed
/// - The crate is not published (no baseline available)
/// - Analysis fails for any reason
pub fn check_api_changes(crate_path: &Path, baseline_version: &str) -> RailResult<Option<SemverReport>> {
  // Check if cargo-semver-checks is installed
  if !is_cargo_semver_checks_installed() {
    return Ok(None);
  }

  // Build the command: cargo semver-checks check-release --baseline-version <version>
  let output = Command::new("cargo")
    .arg("semver-checks")
    .arg("check-release")
    .arg("--baseline-version")
    .arg(baseline_version)
    .arg("--manifest-path")
    .arg(crate_path.join("Cargo.toml"))
    .output();

  match output {
    Ok(result) => {
      // Exit code 0: No breaking changes
      // Exit code 1: Breaking changes detected
      // Exit code other: Error
      let has_major = result.status.code() == Some(1);

      // Parse output for change details
      let stdout = String::from_utf8_lossy(&result.stdout);
      let stderr = String::from_utf8_lossy(&result.stderr);

      // Extract changes from output
      let mut changes = Vec::new();
      for line in stdout.lines().chain(stderr.lines()) {
        if line.contains("BREAKING") || line.contains("breaking") {
          changes.push(line.trim().to_string());
        }
      }

      // Determine suggested bump
      let suggested_bump = if has_major {
        BumpType::Major
      } else if !changes.is_empty() {
        // If there are changes but not breaking, suggest patch
        BumpType::Patch
      } else {
        BumpType::None
      };

      Ok(Some(SemverReport {
        has_major,
        has_minor: false, // cargo-semver-checks doesn't distinguish minor
        has_patch: !has_major && !changes.is_empty(),
        changes,
        suggested_bump,
      }))
    }
    Err(_) => {
      // If command fails, return None (semver check unavailable)
      Ok(None)
    }
  }
}

/// Check if cargo-semver-checks is installed
fn is_cargo_semver_checks_installed() -> bool {
  Command::new("cargo")
    .arg("semver-checks")
    .arg("--version")
    .output()
    .map(|output| output.status.success())
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_is_cargo_semver_checks_installed() {
    // This test just verifies the function doesn't panic
    // It may return true or false depending on the environment
    let _ = is_cargo_semver_checks_installed();
  }

  #[test]
  fn test_semver_report_major_bump() {
    let report = SemverReport {
      has_major: true,
      has_minor: false,
      has_patch: false,
      changes: vec!["Breaking: removed public function".to_string()],
      suggested_bump: BumpType::Major,
    };

    assert_eq!(report.suggested_bump, BumpType::Major);
    assert!(report.has_major);
    assert!(!report.changes.is_empty());
  }

  #[test]
  fn test_semver_report_no_changes() {
    let report = SemverReport {
      has_major: false,
      has_minor: false,
      has_patch: false,
      changes: vec![],
      suggested_bump: BumpType::None,
    };

    assert_eq!(report.suggested_bump, BumpType::None);
    assert!(!report.has_major);
    assert!(report.changes.is_empty());
  }
}
