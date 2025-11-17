//! `cargo rail check` - Run check for affected crates
//!
//! This command analyzes file changes and runs `cargo check` only for:
//! - Crates that directly contain changed files
//! - Crates that transitively depend on those changed crates
//!
//! Supports:
//! - `--since <ref>` to compare against a git reference
//! - `--workspace` to override and check all workspace crates
//! - `--dry-run` to show the plan without executing

use crate::error::{RailError, RailResult};
use crate::git::SystemGit;
use crate::graph::AffectedAnalysis;
use crate::workspace::WorkspaceContext;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run the check command
pub fn run_check(
  ctx: &WorkspaceContext,
  since: Option<String>,
  workspace: bool,
  dry_run: bool,
  cargo_args: Vec<String>,
) -> RailResult<()> {
  if workspace {
    // Run check for entire workspace
    return run_workspace_check(ctx.workspace_root(), dry_run, &cargo_args);
  }

  // Default: run targeted check based on affected analysis
  let since_ref = since.unwrap_or_else(|| "origin/main".to_string());
  run_affected_check(ctx, &since_ref, dry_run, &cargo_args)
}

/// Run check for the entire workspace
fn run_workspace_check(workspace_root: &Path, dry_run: bool, cargo_args: &[String]) -> RailResult<()> {
  println!("🔍 Running check for entire workspace");

  if dry_run {
    println!("DRY RUN: Would execute:");
    println!("  cargo check --workspace {}", cargo_args.join(" "));
    return Ok(());
  }

  let mut cmd = Command::new("cargo");
  cmd.current_dir(workspace_root).arg("check").arg("--workspace");

  for arg in cargo_args {
    cmd.arg(arg);
  }

  println!("Executing: cargo check --workspace {}", cargo_args.join(" "));
  let status = cmd
    .status()
    .map_err(|e| RailError::message(format!("Failed to execute cargo check: {}", e)))?;

  if !status.success() {
    return Err(RailError::message(format!(
      "cargo check failed with exit code: {}",
      status.code().unwrap_or(-1)
    )));
  }

  println!("✅ Check completed successfully");
  Ok(())
}

/// Run check for affected crates only
fn run_affected_check(ctx: &WorkspaceContext, since: &str, dry_run: bool, cargo_args: &[String]) -> RailResult<()> {
  // Get changed files from git
  let changed_files = get_changed_files(ctx.workspace_root(), since)?;

  if changed_files.is_empty() {
    println!("✅ No changes detected since {}", since);
    println!("   Nothing to check");
    return Ok(());
  }

  // Analyze affected crates
  let analysis = crate::graph::analyze(&ctx.graph, &changed_files)?;

  if analysis.impact.test_targets.is_empty() {
    println!("✅ Changes detected but no workspace crates affected");
    println!("   Changed files: {}", changed_files.len());
    println!("   Nothing to check");
    return Ok(());
  }

  // Display plan
  display_check_plan(&analysis, since);

  if dry_run {
    println!("\nDRY RUN: Would execute:");
    for crate_name in &analysis.impact.test_targets {
      println!("  cargo check -p {} {}", crate_name, cargo_args.join(" "));
    }
    return Ok(());
  }

  // Execute check for each affected crate
  println!("\nExecuting checks...\n");
  execute_checks(&analysis.impact.test_targets, ctx.workspace_root(), cargo_args)?;

  println!("\n✅ All checks completed successfully");
  Ok(())
}

/// Get changed files from git
fn get_changed_files(workspace_root: &Path, since: &str) -> RailResult<Vec<PathBuf>> {
  let git = SystemGit::open(workspace_root)?;
  let changes = git.get_changed_files_between(since, "HEAD")?;
  Ok(changes.into_iter().map(|(path, _status)| path).collect())
}

/// Display the check plan
fn display_check_plan(analysis: &AffectedAnalysis, since: &str) {
  println!("🔍 Check Plan (since {})", since);
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

  let targets: Vec<_> = {
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

  println!("\n🔍 Check targets: {} crates", targets.len());
  for crate_name in &targets {
    println!("  {}", crate_name);
  }
}

/// Execute checks for the given crates
fn execute_checks(
  crates: &std::collections::HashSet<String>,
  workspace_root: &Path,
  cargo_args: &[String],
) -> RailResult<()> {
  let mut failed_crates = Vec::new();

  for crate_name in crates {
    println!("Checking {} ...", crate_name);

    let mut cmd = Command::new("cargo");
    cmd.current_dir(workspace_root).arg("check").arg("-p").arg(crate_name);

    for arg in cargo_args {
      cmd.arg(arg);
    }

    let status = cmd
      .status()
      .map_err(|e| RailError::message(format!("Failed to execute cargo check for {}: {}", crate_name, e)))?;

    if !status.success() {
      failed_crates.push(crate_name.clone());
      eprintln!("❌ Check failed for {}", crate_name);
    } else {
      println!("✅ {} passed", crate_name);
    }
    println!();
  }

  if !failed_crates.is_empty() {
    return Err(RailError::message(format!(
      "Check failed for {} crate(s): {}",
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
