//! `cargo rail unify` - Workspace dependency unification
//!
//! This command eliminates the need for workspace-hack crates and cargo-hakari by:
//! 1. Analyzing all workspace dependencies
//! 2. Creating unified [workspace.dependencies] entries
//! 3. Converting member dependencies to use `workspace = true` inheritance
//!
//! Replaces cargo-hakari with native Cargo workspace dependency unification.

use crate::backup::{BackupManager, BackupMetadata};
use crate::cargo::{
  CargoTransform, IssueSeverity, UnifyConfig, UnifyReport, WorkspaceMetadata, WorkspaceUnifier, validate_targets,
};
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;

/// Get workspace metadata for unify operations
///
/// Checks rail.toml `[unify]` configuration to determine if --all-features should be used.
/// If use_all_features=true (default), returns cached --all-features metadata from context.
/// If false, returns default metadata from context.
///
/// This function now uses the lazy-loaded cache in WorkspaceContext, eliminating
/// the expensive metadata reload that previously happened on every unify command.
fn get_unify_metadata(ctx: &WorkspaceContext) -> RailResult<&WorkspaceMetadata> {
  // Check rail config for use_all_features setting
  let use_all_features = ctx
    .config
    .as_ref()
    .map(|cfg| cfg.unify.use_all_features)
    .unwrap_or(true); // Default to true if no config

  if use_all_features {
    // Use cached --all-features metadata (lazy-loaded on first access)
    ctx.metadata_all_features()
  } else {
    // Use default metadata from context (no clone needed!)
    Ok(ctx.cargo.metadata())
  }
}

/// Check if targets have changed since last init and notify user
///
/// Compares current target detection against config. If targets changed,
/// shows a helpful message but doesn't auto-update the config.
/// This keeps users in control while ensuring they're aware of changes.
fn check_target_updates(ctx: &WorkspaceContext) {
  // Only check if we have a config with targets
  let config_targets = if let Some(cfg) = ctx.config.as_ref() {
    if cfg.unify.validation.targets.is_empty() {
      return; // No targets configured, nothing to check
    }
    &cfg.unify.validation.targets
  } else {
    return; // No config, nothing to check
  };

  // Re-detect current targets
  let Ok(current_targets) = crate::targets::detect_targets(ctx.workspace_root()) else {
    return; // Detection failed, silently skip
  };

  // Compare: are they different?
  if current_targets != *config_targets {
    let added = current_targets.iter().filter(|t| !config_targets.contains(t)).count();
    let removed = config_targets.iter().filter(|t| !current_targets.contains(t)).count();

    if added > 0 || removed > 0 {
      println!("📍 Note: Target configuration has changed since last init:");
      if added > 0 {
        println!("  • {} new target(s) detected", added);
      }
      if removed > 0 {
        println!("  • {} target(s) removed", removed);
      }
      println!("  Run 'cargo rail init --force' to update validate_targets in rail.toml\n");
    }
  }
}

/// Run optional per-target validation if configured
fn maybe_run_validation(ctx: &WorkspaceContext) -> RailResult<()> {
  // Check if validation is enabled via config
  let (validate_targets_list, max_parallel_jobs) = if let Some(cfg) = ctx.config.as_ref() {
    if !cfg.unify.validation_enabled() {
      return Ok(()); // Validation not enabled
    }
    (cfg.unify.validation.targets.clone(), cfg.unify.effective_parallelism())
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
  pin_transitives_flag: bool,
  minimal_flag: bool,
) -> RailResult<()> {
  println!("🔍 Analyzing workspace dependencies...\n");

  // Check if targets have changed (lazy update notification)
  check_target_updates(ctx);

  // Get metadata (with --all-features if configured)
  let metadata = get_unify_metadata(ctx)?;

  // Build config from CLI args and rail.toml
  let mut config = if let Some(rail_config) = &ctx.config {
    UnifyConfig::from(rail_config.as_ref())
  } else {
    UnifyConfig::default()
  };

  // Apply excludes/includes from CLI
  for dep in exclude {
    config.exclude.insert(dep);
  }
  for dep in include {
    config.include.insert(dep);
  }

  // Run analysis
  let unifier = WorkspaceUnifier::with_config(metadata, config);
  let plan = unifier.analyze()?;

  // Display summary
  println!("{}", plan.summary());

  // Run optional validation if configured
  maybe_run_validation(ctx)?;

  // Final message
  let blocking_issues = plan.issues.iter().filter(|i| i.severity == IssueSeverity::Hard).count();
  let warning_issues = plan
    .issues
    .iter()
    .filter(|i| matches!(i.severity, IssueSeverity::Soft))
    .count();

  if blocking_issues > 0 {
    println!(
      "⚠️  Note: {} BLOCKING issues detected that will prevent 'cargo rail unify'.",
      blocking_issues
    );
    println!("Resolve these issues before attempting to apply changes.\n");
  } else if warning_issues > 0 {
    println!("⚠️  Note: {} non-blocking issues detected.", warning_issues);
    println!("'cargo rail unify' will proceed with warnings for these issues.\n");
  }

  // Show transitive fragmentation impact comparison
  if !plan.transitive_fragmentations.is_empty() {
    let consolidate_enabled = pin_transitives_flag
      || ctx
        .config
        .as_ref()
        .map(|c| c.unify.transitives.consolidate_features)
        .unwrap_or(false);

    display_transitive_impact(&plan, consolidate_enabled);
  }

  // Show minimize_features status (configuration preview)
  let should_minimize = minimal_flag || ctx.config.as_ref().map(|c| c.unify.minimize_features).unwrap_or(false);

  if should_minimize {
    println!("\n🎯 Feature minimization enabled:");
    println!("   When you run 'cargo rail unify apply', features will be minimized");
    println!("   to only those required for compilation.\n");
  } else {
    println!("\n💡 Feature minimization:");
    println!("   Currently disabled. To enable:");
    println!("     • Run: cargo rail unify apply --minimal");
    println!("     • Or set in rail.toml: minimize_features = true\n");
  }

  println!(
    "✅ Analysis complete{}",
    if plan.workspace_deps.is_empty() && plan.issues.is_empty() {
      ". No unification opportunities found."
    } else if !plan.workspace_deps.is_empty() {
      ". Run 'cargo rail unify' to apply changes."
    } else {
      "."
    }
  );

  Ok(())
}

/// Apply dependency unification (modify files)
pub fn run_unify_apply(
  ctx: &WorkspaceContext,
  exclude: Vec<String>,
  include: Vec<String>,
  backup: bool,
  pin_transitives_flag: bool,
  minimal_flag: bool,
) -> RailResult<()> {
  // Check if targets have changed (lazy update notification)
  check_target_updates(ctx);
  println!("🔧 Applying workspace dependency unification...\n");

  // Get metadata (with --all-features if configured)
  let metadata = get_unify_metadata(ctx)?;

  // Build config
  let mut config = if let Some(rail_config) = &ctx.config {
    UnifyConfig::from(rail_config.as_ref())
  } else {
    UnifyConfig::default()
  };

  for dep in exclude {
    config.exclude.insert(dep);
  }
  for dep in include {
    config.include.insert(dep);
  }

  // Run analysis first
  let unifier = WorkspaceUnifier::with_config(metadata, config.clone());
  let plan = unifier.analyze()?;

  // Check for issues
  let (hard_issues, soft_issues): (Vec<_>, Vec<_>) =
    plan.issues.iter().partition(|i| i.severity == IssueSeverity::Hard);

  if !hard_issues.is_empty() {
    println!("⚠️  Cannot apply: unification has BLOCKING issues:");
    for issue in hard_issues {
      println!("  {}", issue.format_message());
    }
    return Err(RailError::message("Fix blocking issues before applying"));
  }

  if !soft_issues.is_empty() {
    println!("⚠️  Proceeding with warnings (these issues will be handled via workspace.dependencies):");
    for issue in soft_issues {
      println!("  {}", issue.format_message());
    }
    println!();
  }

  // Determine if we should consolidate transitive features
  let should_consolidate_transitives = pin_transitives_flag
    || ctx
      .config
      .as_ref()
      .map(|c| c.unify.transitives.consolidate_features)
      .unwrap_or(false);

  // Handle transitive fragmentations if consolidation is enabled
  let mut transitive_deps_to_add = Vec::new();
  let mut transitive_feature_hosts = Vec::new();

  if should_consolidate_transitives && !plan.transitive_fragmentations.is_empty() {
    println!(
      "📌 Consolidating {} transitive-only crate(s) with fragmented features...\n",
      plan.transitive_fragmentations.len()
    );

    // Get transitive_feature_host from config and resolve to actual crate names
    let host_crates = ctx
      .config
      .as_ref()
      .map(|c| c.unify.transitives.host_selection.resolve(ctx.cargo.metadata()))
      .unwrap_or_else(|| {
        // Fallback to auto-selection if no config
        use crate::config::TransitiveFeatureHost;
        TransitiveFeatureHost::Auto.resolve(ctx.cargo.metadata())
      });

    transitive_feature_hosts = host_crates;

    // Convert transitive fragmentations to UnifiedDep format
    for frag in &plan.transitive_fragmentations {
      println!(
        "  📦 {} v{} ({} feature sets)",
        frag.name,
        frag.version,
        frag.feature_sets.len()
      );

      // Parse version - try exact version first, then bare version
      let version_req = semver::VersionReq::parse(&format!("={}", frag.version))
        .or_else(|_| semver::VersionReq::parse(&frag.version))
        .map_err(|e| {
          RailError::message(format!(
            "Failed to parse version '{}' for transitive dependency '{}': {}",
            frag.version, frag.name, e
          ))
        })?;

      transitive_deps_to_add.push(crate::cargo::UnifiedDep {
        name: frag.name.clone(),
        version_req,
        features: frag.unified_features.clone(),
        feature_provenance: std::collections::HashMap::new(), // Transitive deps don't have member-level provenance
        default_features: true,
        used_by: vec!["<transitive>".to_string()],
        dep_kinds: vec![cargo_metadata::DependencyKind::Development].into_iter().collect(),
        fragmentation_count: frag.feature_sets.len(),
        path: None,
        target: None, // Transitive deps are not target-specific
        comments: Vec::new(),
        is_proc_macro: false, // Will be detected when processing direct deps
      });
    }

    println!();
  }

  // Run optional validation if configured (before applying changes)
  maybe_run_validation(ctx)?;

  // ============================================================================
  // BACKUP: Determine if we should create a backup before modifications
  // ============================================================================

  let backup_manager = BackupManager::new(ctx.workspace_root());

  // First run detection: no previous backups exist
  let is_first_run = !backup_manager.has_backups();

  let should_backup = {
    // Check if backup is enabled via config
    let backup_enabled_in_config = ctx.config.as_ref().map(|c| c.unify.backup.enabled).unwrap_or(true); // Default to true if no config

    // Backup if: first run OR --backup flag OR config enabled
    is_first_run || backup || backup_enabled_in_config
  };

  if should_backup {
    // Collect all files that will be modified
    let mut files_to_backup = Vec::new();

    // 1. Workspace root Cargo.toml
    let workspace_toml = ctx.workspace_root().join("Cargo.toml");
    files_to_backup.push(workspace_toml.strip_prefix(ctx.workspace_root()).unwrap().to_path_buf());

    // 2. All member Cargo.toml files that will be modified
    for (member_name, edits) in &plan.member_edits {
      if !edits.is_empty()
        && let Some(member_pkg) = ctx.cargo.get_package(member_name)
      {
        let member_toml = member_pkg.manifest_path.as_std_path();
        if let Ok(relative) = member_toml.strip_prefix(ctx.workspace_root()) {
          files_to_backup.push(relative.to_path_buf());
        }
      }
    }

    // 3. Transitive feature host crates (if applicable)
    if !transitive_deps_to_add.is_empty() && !transitive_feature_hosts.is_empty() {
      for host_name in &transitive_feature_hosts {
        if let Some(host_pkg) = ctx.cargo.get_package(host_name) {
          let host_toml = host_pkg.manifest_path.as_std_path();
          if let Ok(relative) = host_toml.strip_prefix(ctx.workspace_root())
            && !files_to_backup.contains(&relative.to_path_buf())
          {
            files_to_backup.push(relative.to_path_buf());
          }
        }
      }
    }

    // Create backup with metadata
    let mut backup_metadata = BackupMetadata::new("cargo rail unify apply");

    // Add config snapshot if available
    if let Some(ref rail_config) = ctx.config {
      backup_metadata = backup_metadata.with_config(rail_config.as_ref().clone());
    }

    // Add description
    let description = if is_first_run {
      format!(
        "Initial unify backup: {} dependencies unified across {} members",
        plan.stats.unified_count,
        plan.member_edits.len()
      )
    } else {
      format!(
        "Unify backup: {} dependencies unified across {} members",
        plan.stats.unified_count,
        plan.member_edits.len()
      )
    };
    backup_metadata = backup_metadata.with_description(description);

    println!("💾 Creating backup of {} file(s)...", files_to_backup.len());
    let backup_id = backup_manager.create_backup(&files_to_backup, backup_metadata)?;
    println!("  ✓ Backup created: {}", backup_id);
    println!("  💡 To restore: cargo rail unify undo\n");

    // Clean up old backups if configured
    if let Some(ref rail_config) = ctx.config {
      let keep_count = rail_config.unify.backup.keep_count;
      if let Ok(deleted) = backup_manager.cleanup_old_backups(keep_count)
        && deleted > 0
      {
        println!(
          "  🧹 Cleaned up {} old backup(s) (keeping {} most recent)\n",
          deleted, keep_count
        );
      }
    }
  }

  // Create transformer (use the same metadata we used for analysis)
  let transformer = CargoTransform::new(ctx.cargo.metadata().clone());

  // 1. Write [workspace.dependencies]
  let workspace_toml = ctx.workspace_root().join("Cargo.toml");
  println!("📝 Writing [workspace.dependencies] to {}", workspace_toml.display());

  // Combine regular deps and transitive deps
  let mut all_workspace_deps = plan.workspace_deps.clone();
  all_workspace_deps.extend(transitive_deps_to_add.clone());

  transformer.write_workspace_dependencies(&workspace_toml, &all_workspace_deps, config.add_conflict_comments)?;
  println!("  ✓ Wrote {} unified dependencies", all_workspace_deps.len());
  if !transitive_deps_to_add.is_empty() {
    println!(
      "    (including {} pinned transitive deps)",
      transitive_deps_to_add.len()
    );
  }

  // 1.5. Minimize features if enabled (via config or CLI flag)
  // This is the "killer feature" - automated feature pruning
  let should_minimize = minimal_flag || ctx.config.as_ref().map(|c| c.unify.minimize_features).unwrap_or(false);

  let _final_workspace_deps = if should_minimize {
    use crate::cargo::unify::minimize_workspace_features;

    let (minimized_deps, min_report) = minimize_workspace_features(ctx, &all_workspace_deps, &workspace_toml)?;

    // Re-write workspace.dependencies with minimal features
    println!("\n📝 Updating [workspace.dependencies] with minimal features...");
    transformer.write_workspace_dependencies(&workspace_toml, &minimized_deps, config.add_conflict_comments)?;
    println!("  ✓ Updated with minimal feature sets");

    // Print report
    println!("{}", min_report.summary());

    minimized_deps
  } else {
    // No minimization - use original deps
    all_workspace_deps
  };

  // 2. Convert member dependencies to workspace inheritance
  println!("\n📝 Converting member dependencies to workspace inheritance...");

  let mut all_skipped_deps: Vec<(String, String, cargo_metadata::DependencyKind)> = Vec::new();

  for (member_name, edits) in &plan.member_edits {
    if edits.is_empty() {
      continue;
    }

    let member_pkg = ctx
      .cargo
      .get_package(member_name)
      .ok_or_else(|| RailError::message(format!("Package {} not found", member_name)))?;

    let member_toml = member_pkg.manifest_path.as_std_path();

    let mut successful_edits = 0;
    let mut skipped_edits = Vec::new();

    for edit in edits {
      match edit {
        crate::cargo::unify::MemberEdit::UseWorkspace {
          dep_name,
          kind,
          add_features,
        } => {
          match transformer.convert_to_workspace_inheritance(member_toml, dep_name, kind, add_features.clone()) {
            Ok(_) => successful_edits += 1,
            Err(_) => {
              // This happens when we detect a dep via --all-features metadata resolution,
              // but it doesn't exist in this member's Cargo.toml (it's a transitive dep
              // pulled in by cargo's feature resolution, not directly declared).
              skipped_edits.push((dep_name.clone(), *kind));
              all_skipped_deps.push((member_name.clone(), dep_name.clone(), *kind));
            }
          }
        }
      }
    }

    if successful_edits > 0 {
      println!("  ✓ {} ({} dependencies)", member_name, successful_edits);
    }
    if !skipped_edits.is_empty() {
      println!(
        "    ⚠️  {} transitive deps detected via --all-features (see note below)",
        skipped_edits.len()
      );
    }
  }

  // Show explanation for skipped transitive deps
  if !all_skipped_deps.is_empty() {
    println!("\nℹ️  About transitive dependencies detected via --all-features:");
    println!(
      "   cargo-rail found {} transitive dependencies that cargo resolves automatically",
      all_skipped_deps.len()
    );
    println!("   but aren't directly declared in member Cargo.toml files.");
    println!();
    println!("   Why this happens:");
    println!("   - Using --all-features captures the complete dependency graph");
    println!("   - Cargo automatically pulls in transitive deps based on enabled features");
    println!("   - These deps don't exist in your Cargo.toml, so we can't convert them");
    println!();
    println!("   Options:");
    println!("   A) Do nothing - cargo will continue to resolve these automatically (RECOMMENDED)");
    println!("   B) Explicitly add them to [workspace.dependencies] + [dev-dependencies] in affected members");
    println!("      (useful if you want explicit control over transitive dep versions)");
    println!();

    // Group by dep name for clarity
    let mut by_dep: std::collections::HashMap<String, Vec<(String, cargo_metadata::DependencyKind)>> =
      std::collections::HashMap::new();
    for (member, dep, kind) in &all_skipped_deps {
      by_dep.entry(dep.clone()).or_default().push((member.clone(), *kind));
    }

    println!("   Affected dependencies:");
    for (dep_name, members) in by_dep.iter().take(5) {
      let member_names: Vec<_> = members.iter().map(|(m, _)| m.as_str()).collect();
      println!("   - {}: {}", dep_name, member_names.join(", "));
    }
    if by_dep.len() > 5 {
      println!("   ... and {} more", by_dep.len() - 5);
    }
    println!();
  }

  // 3. Add dev-dependencies for consolidated transitives to host crates
  if !transitive_deps_to_add.is_empty() && !transitive_feature_hosts.is_empty() {
    println!("\n📝 Adding dev-dependencies for consolidated transitives to host crates...");

    for host_name in &transitive_feature_hosts {
      let host_pkg = ctx.cargo.get_package(host_name).ok_or_else(|| {
        RailError::message(format!(
          "Transitive feature host '{}' not found in workspace. Check unify.transitive_feature_host in rail.toml",
          host_name
        ))
      })?;

      let host_toml = host_pkg.manifest_path.as_std_path();

      // Read and parse the host's Cargo.toml
      let content = std::fs::read_to_string(host_toml)?;
      let mut doc: toml_edit::DocumentMut = content
        .parse()
        .map_err(|e| RailError::message(format!("Failed to parse {}: {}", host_toml.display(), e)))?;

      // Ensure [dev-dependencies] section exists
      if !doc.contains_key("dev-dependencies") {
        doc["dev-dependencies"] = toml_edit::table();
      }

      let dev_deps = doc["dev-dependencies"]
        .as_table_mut()
        .ok_or_else(|| RailError::message("[dev-dependencies] is not a table"))?;

      // Add each transitive dep as { workspace = true }
      for transitive_dep in &transitive_deps_to_add {
        let mut dep_table = toml_edit::InlineTable::new();
        dep_table.insert("workspace", toml_edit::Value::Boolean(toml_edit::Formatted::new(true)));
        dev_deps.insert(
          &transitive_dep.name,
          toml_edit::Item::Value(toml_edit::Value::InlineTable(dep_table)),
        );
      }

      // Write back
      std::fs::write(host_toml, doc.to_string())?;

      println!(
        "  ✓ {} ({} transitive dev-dependencies)",
        host_name,
        transitive_deps_to_add.len()
      );
    }
  }

  println!("\n✅ Unification complete!");
  println!("\nStats:");
  println!("  - {} dependencies unified", plan.stats.unified_count);
  println!("  - {} workspace members updated", plan.member_edits.len());
  println!(
    "  - {} redundant compilations eliminated",
    plan.stats.compilations_saved
  );

  // Note: Backup handling is done earlier in the function
  // No need to print additional messages here

  // Generate report if configured
  if config.generate_report {
    let report_path = ctx.workspace_root().join("unify-report.md");
    UnifyReport::write_to_file(&plan, metadata, &report_path)?;
    println!("\n📄 Unification report generated at {}", report_path.display());
  }

  println!("\nNext steps:");
  println!("  1. Run 'cargo check' to verify everything compiles");
  println!("  2. Test your workspace thoroughly");
  println!("  3. Commit the changes");

  Ok(())
}

/// Display transitive feature consolidation impact comparison
///
/// Shows the difference between ignoring transitive fragmentations (Option A)
/// and consolidating them (Option B), with a reference to the decision guide.
fn display_transitive_impact(plan: &crate::cargo::unify::UnificationPlan, consolidate_enabled: bool) {
  println!("\n📊 Transitive Feature Consolidation Impact:\n");

  let total_fragmentations = plan.transitive_fragmentations.len();

  // Calculate total duplicate compilations
  let total_duplicates: usize = plan
    .transitive_fragmentations
    .iter()
    .map(|f| f.feature_sets.len().saturating_sub(1))
    .sum();

  // Calculate how many extra features would be enabled
  let extra_features: usize = plan
    .transitive_fragmentations
    .iter()
    .map(|f| {
      // Union of all features - features in largest set = extras
      let all_features: std::collections::HashSet<_> = f.feature_sets.iter().flat_map(|set| set.iter()).collect();

      let largest_set_size = f.feature_sets.iter().map(|s| s.len()).max().unwrap_or(0);

      all_features.len().saturating_sub(largest_set_size)
    })
    .sum();

  println!(
    "  Detected: {} transitive-only crate(s) with fragmented features",
    total_fragmentations
  );
  println!("  Duplicate compilations: {}\n", total_duplicates);

  println!("  Option A (Ignore):");
  println!("    • No changes to Cargo.toml files");
  println!("    • {} duplicate compilations remain", total_duplicates);
  println!("    • Cargo manages transitives automatically");
  println!("    • Simpler configuration\n");

  println!("  Option B (Consolidate):");
  println!(
    "    • Add {} transitive deps to [workspace.dependencies]",
    total_fragmentations
  );
  println!("    • Compilations saved: {}", total_duplicates);
  println!("    • Extra features enabled: ~{}", extra_features);
  println!("    • Explicit version control");
  println!("    • Faster builds (especially incremental)\n");

  if consolidate_enabled {
    println!("  ✅ Currently using: Option B (Consolidate)");
    println!("     Transitive deps will be added when you run 'cargo rail unify'\n");
  } else {
    println!("  ✅ Currently using: Option A (Ignore)");
    println!("     To enable consolidation:");
    println!("       • Run: cargo rail unify --consolidate-transitives");
    println!("       • Or add to .config/rail.toml:");
    println!("         [unify.transitives]");
    println!("         consolidate_features = true\n");
  }

  println!("  💡 Learn more: docs/src/guides/transitive-consolidation.md");
  println!(
    "     Or online: https://github.com/loadingalias/cargo-rail/blob/main/docs/src/guides/transitive-consolidation.md\n"
  );
}

/// Restore a unify backup (undo functionality)
///
/// This function handles the `cargo rail unify undo` command.
///
/// # Arguments
///
/// * `ctx` - Workspace context
/// * `list` - If true, list available backups instead of restoring
/// * `backup_id` - Optional specific backup ID to restore (if None, restores most recent)
pub fn run_unify_undo(ctx: &WorkspaceContext, list: bool, backup_id: Option<String>) -> RailResult<()> {
  let backup_manager = BackupManager::new(ctx.workspace_root());

  // Handle --list flag
  if list {
    println!("📋 Available backups:\n");

    let backups = backup_manager.list_backups()?;

    if backups.is_empty() {
      println!("  No backups found.");
      println!("\n  💡 Backups are created automatically when running 'cargo rail unify apply'");
      println!("     or explicitly with the --backup flag.");
      return Ok(());
    }

    for (idx, backup) in backups.iter().enumerate() {
      let is_latest = idx == 0;
      let marker = if is_latest { " (latest)" } else { "" };

      println!("{}. {}{}", idx + 1, backup.id, marker);
      println!("   Timestamp: {}", backup.timestamp_display());
      println!("   Command:   {}", backup.metadata.command);
      println!("   Files:     {} modified", backup.file_count());

      if let Some(desc) = &backup.metadata.description {
        println!("   Description: {}", desc);
      }

      println!();
    }

    println!("  To restore a backup:");
    println!("    cargo rail unify undo              # Restore latest");
    println!("    cargo rail unify undo --backup-id <id>  # Restore specific");

    return Ok(());
  }

  // Determine which backup to restore
  let backup_to_restore = if let Some(id) = backup_id {
    // User specified a backup ID
    id
  } else {
    // Use the latest backup
    let latest = backup_manager.get_latest_backup()?;

    match latest {
      Some(backup) => {
        println!("🔄 Restoring latest backup: {}\n", backup.id);
        backup.id
      }
      None => {
        println!("⚠️  No backups found.");
        println!("\n  💡 Backups are created automatically when running 'cargo rail unify apply'");
        println!("     or explicitly with the --backup flag.");
        return Ok(());
      }
    }
  };

  // Restore the backup
  backup_manager.restore_backup(&backup_to_restore)?;

  println!("\n  💡 Next steps:");
  println!("     1. Run 'cargo check' to verify everything compiles");
  println!("     2. Review the restored files");

  Ok(())
}

/// Restore a unify backup (undo functionality) - standalone version
///
/// This is a standalone version that doesn't require WorkspaceContext,
/// allowing it to work even when Cargo.toml files are corrupted.
/// This is critical for the undo functionality - if files are corrupted,
/// we can't load metadata, but we still need to be able to restore from backup.
///
/// # Arguments
///
/// * `workspace_root` - Path to workspace root
/// * `list` - If true, list available backups instead of restoring
/// * `backup_id` - Optional specific backup ID to restore (if None, restores most recent)
pub fn run_unify_undo_standalone(
  workspace_root: impl Into<std::path::PathBuf>,
  list: bool,
  backup_id: Option<String>,
) -> RailResult<()> {
  let workspace_root = workspace_root.into();
  let backup_manager = BackupManager::new(&workspace_root);

  // Handle --list flag
  if list {
    println!("📋 Available backups:\n");

    let backups = backup_manager.list_backups()?;

    if backups.is_empty() {
      println!("  No backups found.");
      println!("\n  💡 Backups are created automatically when running 'cargo rail unify'");
      println!("     or explicitly with the --backup flag.");
      return Ok(());
    }

    for (idx, backup) in backups.iter().enumerate() {
      let is_latest = idx == 0;
      let marker = if is_latest { " (latest)" } else { "" };

      println!("{}. {}{}", idx + 1, backup.id, marker);
      println!("   Timestamp: {}", backup.timestamp_display());
      println!("   Command:   {}", backup.metadata.command);
      println!("   Files:     {} modified", backup.file_count());

      if let Some(desc) = &backup.metadata.description {
        println!("   Description: {}", desc);
      }

      println!();
    }

    println!("  To restore a backup:");
    println!("    cargo rail unify undo              # Restore latest");
    println!("    cargo rail unify undo --backup-id <id>  # Restore specific");

    return Ok(());
  }

  // Determine which backup to restore
  let backup_to_restore = if let Some(id) = backup_id {
    // User specified a backup ID
    id
  } else {
    // Use the latest backup
    let latest = backup_manager.get_latest_backup()?;

    match latest {
      Some(backup) => {
        println!("🔄 Restoring latest backup: {}\n", backup.id);
        backup.id
      }
      None => {
        println!("⚠️  No backups found.");
        println!("\n  💡 Backups are created automatically when running 'cargo rail unify'");
        println!("     or explicitly with the --backup flag.");
        return Ok(());
      }
    }
  };

  // Restore the backup
  backup_manager.restore_backup(&backup_to_restore)?;

  println!("\n  💡 Next steps:");
  println!("     1. Run 'cargo check' to verify everything compiles");
  println!("     2. Review the restored files");

  Ok(())
}
