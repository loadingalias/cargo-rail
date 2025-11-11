//! Health check command for diagnosing issues
//!
//! The doctor command runs all health checks and reports any issues found.

use std::env;

use crate::checks::{CheckContext, create_default_runner};
use crate::core::error::{ExitCode, RailError, RailResult};

/// Run the doctor command to diagnose issues
///
/// Returns Ok(()) if all checks pass, or exits with error code if checks fail
pub fn run_doctor(thorough: bool, json: bool) -> RailResult<()> {
  let current_dir = env::current_dir()?;

  let ctx = CheckContext {
    workspace_root: current_dir,
    crate_name: None,
    thorough,
  };

  let runner = create_default_runner();
  let results = runner.run_all(&ctx)?;

  if json {
    // JSON output for CI/automation
    let json_output = serde_json::to_string_pretty(&results)
      .map_err(|e| RailError::message(format!("Failed to serialize JSON: {}", e)))?;
    println!("{}", json_output);
  } else {
    // Human-readable output
    println!("🏥 Running health checks...\n");

    let mut has_errors = false;
    let mut has_warnings = false;

    // Show what checks are registered
    println!("📋 Registered checks:");
    for check in runner.checks() {
      println!("   • {}: {}", check.name(), check.description());
    }
    println!();

    for result in &results {
      let icon = if result.passed { "✅" } else { "❌" };
      println!("{} {}: {}", icon, result.check_name, result.message);

      if !result.passed {
        if let Some(ref suggestion) = result.suggestion {
          println!("   💡 Fix: {}", suggestion);
        }

        match result.severity {
          crate::checks::Severity::Error => has_errors = true,
          crate::checks::Severity::Warning => has_warnings = true,
          _ => {}
        }
      }
      println!();
    }

    // Summary
    let passed_count = results.iter().filter(|r| r.passed).count();
    let total_count = results.len();

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("Summary: {}/{} checks passed", passed_count, total_count);

    if has_errors {
      println!("\n⚠️  Critical issues found. Please fix errors before proceeding.");
      std::process::exit(ExitCode::Validation.as_i32());
    } else if has_warnings {
      println!("\n⚠️  Some warnings found. Consider addressing them.");
    } else {
      println!("\n✨ All checks passed! Your setup looks healthy.");
    }
  }

  Ok(())
}

/// Run a quick pre-flight check before operations
///
/// This is useful for commands that want to verify the environment is ready
/// before starting work. Returns true if all checks pass, false otherwise.
pub fn run_preflight_check(thorough: bool) -> RailResult<bool> {
  let current_dir = env::current_dir()?;

  let ctx = CheckContext {
    workspace_root: current_dir,
    crate_name: None,
    thorough,
  };

  let runner = create_default_runner();
  runner.run_all_and_check(&ctx)
}

/// Run checks for a specific crate
///
/// Useful for validating a single crate before split/sync operations
pub fn run_crate_check(crate_name: &str, thorough: bool) -> RailResult<bool> {
  let current_dir = env::current_dir()?;

  let ctx = CheckContext {
    workspace_root: current_dir,
    crate_name: Some(crate_name.to_string()),
    thorough,
  };

  let runner = create_default_runner();
  runner.run_all_and_check(&ctx)
}
