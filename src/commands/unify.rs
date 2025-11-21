//! `cargo rail unify` - Workspace dependency unification
//!
//! This command eliminates the need for workspace-hack crates and cargo-hakari by:
//! 1. Analyzing all workspace dependencies
//! 2. Creating unified [workspace.dependencies] entries
//! 3. Converting member dependencies to use `workspace = true` inheritance
//!
//! Replaces cargo-hakari with native Cargo workspace dependency unification.

use crate::cargo::{
  CargoTransform, IssueSeverity, UnifyConfig, UnifyReport, UnifyStrategy, WorkspaceMetadata, WorkspaceUnifier,
  validate_targets,
};
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
  pin_transitives_flag: bool,
) -> RailResult<()> {
  println!("🔍 Analyzing workspace dependencies...\n");

  // Get metadata (with --all-features if configured)
  let metadata = get_unify_metadata(ctx)?;

  // Build config from CLI args and rail.toml
  let mut config = if let Some(rail_config) = &ctx.config {
    UnifyConfig {
      strategy: UnifyStrategy::All,
      allow_renamed: rail_config.unify.allow_renamed,
      exclude: rail_config.unify.exclude.iter().cloned().collect(),
      include: rail_config.unify.include.iter().cloned().collect(),
      auto_resolve_version_conflicts: rail_config.unify.auto_resolve_version_conflicts,
      add_conflict_comments: rail_config.unify.add_conflict_comments,
      generate_report: rail_config.unify.generate_report,
    }
  } else {
    UnifyConfig {
      strategy: UnifyStrategy::All,
      ..Default::default()
    }
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
      "⚠️  Note: {} BLOCKING issues detected that will prevent 'cargo rail unify apply'.",
      blocking_issues
    );
    println!("Resolve these issues before attempting to apply changes.\n");
  } else if warning_issues > 0 {
    println!("⚠️  Note: {} non-blocking issues detected.", warning_issues);
    println!("'cargo rail unify apply' will proceed with warnings for these issues.\n");
  }

  // Show transitive fragmentation info if pin_transitives is enabled
  if !plan.transitive_fragmentations.is_empty()
    && (pin_transitives_flag || ctx.config.as_ref().map(|c| c.unify.pin_transitives).unwrap_or(false))
  {
    println!("📌 Transitive pinning is ENABLED - these crates will be pinned when you run 'apply'.\n");
  }

  println!(
    "✅ Analysis complete{}",
    if plan.workspace_deps.is_empty() && plan.issues.is_empty() {
      ". No unification opportunities found."
    } else if !plan.workspace_deps.is_empty() {
      ". Run 'cargo rail unify apply' to make changes."
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
) -> RailResult<()> {
  println!("🔧 Applying workspace dependency unification...\n");

  // Get metadata (with --all-features if configured)
  let metadata = get_unify_metadata(ctx)?;

  // Build config
  let mut config = if let Some(rail_config) = &ctx.config {
    UnifyConfig {
      strategy: UnifyStrategy::All,
      allow_renamed: rail_config.unify.allow_renamed,
      exclude: rail_config.unify.exclude.iter().cloned().collect(),
      include: rail_config.unify.include.iter().cloned().collect(),
      auto_resolve_version_conflicts: rail_config.unify.auto_resolve_version_conflicts,
      add_conflict_comments: rail_config.unify.add_conflict_comments,
      generate_report: rail_config.unify.generate_report,
    }
  } else {
    UnifyConfig {
      strategy: UnifyStrategy::All,
      ..Default::default()
    }
  };

  for dep in exclude {
    config.exclude.insert(dep);
  }
  for dep in include {
    config.include.insert(dep);
  }

  // Run analysis first
  let unifier = WorkspaceUnifier::with_config(&metadata, config.clone());
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

  // Determine if we should pin transitives
  let should_pin_transitives =
    pin_transitives_flag || ctx.config.as_ref().map(|c| c.unify.pin_transitives).unwrap_or(false);

  // Handle transitive fragmentations if pinning is enabled
  let mut transitive_deps_to_add = Vec::new();
  let mut transitive_pin_hosts = Vec::new();

  if should_pin_transitives && !plan.transitive_fragmentations.is_empty() {
    println!(
      "📌 Pinning {} transitive-only crate(s) with fragmented features...\n",
      plan.transitive_fragmentations.len()
    );

    // Get pin_hosts from config or use default
    let pin_hosts = ctx
      .config
      .as_ref()
      .and_then(|c| {
        if c.unify.pin_hosts.is_empty() {
          None
        } else {
          Some(c.unify.pin_hosts.clone())
        }
      })
      .unwrap_or_else(|| {
        // Default: use the first workspace member or create a list
        vec![
          ctx
            .cargo
            .metadata()
            .list_crates()
            .first()
            .map(|p| p.name.to_string())
            .unwrap_or_else(|| "cargo-rail".to_string()),
        ]
      });

    transitive_pin_hosts = pin_hosts;

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
        default_features: true,
        used_by: vec!["<transitive>".to_string()],
        dep_kinds: vec![cargo_metadata::DependencyKind::Development].into_iter().collect(),
        fragmentation_count: frag.feature_sets.len(),
        path: None,
        comments: Vec::new(),
      });
    }

    println!();
  }

  // Run optional validation if configured (before applying changes)
  maybe_run_validation(ctx)?;

  // Create transformer (use the same metadata we used for analysis)
  let transformer = CargoTransform::new(metadata.clone());

  // 1. Write [workspace.dependencies]
  let workspace_toml = ctx.workspace_root().join("Cargo.toml");
  println!("📝 Writing [workspace.dependencies] to {}", workspace_toml.display());

  // Backup if requested
  if backup {
    backup_file(&workspace_toml)?;
  }

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

    if backup {
      backup_file(member_toml)?;
    }

    let mut successful_edits = 0;
    let mut skipped_edits = Vec::new();

    for edit in edits {
      match edit {
        crate::cargo::unify::MemberEdit::UseWorkspace { dep_name, kind } => {
          match transformer.convert_to_workspace_inheritance(member_toml, dep_name, kind) {
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

  // 3. Add dev-dependencies for pinned transitives to pin_hosts
  if !transitive_deps_to_add.is_empty() && !transitive_pin_hosts.is_empty() {
    println!("\n📝 Adding dev-dependencies for pinned transitives to pin_hosts...");

    for host_name in &transitive_pin_hosts {
      let host_pkg = ctx.cargo.get_package(host_name).ok_or_else(|| {
        RailError::message(format!(
          "Pin host '{}' not found in workspace. Check unify.pin_hosts in rail.toml",
          host_name
        ))
      })?;

      let host_toml = host_pkg.manifest_path.as_std_path();

      if backup {
        backup_file(host_toml)?;
      }

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

  if backup {
    println!("\n💾 Backups created with .bak extension");
  }

  // Generate report if configured
  if config.generate_report {
    let report_path = ctx.workspace_root().join("unify-report.md");
    UnifyReport::write_to_file(&plan, &metadata, &report_path)?;
    println!("\n📄 Unification report generated at {}", report_path.display());
  }

  println!("\nNext steps:");
  println!("  1. Run 'cargo check' to verify everything compiles");
  println!("  2. Test your workspace thoroughly");
  println!("  3. Commit the changes");

  Ok(())
}

/// Create a backup of a file
fn backup_file(path: &Path) -> RailResult<()> {
  let backup_path = path.with_extension("toml.bak");
  std::fs::copy(path, &backup_path)
    .map_err(|e| RailError::message(format!("Failed to backup {}: {}", path.display(), e)))?;
  Ok(())
}
