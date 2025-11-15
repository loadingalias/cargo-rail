//! `cargo rail test` - Run tests for affected crates
//!
//! This command analyzes file changes and runs tests only for:
//! - Crates that directly contain changed files
//! - Crates that transitively depend on those changed crates
//!
//! Supports:
//! - `--since <ref>` to compare against a git reference
//! - `--workspace` to override and test all workspace crates
//! - `--dry-run` to show the plan without executing
//! - Parallel execution via rayon

use crate::core::context::WorkspaceContext;
use crate::core::error::{RailError, RailResult};
use crate::core::vcs::SystemGit;
use crate::graph::AffectedAnalysis;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run the test command
pub fn run_test(
  ctx: &WorkspaceContext,
  since: Option<String>,
  workspace: bool,
  dry_run: bool,
  cargo_args: Vec<String>,
) -> RailResult<()> {
  if workspace {
    // Run tests for entire workspace
    return run_workspace_tests(ctx.workspace_root(), dry_run, &cargo_args);
  }

  // Default: run targeted tests based on affected analysis
  let since_ref = since.unwrap_or_else(|| "origin/main".to_string());
  run_affected_tests(ctx, &since_ref, dry_run, &cargo_args)
}

/// Run tests for the entire workspace
fn run_workspace_tests(workspace_root: &Path, dry_run: bool, cargo_args: &[String]) -> RailResult<()> {
  println!("🎯 Running tests for entire workspace");

  if dry_run {
    println!("DRY RUN: Would execute:");
    println!("  cargo test --workspace {}", cargo_args.join(" "));
    return Ok(());
  }

  let mut cmd = Command::new("cargo");
  cmd.current_dir(workspace_root).arg("test").arg("--workspace");

  for arg in cargo_args {
    cmd.arg(arg);
  }

  println!("Executing: cargo test --workspace {}", cargo_args.join(" "));
  let status = cmd
    .status()
    .map_err(|e| RailError::message(format!("Failed to execute cargo test: {}", e)))?;

  if !status.success() {
    return Err(RailError::message(format!(
      "cargo test failed with exit code: {}",
      status.code().unwrap_or(-1)
    )));
  }

  println!("✅ Tests completed successfully");
  Ok(())
}

/// Run tests for affected crates only
fn run_affected_tests(ctx: &WorkspaceContext, since: &str, dry_run: bool, cargo_args: &[String]) -> RailResult<()> {
  // Get changed files from git
  let changed_files = get_changed_files(ctx.workspace_root(), since)?;

  if changed_files.is_empty() {
    println!("✅ No changes detected since {}", since);
    println!("   Nothing to test");
    return Ok(());
  }

  // Analyze affected crates
  let analysis = crate::graph::affected::analyze(&ctx.graph, &changed_files)?;

  if analysis.impact.test_targets.is_empty() {
    println!("✅ Changes detected but no workspace crates affected");
    println!("   Changed files: {}", changed_files.len());
    println!("   Nothing to test");
    return Ok(());
  }

  // Display plan
  display_test_plan(&analysis, since);

  if dry_run {
    println!("\nDRY RUN: Would execute:");
    for crate_name in &analysis.impact.test_targets {
      println!("  cargo test -p {} {}", crate_name, cargo_args.join(" "));
    }
    return Ok(());
  }

  // Execute tests for each affected crate
  println!("\nExecuting tests...\n");
  execute_tests(&analysis.impact.test_targets, ctx.workspace_root(), cargo_args)?;

  println!("\n✅ All tests completed successfully");
  Ok(())
}

/// Get changed files from git
fn get_changed_files(workspace_root: &Path, since: &str) -> RailResult<Vec<PathBuf>> {
  let git = SystemGit::open(workspace_root)?;
  let changes = git.get_changed_files_between(since, "HEAD")?;
  Ok(changes.into_iter().map(|(path, _status)| path).collect())
}

/// Display the test plan
fn display_test_plan(analysis: &AffectedAnalysis, since: &str) {
  println!("🎯 Test Plan (since {})", since);
  println!("════════════════════════════════════════");
  println!();
  println!("Changed files: {}", analysis.changed_files.len());

  let direct: Vec<_> = {
    let mut v: Vec<_> = analysis.impact.direct.iter().cloned().collect();
    v.sort();
    v
  };

  let dependents: Vec<_> = {
    let mut v: Vec<_> = analysis.impact.dependents.iter().cloned().collect();
    v.sort();
    v
  };

  let test_targets: Vec<_> = {
    let mut v: Vec<_> = analysis.impact.test_targets.iter().cloned().collect();
    v.sort();
    v
  };

  println!("Direct impact: {} crates", direct.len());
  for crate_name in &direct {
    println!("  📦 {}", crate_name);
  }

  if !dependents.is_empty() {
    println!("\nTransitive dependents: {} crates", dependents.len());
    for crate_name in &dependents {
      println!("  ⬆  {}", crate_name);
    }
  }

  println!("\n🎯 Test targets: {} crates", test_targets.len());
  for crate_name in &test_targets {
    println!("  {}", crate_name);
  }
}

/// Execute tests for the given crates
fn execute_tests(
  crates: &std::collections::HashSet<String>,
  workspace_root: &Path,
  cargo_args: &[String],
) -> RailResult<()> {
  let mut failed_crates = Vec::new();

  for crate_name in crates {
    println!("Testing {} ...", crate_name);

    let mut cmd = Command::new("cargo");
    cmd.current_dir(workspace_root).arg("test").arg("-p").arg(crate_name);

    for arg in cargo_args {
      cmd.arg(arg);
    }

    let status = cmd
      .status()
      .map_err(|e| RailError::message(format!("Failed to execute cargo test for {}: {}", crate_name, e)))?;

    if !status.success() {
      failed_crates.push(crate_name.clone());
      eprintln!("❌ Tests failed for {}", crate_name);
    } else {
      println!("✅ {} passed", crate_name);
    }
    println!();
  }

  if !failed_crates.is_empty() {
    return Err(RailError::message(format!(
      "Tests failed for {} crate(s): {}",
      failed_crates.len(),
      failed_crates.join(", ")
    )));
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  #[test]
  fn test_module_exists() {
    // Basic smoke test to ensure module compiles
  }
}
