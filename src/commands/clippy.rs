//! `cargo rail clippy` - Run clippy for affected crates
//!
//! This command analyzes file changes and runs `cargo clippy` only for:
//! - Crates that directly contain changed files
//! - Crates that transitively depend on those changed crates
//!
//! Supports:
//! - `--since <ref>` to compare against a git reference
//! - `--workspace` to override and lint all workspace crates
//! - `--dry-run` to show the plan without executing

use crate::core::context::WorkspaceContext;
use crate::core::error::{RailError, RailResult};
use crate::core::vcs::SystemGit;
use crate::graph::AffectedAnalysis;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run the clippy command
pub fn run_clippy(
  ctx: &WorkspaceContext,
  since: Option<String>,
  workspace: bool,
  dry_run: bool,
  cargo_args: Vec<String>,
) -> RailResult<()> {
  if workspace {
    // Run clippy for entire workspace
    return run_workspace_clippy(ctx.workspace_root(), dry_run, &cargo_args);
  }

  // Default: run targeted clippy based on affected analysis
  let since_ref = since.unwrap_or_else(|| "origin/main".to_string());
  run_affected_clippy(ctx, &since_ref, dry_run, &cargo_args)
}

/// Run clippy for the entire workspace
fn run_workspace_clippy(workspace_root: &Path, dry_run: bool, cargo_args: &[String]) -> RailResult<()> {
  println!("📎 Running clippy for entire workspace");

  if dry_run {
    println!("DRY RUN: Would execute:");
    println!("  cargo clippy --workspace {}", cargo_args.join(" "));
    return Ok(());
  }

  let mut cmd = Command::new("cargo");
  cmd.current_dir(workspace_root).arg("clippy").arg("--workspace");

  for arg in cargo_args {
    cmd.arg(arg);
  }

  println!("Executing: cargo clippy --workspace {}", cargo_args.join(" "));
  let status = cmd
    .status()
    .map_err(|e| RailError::message(format!("Failed to execute cargo clippy: {}", e)))?;

  if !status.success() {
    return Err(RailError::message(format!(
      "cargo clippy failed with exit code: {}",
      status.code().unwrap_or(-1)
    )));
  }

  println!("✅ Clippy completed successfully");
  Ok(())
}

/// Run clippy for affected crates only
fn run_affected_clippy(ctx: &WorkspaceContext, since: &str, dry_run: bool, cargo_args: &[String]) -> RailResult<()> {
  // Load workspace graph

  // Get changed files from git
  let changed_files = get_changed_files(ctx.workspace_root(), since)?;

  if changed_files.is_empty() {
    println!("✅ No changes detected since {}", since);
    println!("   Nothing to lint");
    return Ok(());
  }

  // Analyze affected crates
  let analysis = crate::graph::affected::analyze(&ctx.graph, &changed_files)?;

  if analysis.impact.test_targets.is_empty() {
    println!("✅ Changes detected but no workspace crates affected");
    println!("   Changed files: {}", changed_files.len());
    println!("   Nothing to lint");
    return Ok(());
  }

  // Display plan
  display_clippy_plan(&analysis, since);

  if dry_run {
    println!("\nDRY RUN: Would execute:");
    for crate_name in &analysis.impact.test_targets {
      println!("  cargo clippy -p {} {}", crate_name, cargo_args.join(" "));
    }
    return Ok(());
  }

  // Execute clippy for each affected crate
  println!("\nExecuting clippy...\n");
  execute_clippy(&analysis.impact.test_targets, ctx.workspace_root(), cargo_args)?;

  println!("\n✅ All clippy checks completed successfully");
  Ok(())
}

/// Get changed files from git
fn get_changed_files(workspace_root: &Path, since: &str) -> RailResult<Vec<PathBuf>> {
  let git = SystemGit::open(workspace_root)?;
  let changes = git.get_changed_files_between(since, "HEAD")?;
  Ok(changes.into_iter().map(|(path, _status)| path).collect())
}

/// Display the clippy plan
fn display_clippy_plan(analysis: &AffectedAnalysis, since: &str) {
  println!("📎 Clippy Plan (since {})", since);
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

  println!("\n📎 Clippy targets: {} crates", targets.len());
  for crate_name in &targets {
    println!("  {}", crate_name);
  }
}

/// Execute clippy for the given crates
fn execute_clippy(
  crates: &std::collections::HashSet<String>,
  workspace_root: &Path,
  cargo_args: &[String],
) -> RailResult<()> {
  let mut failed_crates = Vec::new();

  for crate_name in crates {
    println!("Linting {} ...", crate_name);

    let mut cmd = Command::new("cargo");
    cmd.current_dir(workspace_root).arg("clippy").arg("-p").arg(crate_name);

    for arg in cargo_args {
      cmd.arg(arg);
    }

    let status = cmd
      .status()
      .map_err(|e| RailError::message(format!("Failed to execute cargo clippy for {}: {}", crate_name, e)))?;

    if !status.success() {
      failed_crates.push(crate_name.clone());
      eprintln!("❌ Clippy failed for {}", crate_name);
    } else {
      println!("✅ {} passed", crate_name);
    }
    println!();
  }

  if !failed_crates.is_empty() {
    return Err(RailError::message(format!(
      "Clippy failed for {} crate(s): {}",
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
