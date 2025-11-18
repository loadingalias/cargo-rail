//! Optional per-target validation for workspace dependency unification
//!
//! This module provides parallel validation against specific target triples to catch
//! platform-specific issues that might not be visible in the default --all-features analysis.
//!
//! Validation is opt-in via rail.toml `[unify] validate_targets = [...]` or CLI flag.

use crate::error::{RailError, RailResult};
use rayon::prelude::*;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Result of validating a single target
#[derive(Debug, Clone)]
pub struct TargetValidationResult {
  pub target: String,
  pub success: bool,
  pub error: Option<String>,
  pub warnings: Vec<String>,
}

/// Summary of all target validations
#[derive(Debug)]
pub struct ValidationSummary {
  pub results: Vec<TargetValidationResult>,
  pub total_targets: usize,
  pub successful: usize,
  pub failed: usize,
}

impl ValidationSummary {
  /// Check if all validations passed
  pub fn all_passed(&self) -> bool {
    self.failed == 0
  }

  /// Get failed targets
  pub fn failed_targets(&self) -> Vec<&str> {
    self
      .results
      .iter()
      .filter(|r| !r.success)
      .map(|r| r.target.as_str())
      .collect()
  }
}

/// Validate workspace unification against multiple target triples in parallel
///
/// This runs `cargo metadata --all-features --filter-platform=<triple>` for each target
/// and validates that the workspace still resolves correctly.
///
/// Uses Rayon for parallel execution with configurable parallelism.
pub fn validate_targets(
  workspace_root: &Path,
  targets: &[String],
  max_parallel_jobs: usize,
) -> RailResult<ValidationSummary> {
  if targets.is_empty() {
    return Ok(ValidationSummary {
      results: vec![],
      total_targets: 0,
      successful: 0,
      failed: 0,
    });
  }

  // Configure Rayon thread pool
  let pool = rayon::ThreadPoolBuilder::new()
    .num_threads(max_parallel_jobs)
    .build()
    .map_err(|e| RailError::message(format!("Failed to build thread pool: {}", e)))?;

  // Shared result collector (thread-safe)
  let results = Arc::new(Mutex::new(Vec::new()));

  // Run validations in parallel
  pool.install(|| {
    targets.par_iter().for_each(|target| {
      let result = validate_single_target(workspace_root, target);
      results.lock().unwrap().push(result);
    });
  });

  // Collect results
  let results = Arc::try_unwrap(results).unwrap().into_inner().unwrap();
  let successful = results.iter().filter(|r| r.success).count();
  let failed = results.iter().filter(|r| !r.success).count();

  Ok(ValidationSummary {
    total_targets: targets.len(),
    successful,
    failed,
    results,
  })
}

/// Validate a single target triple
fn validate_single_target(workspace_root: &Path, target: &str) -> TargetValidationResult {
  // Try to load metadata with --all-features and --filter-platform
  let result = std::process::Command::new("cargo")
    .arg("metadata")
    .arg("--all-features")
    .arg("--filter-platform")
    .arg(target)
    .arg("--format-version=1")
    .current_dir(workspace_root)
    .output();

  match result {
    Ok(output) => {
      // Collect warnings from stderr (even on success)
      let warnings = extract_warnings_from_stderr(&output.stderr);

      if output.status.success() {
        // Try to parse the metadata to ensure it's valid
        match serde_json::from_slice::<serde_json::Value>(&output.stdout) {
          Ok(_) => {
            // Success - workspace resolves for this target
            TargetValidationResult {
              target: target.to_string(),
              success: true,
              error: None,
              warnings,
            }
          }
          Err(e) => {
            // Failed to parse metadata JSON
            TargetValidationResult {
              target: target.to_string(),
              success: false,
              error: Some(format!("Failed to parse metadata: {}", e)),
              warnings,
            }
          }
        }
      } else {
        // cargo metadata failed
        let stderr = String::from_utf8_lossy(&output.stderr);
        TargetValidationResult {
          target: target.to_string(),
          success: false,
          error: Some(format!("cargo metadata failed: {}", stderr)),
          warnings,
        }
      }
    }
    Err(e) => {
      // Failed to execute cargo metadata
      TargetValidationResult {
        target: target.to_string(),
        success: false,
        error: Some(format!("Failed to execute cargo: {}", e)),
        warnings: vec![],
      }
    }
  }
}

/// Extract warning messages from cargo metadata stderr
///
/// Parses cargo's diagnostic output to extract warnings about:
/// - Platform-specific dependencies
/// - Unavailable features
/// - Target-specific issues
fn extract_warnings_from_stderr(stderr: &[u8]) -> Vec<String> {
  let stderr_str = String::from_utf8_lossy(stderr);
  let mut warnings = Vec::new();

  for line in stderr_str.lines() {
    let line = line.trim();

    // Collect lines that look like warnings
    let is_warning = line.starts_with("warning:")
      || line.contains("only available on")
      || line.contains("not available on")
      || (line.contains("feature") && (line.contains("unavailable") || line.contains("unsupported")))
      || (line.contains("-specific") && line.contains("crate"));

    if is_warning {
      warnings.push(line.to_string());
    }
  }

  warnings
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_validate_single_target_host() {
    // Use current directory as test workspace
    let current_dir = std::env::current_dir().unwrap();

    // Validate against host target (should always work)
    let host_target = std::env::consts::ARCH.to_string() + "-" + std::env::consts::OS;

    // Map common OS names to target triple format
    let host_target = match std::env::consts::OS {
      "macos" => format!("{}-apple-darwin", std::env::consts::ARCH),
      "linux" => format!("{}-unknown-linux-gnu", std::env::consts::ARCH),
      "windows" => format!("{}-pc-windows-msvc", std::env::consts::ARCH),
      _ => host_target,
    };

    let result = validate_single_target(&current_dir, &host_target);
    assert!(result.success, "Host target validation should succeed");
    assert!(result.error.is_none(), "Should have no error");
  }

  #[test]
  fn test_validate_single_target_invalid() {
    let current_dir = std::env::current_dir().unwrap();

    // Use an invalid target triple
    let result = validate_single_target(&current_dir, "invalid-target-triple");
    assert!(!result.success, "Invalid target should fail");
    assert!(result.error.is_some(), "Should have an error message");
  }

  #[test]
  fn test_validation_summary_all_passed() {
    let summary = ValidationSummary {
      results: vec![
        TargetValidationResult {
          target: "target1".to_string(),
          success: true,
          error: None,
          warnings: vec![],
        },
        TargetValidationResult {
          target: "target2".to_string(),
          success: true,
          error: None,
          warnings: vec![],
        },
      ],
      total_targets: 2,
      successful: 2,
      failed: 0,
    };

    assert!(summary.all_passed());
    assert!(summary.failed_targets().is_empty());
  }

  #[test]
  fn test_validation_summary_some_failed() {
    let summary = ValidationSummary {
      results: vec![
        TargetValidationResult {
          target: "target1".to_string(),
          success: true,
          error: None,
          warnings: vec![],
        },
        TargetValidationResult {
          target: "target2".to_string(),
          success: false,
          error: Some("Failed".to_string()),
          warnings: vec![],
        },
      ],
      total_targets: 2,
      successful: 1,
      failed: 1,
    };

    assert!(!summary.all_passed());
    assert_eq!(summary.failed_targets(), vec!["target2"]);
  }
}
