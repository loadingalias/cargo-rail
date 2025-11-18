//! `cargo rail unify` - Workspace dependency unification
//!
//! This command eliminates the need for workspace-hack crates and cargo-hakari by:
//! 1. Analyzing all workspace dependencies
//! 2. Creating unified [workspace.dependencies] entries
//! 3. Converting member dependencies to use `workspace = true` inheritance
//!
//! Replaces cargo-hakari with native Cargo workspace dependency unification.

use crate::cargo::{CargoTransform, UnifyConfig, UnifyStrategy, WorkspaceMetadata, WorkspaceUnifier, validate_targets};
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;
use std::path::Path;

/// Get workspace metadata for unify operations
///
/// Checks rail.toml `[unify]` configuration to determine if --all-features should be used.
/// If use_all_features=true (default), reloads metadata with --all-features for accurate
/// feature union across the workspace.
fn get_unify_metadata(ctx: &WorkspaceContext) -> RailResult<WorkspaceMetadata> {
  // Try to load rail config (optional)
  let use_all_features = ctx
    .config
    .as_ref()
    .map(|cfg| cfg.unify.use_all_features)
    .unwrap_or(true); // Default to true if no config

  if use_all_features {
    // Reload metadata with --all-features for accurate feature resolution
    WorkspaceMetadata::load_with_features(ctx.workspace_root(), true, false, vec![])
  } else {
    // Use existing metadata from context
    Ok(ctx.cargo.metadata().clone())
  }
}

/// Run config sync if configured
fn maybe_sync_config(ctx: &WorkspaceContext) -> RailResult<()> {
  // Check if sync_on_unify is enabled (default: true)
  let sync_enabled = ctx.config.as_ref().map(|cfg| cfg.unify.sync_on_unify).unwrap_or(true);

  if sync_enabled {
    // Run config sync (non-check mode)
    crate::commands::run_config_sync(ctx, false)?;
    println!(); // Add newline separator
  }

  Ok(())
}

/// Run optional per-target validation if configured
fn maybe_run_validation(ctx: &WorkspaceContext) -> RailResult<()> {
  // Check if validation is enabled via config
  let (validate_targets_list, max_parallel_jobs) = if let Some(cfg) = ctx.config.as_ref() {
    if !cfg.unify.validation_enabled() {
      return Ok(()); // Validation not enabled
    }
    (cfg.unify.validate_targets.clone(), cfg.unify.effective_parallelism())
  } else {
    return Ok(()); // No config, validation not enabled
  };

  println!("🔍 Running optional per-target validation...\n");

  // Run parallel validation
  let summary = validate_targets(ctx.workspace_root(), &validate_targets_list, max_parallel_jobs)?;

  // Print summary
  println!("📊 Validation Summary:");
  println!("  Total targets: {}", summary.total_targets);
  println!("  Successful: {}", summary.successful);
  println!("  Failed: {}", summary.failed);
  println!();

  // Print detailed results
  for result in &summary.results {
    if result.success {
      println!("  ✓ {} - OK", result.target);
      // Show warnings even on success
      if !result.warnings.is_empty() {
        for warning in &result.warnings {
          println!("    ⚠️  {}", warning);
        }
      }
    } else {
      println!("  ✗ {} - FAILED", result.target);
      if let Some(ref error) = result.error {
        println!("    Error: {}", error);
      }
      if !result.warnings.is_empty() {
        for warning in &result.warnings {
          println!("    ⚠️  {}", warning);
        }
      }
    }
  }
  println!();

  // Fail if any target failed
  if !summary.all_passed() {
    return Err(RailError::message(format!(
      "Validation failed for {} target(s): {}",
      summary.failed,
      summary.failed_targets().join(", ")
    )));
  }

  println!("✅ All target validations passed!\n");
  Ok(())
}

/// Run dependency unification analysis (dry-run)
pub fn run_unify_analyze(
  ctx: &WorkspaceContext,
  exclude: Vec<String>,
  include: Vec<String>,
  normal_only: bool,
) -> RailResult<()> {
  // Sync config if enabled
  maybe_sync_config(ctx)?;

  println!("🔍 Analyzing workspace dependencies...\n");

  // Get metadata (with --all-features if configured)
  let metadata = get_unify_metadata(ctx)?;

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
  let unifier = WorkspaceUnifier::with_config(&metadata, config);
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

  // Run optional validation if configured
  maybe_run_validation(ctx)?;

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
  // Sync config if enabled
  maybe_sync_config(ctx)?;

  println!("🔧 Applying workspace dependency unification...\n");

  // Get metadata (with --all-features if configured)
  let metadata = get_unify_metadata(ctx)?;

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
  let unifier = WorkspaceUnifier::with_config(&metadata, config);
  let plan = unifier.analyze()?;

  // Check for issues
  if !plan.issues.is_empty() {
    println!("⚠️  Cannot apply: unification has blocking issues:");
    for issue in &plan.issues {
      println!("  {}", issue.format_message());
    }
    return Err(RailError::message("Fix issues before applying"));
  }

  // Run optional validation if configured (before applying changes)
  maybe_run_validation(ctx)?;

  // Create transformer (use the same metadata we used for analysis)
  let transformer = CargoTransform::new(metadata);

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
pub fn run_unify_check(
  ctx: &WorkspaceContext,
  exclude: Vec<String>,
  normal_only: bool,
  validate_targets_flag: bool,
) -> RailResult<()> {
  // Sync config check (don't modify, just validate)
  let sync_enabled = ctx.config.as_ref().map(|cfg| cfg.unify.sync_on_unify).unwrap_or(true);

  if sync_enabled {
    crate::commands::run_config_sync(ctx, true)?; // check=true
    println!();
  }

  println!("🔍 Checking workspace dependency unification...\n");

  // Get metadata (with --all-features if configured)
  let metadata = get_unify_metadata(ctx)?;

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
  let unifier = WorkspaceUnifier::with_config(&metadata, config);
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

  // Run optional per-target validation if CLI flag is set
  if validate_targets_flag {
    // Get targets from config
    let targets = if let Some(cfg) = ctx.config.as_ref() {
      if cfg.toolchain.targets.is_empty() {
        println!("\n⚠️  --validate-targets flag set but no targets configured in rail.toml [toolchain.targets]");
        println!("Add targets to .config/rail.toml to enable validation.");
        return Ok(());
      }
      cfg.toolchain.targets.clone()
    } else {
      println!("\n⚠️  --validate-targets flag set but no rail.toml found");
      println!("Create .config/rail.toml with [toolchain.targets] to enable validation.");
      return Ok(());
    };

    println!("\n🔍 Running per-target validation for {} targets...\n", targets.len());

    // Use configured parallelism or auto-detect
    let max_parallel_jobs = ctx
      .config
      .as_ref()
      .map(|cfg| cfg.unify.effective_parallelism())
      .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1));

    let summary = validate_targets(ctx.workspace_root(), &targets, max_parallel_jobs)?;

    // Print summary
    println!("📊 Validation Summary:");
    println!("  Total targets: {}", summary.total_targets);
    println!("  Successful: {}", summary.successful);
    println!("  Failed: {}", summary.failed);
    println!();

    // Print results
    for result in &summary.results {
      if result.success {
        println!("  ✓ {} - OK", result.target);
        if !result.warnings.is_empty() {
          for warning in &result.warnings {
            println!("    ⚠️  {}", warning);
          }
        }
      } else {
        println!("  ✗ {} - FAILED", result.target);
        if let Some(ref error) = result.error {
          println!("    Error: {}", error);
        }
      }
    }

    if !summary.all_passed() {
      println!("\n❌ Target validation FAILED");
      return Err(RailError::message(format!(
        "Validation failed for {} target(s): {}",
        summary.failed,
        summary.failed_targets().join(", ")
      )));
    }

    println!("\n✅ All target validations passed!");
  }

  Ok(())
}

/// Create a backup of a file
fn backup_file(path: &Path) -> RailResult<()> {
  let backup_path = path.with_extension("toml.bak");
  std::fs::copy(path, &backup_path)
    .map_err(|e| RailError::message(format!("Failed to backup {}: {}", path.display(), e)))?;
  Ok(())
}
