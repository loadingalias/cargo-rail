//! `cargo rail unify` - Workspace dependency unification
//!
//! This command eliminates the need for workspace-hack crates and cargo-hakari by:
//! 1. Analyzing all workspace dependencies
//! 2. Creating unified [workspace.dependencies] entries
//! 3. Converting member dependencies to use `workspace = true` inheritance
//!
//! Replaces cargo-hakari with native Cargo workspace dependency unification.

use crate::cargo::{CargoTransform, UnifyConfig, UnifyStrategy, WorkspaceUnifier};
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;
use std::path::Path;

/// Run dependency unification analysis (dry-run)
pub fn run_unify_analyze(
  ctx: &WorkspaceContext,
  exclude: Vec<String>,
  include: Vec<String>,
  normal_only: bool,
) -> RailResult<()> {
  println!("🔍 Analyzing workspace dependencies...\n");

  // Build config from CLI args (or load from rail.toml if we want)
  let mut config = UnifyConfig {
    strategy: UnifyStrategy::All, // Unify all duplicated dependencies
    normal_only,
    ..Default::default()
  };

  // Apply excludes/includes from CLI
  for dep in exclude {
    config.exclude.insert(dep);
  }
  for dep in include {
    config.include.insert(dep);
  }

  // Run analysis
  let unifier = WorkspaceUnifier::with_config(ctx.cargo.metadata(), config);
  let plan = unifier.analyze()?;

  // Display summary
  println!("{}", plan.summary());

  // Show issues if any
  if !plan.issues.is_empty() {
    println!("\n⚠️  Issues found:");
    for issue in &plan.issues {
      println!("  {}", issue.format_message());
    }
    println!("\nResolve these issues before running 'cargo rail unify apply'");
    return Err(RailError::message("Unification has blocking issues"));
  }

  println!("\n✅ Analysis complete. Run 'cargo rail unify apply' to make changes.");
  Ok(())
}

/// Apply dependency unification (modify files)
pub fn run_unify_apply(
  ctx: &WorkspaceContext,
  exclude: Vec<String>,
  include: Vec<String>,
  backup: bool,
  normal_only: bool,
) -> RailResult<()> {
  println!("🔧 Applying workspace dependency unification...\n");

  // Build config
  let mut config = UnifyConfig {
    strategy: UnifyStrategy::All,
    backup,
    normal_only,
    ..Default::default()
  };

  for dep in exclude {
    config.exclude.insert(dep);
  }
  for dep in include {
    config.include.insert(dep);
  }

  // Run analysis first
  let unifier = WorkspaceUnifier::with_config(ctx.cargo.metadata(), config);
  let plan = unifier.analyze()?;

  // Check for issues
  if !plan.issues.is_empty() {
    println!("⚠️  Cannot apply: unification has blocking issues:");
    for issue in &plan.issues {
      println!("  {}", issue.format_message());
    }
    return Err(RailError::message("Fix issues before applying"));
  }

  // Create transformer
  let transformer = CargoTransform::new(ctx.cargo.metadata().clone());

  // 1. Write [workspace.dependencies]
  let workspace_toml = ctx.workspace_root().join("Cargo.toml");
  println!("📝 Writing [workspace.dependencies] to {}", workspace_toml.display());

  // Backup if requested
  if backup {
    backup_file(&workspace_toml)?;
  }

  transformer.write_workspace_dependencies(&workspace_toml, &plan.workspace_deps)?;
  println!("  ✓ Wrote {} unified dependencies", plan.workspace_deps.len());

  // 2. Convert member dependencies to workspace inheritance
  println!("\n📝 Converting member dependencies to workspace inheritance...");

  for (member_name, edits) in &plan.member_edits {
    if edits.is_empty() {
      continue;
    }

    let member_pkg = ctx
      .cargo
      .get_package(member_name)
      .ok_or_else(|| RailError::message(format!("Package {} not found", member_name)))?;

    let member_toml = member_pkg.manifest_path.as_std_path();

    if backup {
      backup_file(member_toml)?;
    }

    for edit in edits {
      match edit {
        crate::cargo::unify::MemberEdit::UseWorkspace { dep_name, kind } => {
          transformer.convert_to_workspace_inheritance(member_toml, dep_name, kind)?;
        }
      }
    }

    println!("  ✓ {} ({} dependencies)", member_name, edits.len());
  }

  println!("\n✅ Unification complete!");
  println!("\nStats:");
  println!("  - {} dependencies unified", plan.stats.unified_count);
  println!("  - {} workspace members updated", plan.member_edits.len());
  println!(
    "  - {} redundant compilations eliminated",
    plan.stats.compilations_saved
  );

  if backup {
    println!("\n💾 Backups created with .bak extension");
  }

  println!("\nNext steps:");
  println!("  1. Run 'cargo check' to verify everything compiles");
  println!("  2. Test your workspace thoroughly");
  println!("  3. Commit the changes");

  Ok(())
}

/// Check workspace dependencies are properly unified (for CI)
pub fn run_unify_check(ctx: &WorkspaceContext, exclude: Vec<String>, normal_only: bool) -> RailResult<()> {
  println!("🔍 Checking workspace dependency unification...\n");

  // Build config
  let mut config = UnifyConfig {
    strategy: UnifyStrategy::All,
    normal_only,
    ..Default::default()
  };

  for dep in exclude {
    config.exclude.insert(dep);
  }

  // Run analysis
  let unifier = WorkspaceUnifier::with_config(ctx.cargo.metadata(), config);
  let plan = unifier.analyze()?;

  // Check for issues
  if !plan.issues.is_empty() {
    println!("❌ Unification check FAILED - issues detected:\n");
    for issue in &plan.issues {
      println!("  {}", issue.format_message());
    }
    println!("\nRun 'cargo rail unify analyze' for full details.");
    return Err(RailError::message("Workspace dependencies have unification issues"));
  }

  // Check if there are unifiable dependencies
  if !plan.workspace_deps.is_empty() {
    println!(
      "❌ Unification check FAILED - {} dependencies can be unified:\n",
      plan.workspace_deps.len()
    );
    for dep in plan.workspace_deps.iter().take(10) {
      println!("  {} (used by {} members)", dep.name, dep.used_by.len());
    }
    if plan.workspace_deps.len() > 10 {
      println!("  ... and {} more", plan.workspace_deps.len() - 10);
    }
    println!("\nRun 'cargo rail unify apply' to fix.");
    return Err(RailError::message("Workspace dependencies are not fully unified"));
  }

  println!("✅ Workspace dependencies are properly unified!");
  println!("\nStats:");
  println!("  - {} dependencies analyzed", plan.stats.total_deps);
  println!("  - No unification opportunities found");

  Ok(())
}

/// Create a backup of a file
fn backup_file(path: &Path) -> RailResult<()> {
  let backup_path = path.with_extension("toml.bak");
  std::fs::copy(path, &backup_path)
    .map_err(|e| RailError::message(format!("Failed to backup {}: {}", path.display(), e)))?;
  Ok(())
}
