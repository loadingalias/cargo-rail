//! Health check command for diagnosing issues
//!
//! The doctor command runs all health checks and reports any issues found.

use anyhow::Result;
use std::env;

use crate::checks::{CheckContext, create_default_runner};

/// Run the doctor command to diagnose issues
pub fn run_doctor(thorough: bool, json: bool) -> Result<()> {
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
    let json_output = serde_json::to_string_pretty(&results)?;
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
      std::process::exit(3); // Exit code 3 for validation failures
    } else if has_warnings {
      println!("\n⚠️  Some warnings found. Consider addressing them.");
    } else {
      println!("\n✨ All checks passed! Your setup looks healthy.");
    }
  }

  Ok(())
}
