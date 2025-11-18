//! Smart test runner - only test affected crates

use crate::error::RailResult;
use crate::workspace::{ChangeImpact, WorkspaceContext};
use std::process::Command;

/// Run tests only for crates affected by changes since a given ref
pub fn run_test(ctx: &WorkspaceContext, since: String, test_args: Vec<String>) -> RailResult<()> {
  let analyzer = ChangeImpact::new(ctx);

  // Analyze changes since the given ref
  let impact = analyzer.analyze_changes(&since, "HEAD")?;

  // Get minimal test set
  let test_targets = impact.minimal_test_set();

  if test_targets.is_empty() {
    println!("✓ No affected crates - all tests skipped");
    return Ok(());
  }

  println!("Running tests for {} affected crate(s):", test_targets.len());
  for target in &test_targets {
    println!("  • {}", target);
  }
  println!();

  // Build cargo test command with package filters
  let mut cmd = Command::new("cargo");
  cmd.arg("test");

  // Add package filters for each affected crate
  for target in &test_targets {
    cmd.arg("-p").arg(target);
  }

  // Add user-provided test arguments
  if !test_args.is_empty() {
    cmd.arg("--");
    cmd.args(&test_args);
  }

  // Run the test command
  let status = cmd
    .status()
    .map_err(|e| crate::error::RailError::message(format!("Failed to run cargo test: {}", e)))?;

  if !status.success() {
    std::process::exit(status.code().unwrap_or(1));
  }

  println!("\n✓ All affected tests passed");

  Ok(())
}
