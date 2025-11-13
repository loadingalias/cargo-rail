//! Check trait abstraction for health checks and validations
//!
//! This module provides a unified interface for running health checks and validations.
//! All checks implement the `Check` trait, making it easy to add new checks without
//! modifying core logic.
//!
//! Built-in checks include:
//! - Workspace validity (Cargo.toml, structure)
//! - SSH key validation (existence, permissions, connectivity)
//! - Git-notes integrity (mappings valid, no orphans)
//! - Remote accessibility (can reach remotes)
//!
//! Future extensions (WASM plugins, custom org checks) can implement the same trait.

use crate::core::error::RailResult;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Severity level for check results
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
  /// Informational message (not an issue)
  Info,
  /// Warning (non-blocking, but should be addressed)
  Warning,
  /// Error (blocking, must be fixed)
  Error,
}

impl fmt::Display for Severity {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Severity::Info => write!(f, "INFO"),
      Severity::Warning => write!(f, "WARN"),
      Severity::Error => write!(f, "ERROR"),
    }
  }
}

/// Result of running a check
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
  /// Name of the check that ran
  pub check_name: String,
  /// Whether the check passed
  pub passed: bool,
  /// Severity level (if failed)
  pub severity: Severity,
  /// Human-readable message
  pub message: String,
  /// Optional suggested fix
  pub suggestion: Option<String>,
  /// Additional metadata (for JSON output)
  #[serde(skip_serializing_if = "Option::is_none")]
  pub details: Option<serde_json::Value>,
}

impl CheckResult {
  /// Create a passing check result
  pub fn pass(check_name: impl Into<String>, message: impl Into<String>) -> Self {
    Self {
      check_name: check_name.into(),
      passed: true,
      severity: Severity::Info,
      message: message.into(),
      suggestion: None,
      details: None,
    }
  }

  /// Create a failing check result with error severity
  pub fn error(
    check_name: impl Into<String>,
    message: impl Into<String>,
    suggestion: Option<impl Into<String>>,
  ) -> Self {
    Self {
      check_name: check_name.into(),
      passed: false,
      severity: Severity::Error,
      message: message.into(),
      suggestion: suggestion.map(|s| s.into()),
      details: None,
    }
  }

  /// Create a failing check result with warning severity
  pub fn warning(
    check_name: impl Into<String>,
    message: impl Into<String>,
    suggestion: Option<impl Into<String>>,
  ) -> Self {
    Self {
      check_name: check_name.into(),
      passed: false,
      severity: Severity::Warning,
      message: message.into(),
      suggestion: suggestion.map(|s| s.into()),
      details: None,
    }
  }

  /// Add details to the check result
  #[allow(dead_code)]
  pub fn with_details(mut self, details: serde_json::Value) -> Self {
    self.details = Some(details);
    self
  }
}

/// Context passed to checks
#[derive(Debug, Clone)]
pub struct CheckContext {
  /// Workspace root directory
  pub workspace_root: std::path::PathBuf,
  /// Optional specific crate to check (None = check all)
  pub crate_name: Option<String>,
  /// Whether to run expensive checks (e.g., remote connectivity)
  pub thorough: bool,
}

/// Health check trait
///
/// Each check implements this trait to provide validation logic.
/// Checks can be run individually or in batch via the CheckRunner.
///
/// # Example
///
/// ```rust,ignore
/// use cargo_rail::checks::{Check, CheckContext, CheckResult};
///
/// struct MyCheck;
///
/// impl Check for MyCheck {
///   fn name(&self) -> &str {
///     "my-custom-check"
///   }
///
///   fn description(&self) -> &str {
///     "Validates my custom requirement"
///   }
///
///   fn run(&self, ctx: &CheckContext) -> RailResult<CheckResult> {
///     // Run validation logic
///     if everything_ok {
///       Ok(CheckResult::pass(self.name(), "All good!"))
///     } else {
///       Ok(CheckResult::error(
///         self.name(),
///         "Something is wrong",
///         Some("Try fixing it this way")
///       ))
///     }
///   }
/// }
/// ```
pub trait Check: Send + Sync {
  /// Unique name for this check (kebab-case)
  fn name(&self) -> &str;

  /// Human-readable description of what this check validates
  fn description(&self) -> &str;

  /// Run the check and return a result
  fn run(&self, ctx: &CheckContext) -> RailResult<CheckResult>;

  /// Whether this check is expensive (requires network, etc.)
  /// Default: false
  fn is_expensive(&self) -> bool {
    false
  }

  /// Whether this check requires a specific crate context
  /// Default: false (can run at workspace level)
  fn requires_crate(&self) -> bool {
    false
  }
}
