//! `cargo rail unify` - Workspace dependency unification
//!
//! Implements resolution-based unification:
//! 1. Multi-target metadata for versions
//! 2. Manifest parsing for minimal features
//! 3. Intersection-based feature unification

use crate::cargo::{ManifestWriter, UnifyAnalyzer, UnifyReport};
use crate::commands::common::OutputFormat;
use crate::error::{RailError, RailResult};
use crate::progress;
use crate::workspace::WorkspaceContext;

/// Analyze workspace dependencies (check mode)
pub fn run_unify_analyze(
  ctx: &WorkspaceContext,
  show_diff: bool,
  explain: bool,
  format: OutputFormat,
) -> RailResult<()> {
  let json = format.is_json();

  // JSON mode enables structured error output and suppresses progress
  if json {
    crate::output::set_json_mode(true);
  }

  // Create analyzer (config comes from rail.toml via ctx)
  let analyzer = UnifyAnalyzer::new(ctx)?;

  // Run analysis
  let plan = analyzer.analyze()?;

  // JSON output mode
  if json {
    let output = serde_json::json!({
      "command": "unify",
      "check": true,
      "workspace_deps": plan.workspace_deps.iter().map(|d| serde_json::json!({
        "name": d.name,
        "version": d.version_req,
        "features": d.features,
      })).collect::<Vec<_>>(),
      "summary": {
        "workspace_deps_count": plan.workspace_deps.len(),
        "member_edits_count": plan.member_edits.values().map(|v| v.len()).sum::<usize>(),
        "members_affected": plan.member_edits.len(),
        "transitive_pins_count": plan.transitive_pins.len(),
        "duplicates_unified": plan.duplicates_cleaned.len(),
        "dead_features_pruned": plan.pruned_features.len(),
        "optional_features_detected": plan.optional_features.len(),
        "version_mismatches": plan.version_mismatches.len(),
        "unused_deps": plan.unused_deps.len(),
      },
      "has_blocking_issues": plan.has_blocking_issues(),
      "issues": plan.issues.iter().map(|i| serde_json::json!({
        "dep_name": i.dep_name,
        "severity": format!("{:?}", i.severity),
        "message": i.message,
      })).collect::<Vec<_>>(),
      "duplicates_cleaned": plan.duplicates_cleaned.iter().map(|d| serde_json::json!({
        "dep_name": d.dep_name,
        "selected_version": d.selected_version,
        "previous_versions": d.versions_found,
      })).collect::<Vec<_>>(),
      "pruned_features": plan.pruned_features.iter().map(|f| serde_json::json!({
        "crate_name": f.crate_name,
        "feature_name": f.feature_name,
      })).collect::<Vec<_>>(),
      "optional_features": plan.optional_features.iter().map(|f| serde_json::json!({
        "crate_name": f.crate_name,
        "feature_name": f.feature_name,
        "enables": f.enables,
      })).collect::<Vec<_>>(),
    });
    println!(
      "{}",
      serde_json::to_string_pretty(&output).map_err(|e| RailError::message(format!("JSON error: {}", e)))?
    );
    return Ok(());
  }

  // Display summary
  println!("{}", plan.summary());

  // Show explain output if requested
  if explain {
    display_explain(&plan);
  }

  // Show diff if requested
  if show_diff && (!plan.member_edits.is_empty() || !plan.workspace_deps.is_empty()) {
    println!("\nplanned changes:\n");

    // Show workspace deps that will be added
    if !plan.workspace_deps.is_empty() {
      println!("[workspace.dependencies]:");
      for dep in &plan.workspace_deps {
        println!("  + {} = \"{}\"", dep.name, dep.version_req);
        if !dep.features.is_empty() {
          let mut features = dep.features.clone();
          features.sort();
          println!(
            "      features = [{}]",
            features
              .iter()
              .map(|f| format!("\"{}\"", f))
              .collect::<Vec<_>>()
              .join(", ")
          );
        }
      }
      println!();
    }

    // Show member edits
    let mut members: Vec<_> = plan.member_edits.keys().collect();
    members.sort();
    for member in members {
      let edits = &plan.member_edits[member];
      if edits.is_empty() {
        continue;
      }
      let path = plan
        .member_paths
        .get(member)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| member.clone());
      println!("{}:", path);
      for edit in edits {
        match edit {
          crate::cargo::MemberEdit::UseWorkspace {
            dep_name,
            dep_kind,
            target,
            local_features,
            is_optional,
          } => {
            let section = match (dep_kind, target) {
              (crate::cargo::DepKind::Dev, None) => "[dev-dependencies]".to_string(),
              (crate::cargo::DepKind::Build, None) => "[build-dependencies]".to_string(),
              (_, None) => "[dependencies]".to_string(),
              (crate::cargo::DepKind::Dev, Some(t)) => format!("[target.'{}'.dev-dependencies]", t),
              (crate::cargo::DepKind::Build, Some(t)) => format!("[target.'{}'.build-dependencies]", t),
              (_, Some(t)) => format!("[target.'{}'.dependencies]", t),
            };
            let mut line = format!("  {} {} -> workspace = true", section, dep_name);
            if !local_features.is_empty() {
              let mut features = local_features.clone();
              features.sort();
              line.push_str(&format!(
                ", features = [{}]",
                features
                  .iter()
                  .map(|f| format!("\"{}\"", f))
                  .collect::<Vec<_>>()
                  .join(", ")
              ));
            }
            if *is_optional {
              line.push_str(", optional = true");
            }
            println!("{}", line);
          }
          crate::cargo::MemberEdit::RemoveDep {
            dep_name,
            dep_kind,
            target,
          } => {
            let section = match (dep_kind, target) {
              (crate::cargo::DepKind::Dev, None) => "[dev-dependencies]".to_string(),
              (crate::cargo::DepKind::Build, None) => "[build-dependencies]".to_string(),
              (_, None) => "[dependencies]".to_string(),
              (crate::cargo::DepKind::Dev, Some(t)) => format!("[target.'{}'.dev-dependencies]", t),
              (crate::cargo::DepKind::Build, Some(t)) => format!("[target.'{}'.build-dependencies]", t),
              (_, Some(t)) => format!("[target.'{}'.dependencies]", t),
            };
            println!("  {} {} -> REMOVE (unused)", section, dep_name);
          }
          crate::cargo::MemberEdit::RemoveFeature { feature_name } => {
            println!("  [features] {} -> REMOVE (dead/empty)", feature_name);
          }
          crate::cargo::MemberEdit::AddFeatures {
            dep_name,
            dep_kind,
            target,
            features_to_add,
          } => {
            let section = match (dep_kind, target) {
              (crate::cargo::DepKind::Dev, None) => "[dev-dependencies]".to_string(),
              (crate::cargo::DepKind::Build, None) => "[build-dependencies]".to_string(),
              (_, None) => "[dependencies]".to_string(),
              (crate::cargo::DepKind::Dev, Some(t)) => format!("[target.'{}'.dev-dependencies]", t),
              (crate::cargo::DepKind::Build, Some(t)) => format!("[target.'{}'.build-dependencies]", t),
              (_, Some(t)) => format!("[target.'{}'.dependencies]", t),
            };
            let mut sorted_features = features_to_add.clone();
            sorted_features.sort();
            println!(
              "  {} {} -> ADD features [{}]",
              section,
              dep_name,
              sorted_features
                .iter()
                .map(|f| format!("\"{}\"", f))
                .collect::<Vec<_>>()
                .join(", ")
            );
          }
        }
      }
      println!();
    }
  }

  // Show validation results if any failed
  let failed_validations: Vec<_> = plan.validation_results.iter().filter(|v| !v.success).collect();

  if !failed_validations.is_empty() {
    eprintln!("\nvalidation errors:");
    for val in failed_validations {
      eprintln!(
        "  {}: {}",
        val.target,
        val.error.as_ref().unwrap_or(&"unknown".to_string())
      );
    }
  }

  // Final message and exit code
  if plan.has_blocking_issues() {
    eprintln!("\nerror: blocking issues prevent unification");
    return Err(RailError::message("blocking issues prevent unification"));
  } else if !plan.workspace_deps.is_empty() || !plan.member_edits.is_empty() {
    let total_edits: usize = plan.member_edits.values().map(|v| v.len()).sum();
    if !plan.workspace_deps.is_empty() {
      println!(
        "\nready: {} dependencies, {} member edits",
        plan.workspace_deps.len(),
        total_edits
      );
    } else {
      println!("\nready: {} members, {} edits", plan.member_edits.len(), total_edits);
    }
    println!("Changes detected. Run without --check to apply.");
    return Err(RailError::CheckHasPendingChanges);
  } else {
    println!("\nno unification opportunities found");
  }

  Ok(())
}

/// Execute dependency unification
pub fn run_unify_apply(
  ctx: &WorkspaceContext,
  backup: bool,
  no_report: bool,
  report_path: Option<std::path::PathBuf>,
) -> RailResult<()> {
  use crate::backup::{BackupManager, BackupMetadata};
  use std::path::PathBuf;

  // Create analyzer (config comes from rail.toml via ctx)
  let analyzer = UnifyAnalyzer::new(ctx)?;

  let plan = analyzer.analyze()?;

  // Check for blockers
  if plan.has_blocking_issues() {
    eprintln!("error: blocking issues prevent unification:");
    for issue in &plan.issues {
      if issue.severity == crate::cargo::IssueSeverity::Error {
        eprintln!("  {}: {}", issue.dep_name, issue.message);
      }
    }
    return Err(crate::error::RailError::message("blocking issues prevent unification"));
  }

  if plan.workspace_deps.is_empty() && plan.member_edits.is_empty() {
    println!("nothing to unify");
    return Ok(());
  }

  // Create backup if requested or first run
  let backup_manager = BackupManager::new(ctx.workspace_root());
  let is_first_run = !backup_manager.has_backups();
  let should_backup = backup || is_first_run;
  let mut created_backup_id: Option<String> = None;

  if should_backup {
    if is_first_run && !backup {
      progress!("creating backup (first run)...");
    } else {
      progress!("creating backup...");
    }

    let mut files_to_backup = Vec::new();
    if !plan.workspace_deps.is_empty() {
      files_to_backup.push(PathBuf::from("Cargo.toml"));
    }

    for member_name in plan.member_edits.keys() {
      if let Some(manifest_path) = plan.member_paths.get(member_name)
        && let Ok(rel_path) = manifest_path.strip_prefix(ctx.workspace_root())
      {
        files_to_backup.push(rel_path.to_path_buf());
      }
    }

    let metadata = BackupMetadata::new("cargo rail unify");
    let max_backups = ctx.config.as_ref().map(|c| c.unify.max_backups).unwrap_or(3);
    let backup_id = backup_manager.create_backup(&files_to_backup, metadata, max_backups)?;
    progress!("backup: {}", backup_id);
    created_backup_id = Some(backup_id);
  }

  let sort_mode = ctx
    .config
    .as_ref()
    .map(|c| c.unify.sort_dependencies)
    .unwrap_or(true);
  let writer = ManifestWriter::new().with_dependency_sort(sort_mode);

  if !plan.workspace_deps.is_empty() {
    progress!("writing [workspace.dependencies]...");
    writer.write_workspace_deps(&ctx.workspace_root().join("Cargo.toml"), &plan.workspace_deps)?;
  }

  progress!("updating {} members...", plan.member_edits.len());
  for (member_name, edits) in &plan.member_edits {
    let member_path = plan
      .member_paths
      .get(member_name)
      .ok_or_else(|| crate::error::RailError::message(format!("member path not found: {}", member_name)))?;

    for edit in edits {
      match edit {
        crate::cargo::MemberEdit::UseWorkspace {
          dep_name,
          dep_kind,
          target,
          local_features,
          is_optional,
        } => {
          writer.update_member(
            member_path,
            dep_name,
            *dep_kind,
            target.as_deref(),
            if local_features.is_empty() {
              None
            } else {
              Some(local_features.clone())
            },
            *is_optional,
          )?;
        }
        crate::cargo::MemberEdit::RemoveDep {
          dep_name,
          dep_kind,
          target,
        } => {
          writer.remove_dep(member_path, dep_name, *dep_kind, target.as_deref())?;
        }
        crate::cargo::MemberEdit::RemoveFeature { feature_name } => {
          writer.remove_feature(member_path, feature_name)?;
        }
        crate::cargo::MemberEdit::AddFeatures {
          dep_name,
          dep_kind,
          target,
          features_to_add,
        } => {
          writer.add_features(member_path, dep_name, *dep_kind, target.as_deref(), features_to_add)?;
        }
      }
    }
  }

  if !plan.transitive_pins.is_empty() {
    progress!("pinning {} transitives...", plan.transitive_pins.len());

    // STEP 1: Add transitive deps to [workspace.dependencies] first
    // This is required before we can reference them with `workspace = true`
    progress!("  adding to [workspace.dependencies]...");
    writer.write_transitive_workspace_deps(&ctx.workspace_root().join("Cargo.toml"), &plan.transitive_pins)?;

    // STEP 2: Add to host's [dev-dependencies] with workspace = true
    let transitive_host_setting = ctx.config.as_ref().map(|c| &c.unify.transitive_host);
    let is_root_host = matches!(
      transitive_host_setting,
      None | Some(crate::config::TransitiveFeatureHost::Root)
    );

    // For virtual workspaces (no [package] section), we can't use the root as the transitive host
    // because virtual manifests can't have [dev-dependencies]
    if is_root_host && is_virtual_workspace(ctx.workspace_root()) {
      // Auto-select first workspace member as the host
      let members = ctx.graph.workspace_members();
      if members.is_empty() {
        return Err(RailError::with_help(
          "transitive_host = \"root\" is incompatible with virtual workspaces".to_string(),
          "Virtual workspaces cannot have [dev-dependencies]. Set transitive_host to a workspace member path in your rail.toml:\n  \
           [unify]\n  \
           transitive_host = \"crates/some-crate\"".to_string(),
        ));
      }

      // Find a suitable member's path
      let first_member = &members[0];
      if let Some(pkg) = ctx.cargo.get_package(first_member) {
        let member_path = pkg
          .manifest_path
          .parent()
          .ok_or_else(|| RailError::message(format!("Invalid manifest path: {}", pkg.manifest_path)))?;
        let relative_path = member_path.strip_prefix(ctx.workspace_root()).unwrap_or(member_path);
        progress!(
          "  note: using '{}' as transitive host (virtual workspace detected)",
          relative_path
        );
        let host_path = member_path.join("Cargo.toml");
        writer.add_transitive_pins(&host_path.into_std_path_buf(), &plan.transitive_pins)?;
      } else {
        return Err(RailError::message("Failed to find a suitable transitive host member"));
      }
    } else {
      let host_path = match transitive_host_setting {
        Some(crate::config::TransitiveFeatureHost::Path(p)) => ctx.workspace_root().join(p).join("Cargo.toml"),
        _ => ctx.workspace_root().join("Cargo.toml"),
      };
      writer.add_transitive_pins(&host_path, &plan.transitive_pins)?;
    }
  }

  if let Some(ref msrv) = plan.computed_msrv {
    // Show warning if workspace mode has compatibility issues
    if let Some(ref warning) = msrv.warning {
      eprintln!("warning: {}", warning);
    }
    progress!(
      "writing rust-version = \"{}.{}\"...",
      msrv.version.major,
      msrv.version.minor
    );
    writer.write_workspace_msrv(&ctx.workspace_root().join("Cargo.toml"), &msrv.version)?;
  }

  if !no_report {
    let actual_report_path = report_path.unwrap_or_else(|| {
      ctx
        .workspace_root()
        .join("target")
        .join("cargo-rail")
        .join("unify-report.md")
    });
    UnifyReport::write_to_file(&plan, &actual_report_path)?;
    progress!("report: {}", actual_report_path.display());
  }

  // Summary
  println!(
    "\nunified {} dependencies across {} members",
    plan.workspace_deps.len(),
    plan.member_edits.len()
  );
  if !plan.transitive_pins.is_empty() {
    println!("  {} transitives pinned", plan.transitive_pins.len());
  }
  if !plan.duplicates_cleaned.is_empty() {
    println!("  {} duplicates resolved", plan.duplicates_cleaned.len());
  }
  if !plan.pruned_features.is_empty() {
    println!("  {} dead features pruned (empty no-ops)", plan.pruned_features.len());
  }
  if !plan.optional_features.is_empty() {
    println!(
      "  {} optional features detected (user-facing, preserved)",
      plan.optional_features.len()
    );
  }
  // Count undeclared feature fixes
  let features_fixed: usize = plan
    .member_edits
    .values()
    .flat_map(|edits| edits.iter())
    .filter_map(|e| match e {
      crate::cargo::MemberEdit::AddFeatures { features_to_add, .. } => Some(features_to_add.len()),
      _ => None,
    })
    .sum();
  if features_fixed > 0 {
    let crates_fixed: std::collections::HashSet<_> = plan
      .member_edits
      .iter()
      .filter(|(_, edits)| {
        edits
          .iter()
          .any(|e| matches!(e, crate::cargo::MemberEdit::AddFeatures { .. }))
      })
      .map(|(name, _)| name)
      .collect();
    println!(
      "  {} undeclared features fixed across {} crates",
      features_fixed,
      crates_fixed.len()
    );
  }
  if let Some(ref msrv) = plan.computed_msrv {
    use crate::cargo::MsrvSourceUsed;
    let source_desc = match msrv.source_used {
      MsrvSourceUsed::Deps => format!(
        "from deps: {}",
        msrv.contributors.first().unwrap_or(&"unknown".to_string())
      ),
      MsrvSourceUsed::Workspace => "preserved from workspace".to_string(),
      MsrvSourceUsed::MaxWorkspace => "from workspace (higher than deps)".to_string(),
      MsrvSourceUsed::MaxDeps => format!(
        "from deps: {}",
        msrv.contributors.first().unwrap_or(&"unknown".to_string())
      ),
    };
    println!(
      "  rust-version = {}.{} ({})",
      msrv.version.major, msrv.version.minor, source_desc
    );
  }

  println!("\nnext: cargo check && cargo test");

  // Show undo hint if backup was created
  if let Some(backup_id) = created_backup_id {
    println!("undo: cargo rail unify undo  (backup: {})", backup_id);
  }

  Ok(())
}

/// Restore a previous state from backup
pub fn run_unify_undo(workspace_root: &std::path::Path, list: bool, backup_id: Option<String>) -> RailResult<()> {
  use crate::backup::BackupManager;

  let backup_manager = BackupManager::new(workspace_root);

  if list {
    let backups = backup_manager.list_backups()?;

    if backups.is_empty() {
      println!("no backups found");
      return Ok(());
    }

    println!("backups:\n");
    for (i, backup) in backups.iter().enumerate() {
      let marker = if i == 0 { " (latest)" } else { "" };
      println!("  {}{}", backup.id, marker);
      println!("    {}", backup.metadata.timestamp);
      println!("    {} files", backup.metadata.files_modified.len());
    }

    println!("\nrestore with: cargo rail unify undo [--backup-id <id>]");
    return Ok(());
  }

  let target_backup_id = if let Some(id) = backup_id {
    id
  } else {
    match backup_manager.get_latest_backup()? {
      Some(backup) => backup.id,
      None => {
        return Err(crate::error::RailError::with_help(
          "no backups found",
          "run 'cargo rail unify undo --list' to see available backups",
        ));
      }
    }
  };

  backup_manager.restore_backup(&target_backup_id)?;

  Ok(())
}

/// Display detailed explanation of unification decisions
fn display_explain(plan: &crate::cargo::UnificationPlan) {
  use std::collections::BTreeMap;

  println!("\n=== Explanation ===\n");

  // Explain dependencies being unified
  if !plan.workspace_deps.is_empty() {
    println!("Dependencies unified to [workspace.dependencies]:\n");
    for dep in &plan.workspace_deps {
      println!("  {} = \"{}\"", dep.name, dep.version_req);
      println!("    reason: used by {} workspace member(s)", dep.used_by.len());
      if dep.used_by.len() <= 5 {
        for member in &dep.used_by {
          println!("      - {}", member);
        }
      } else {
        for member in dep.used_by.iter().take(3) {
          println!("      - {}", member);
        }
        println!("      ... and {} more", dep.used_by.len() - 3);
      }
      if !dep.features.is_empty() {
        println!("    features: union of features from all uses");
        let mut sorted_features = dep.features.clone();
        sorted_features.sort();
        println!("      [{}]", sorted_features.join(", "));
      }
      println!();
    }
  }

  // Explain transitive pins
  if !plan.transitive_pins.is_empty() {
    println!("Transitive dependencies pinned:\n");
    for pin in &plan.transitive_pins {
      println!("  {} = \"{}\"", pin.name, pin.version);
      println!("    reason: transitive dep with target-specific features");
      if !pin.features.is_empty() {
        let mut sorted = pin.features.clone();
        sorted.sort();
        println!("    features: [{}]", sorted.join(", "));
      }
      println!();
    }
  }

  // Explain unused deps being removed
  if !plan.unused_deps.is_empty() {
    println!("Unused dependencies flagged for removal:\n");
    for unused in &plan.unused_deps {
      println!("  {} in {}", unused.dep_name, unused.member);
      println!("    reason: {:?}", unused.reason);
      println!();
    }
  }

  // Explain pruned features
  if !plan.pruned_features.is_empty() {
    println!("Dead features pruned:\n");
    for pruned in &plan.pruned_features {
      println!("  [features].{} in {}", pruned.feature_name, pruned.crate_name);
      println!("    reason: empty feature (no dependencies, no sub-features)");
      println!();
    }
  }

  // Explain issues/blockers
  if !plan.issues.is_empty() {
    println!("Issues detected:\n");

    // Group by severity
    let mut by_severity: BTreeMap<String, Vec<_>> = BTreeMap::new();
    for issue in &plan.issues {
      let severity = format!("{:?}", issue.severity);
      by_severity.entry(severity).or_default().push(issue);
    }

    for (severity, issues) in &by_severity {
      println!("  {} ({}):", severity, issues.len());
      for issue in issues {
        println!("    {}: {}", issue.dep_name, issue.message);
      }
      println!();
    }
  }

  // Explain undeclared features
  if !plan.undeclared_features.is_empty() {
    println!("Undeclared features detected:\n");
    for uf in &plan.undeclared_features {
      println!("  {} in {}", uf.dep_name, uf.member);
      println!("    undeclared: [{}]", uf.undeclared_features.join(", "));
      if !uf.borrowed_from.is_empty() {
        println!("    borrowed from: {}", uf.borrowed_from.join(", "));
      }
      println!("    reason: features enabled via resolver but not declared in Cargo.toml");
      println!();
    }
  }

  // Explain why no changes if plan is empty
  if plan.workspace_deps.is_empty()
    && plan.member_edits.is_empty()
    && plan.transitive_pins.is_empty()
    && plan.unused_deps.is_empty()
  {
    println!("No unification opportunities found.\n");
    println!("Possible reasons:");
    println!("  - Dependencies are already unified");
    println!("  - Dependencies have incompatible versions (see issues above)");
    println!("  - Dependencies are excluded via [unify].exclude config");
    println!("  - Dependencies are renamed (use include_renamed = true)");
    println!("  - Single-use dependencies (not shared across crates)");
  }
}

/// Check if a workspace is a "virtual" workspace (has [workspace] but no [package])
///
/// Virtual workspaces cannot have [dev-dependencies] directly in their manifest,
/// which affects transitive dependency pinning.
fn is_virtual_workspace(workspace_root: &std::path::Path) -> bool {
  use std::fs;

  let root_manifest = workspace_root.join("Cargo.toml");
  let Ok(content) = fs::read_to_string(&root_manifest) else {
    return false;
  };

  let Ok(doc) = content.parse::<toml_edit::DocumentMut>() else {
    return false;
  };

  // A virtual workspace has [workspace] but no [package]
  doc.contains_key("workspace") && !doc.contains_key("package")
}
