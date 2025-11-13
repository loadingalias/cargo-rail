//! Check runner for executing health checks

use super::trait_def::{Check, CheckContext, CheckResult};
use crate::core::error::RailResult;
use std::sync::Arc;

/// Check runner that executes multiple checks
pub struct CheckRunner {
  checks: Vec<Arc<dyn Check>>,
}

impl CheckRunner {
  /// Create a new check runner
  pub fn new() -> Self {
    Self { checks: Vec::new() }
  }

  /// Add a check to the runner
  pub fn add_check(&mut self, check: Arc<dyn Check>) {
    self.checks.push(check);
  }

  /// Run all checks and collect results
  pub fn run_all(&self, ctx: &CheckContext) -> RailResult<Vec<CheckResult>> {
    let mut results = Vec::new();

    for check in &self.checks {
      // Skip expensive checks if not thorough mode
      if check.is_expensive() && !ctx.thorough {
        continue;
      }

      // Skip crate-specific checks if no crate specified
      if check.requires_crate() && ctx.crate_name.is_none() {
        continue;
      }

      match check.run(ctx) {
        Ok(result) => results.push(result),
        Err(err) => {
          // If a check itself fails to run, create an error result
          results.push(CheckResult::error(
            check.name(),
            format!("Check failed to run: {}", err),
            Some("Check the logs for more details"),
          ));
        }
      }
    }

    Ok(results)
  }

  /// Run all checks and return whether all passed
  ///
  /// This is a convenience method for programmatic health checks
  /// where you only need a boolean result.
  pub fn run_all_and_check(&self, ctx: &CheckContext) -> RailResult<bool> {
    let results = self.run_all(ctx)?;
    Ok(results.iter().all(|r| r.passed))
  }

  /// Get all registered checks
  pub fn checks(&self) -> &[Arc<dyn Check>] {
    &self.checks
  }
}

impl Default for CheckRunner {
  fn default() -> Self {
    Self::new()
  }
}

/// Create a runner with all built-in checks
pub fn create_default_runner() -> CheckRunner {
  let mut runner = CheckRunner::new();

  // Add built-in checks
  runner.add_check(Arc::new(super::workspace::WorkspaceValidityCheck));
  runner.add_check(Arc::new(super::ssh::SshKeyCheck));
  runner.add_check(Arc::new(super::security_config::SecurityConfigCheck));
  runner.add_check(Arc::new(super::git_notes::GitNotesCheck));
  runner.add_check(Arc::new(super::remotes::RemoteAccessCheck));

  runner
}
