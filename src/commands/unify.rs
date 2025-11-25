//! `cargo rail unify` - Workspace dependency unification (CLEAN implementation)
//!
//! This command implements the HYBRID approach:
//! 1. Multi-target metadata for versions
//! 2. Manifest parsing for minimal features
//! 3. Clean unification with intersection-based features

use crate::cargo::{ManifestWriter, UnifyAnalyzer, UnifyReport};
use crate::error::RailResult;
use crate::workspace::WorkspaceContext;

/// Run dependency unification analysis (dry-run)
pub fn run_unify_analyze(
  ctx: &WorkspaceContext,
  exclude: Vec<String>,
  include: Vec<String>,
  pin_transitives_flag: bool,
) -> RailResult<()> {
  println!("🔍 Analyzing workspace dependencies...\n");

  // Create analyzer
  let mut analyzer = UnifyAnalyzer::new(ctx)?;

  // Apply CLI overrides to config
  if !exclude.is_empty() {
    analyzer.config.exclude.extend(exclude);
  }
  if !include.is_empty() {
    analyzer.config.include.extend(include);
  }
  if pin_transitives_flag {
    analyzer.config.pin_transitives = true;
  }

  // Run analysis
  let plan = analyzer.analyze()?;

  // Display summary
  println!("{}", plan.summary());

  // Show validation results if any failed
  let failed_validations: Vec<_> = plan.validation_results.iter().filter(|v| !v.success).collect();

  if !failed_validations.is_empty() {
    println!("\n⚠️  Validation Issues:");
    for val in failed_validations {
      println!(
        "  - {}: {}",
        val.target,
        val.error.as_ref().unwrap_or(&"Unknown error".to_string())
      );
    }
  }

  // Final message
  if plan.has_blocking_issues() {
    println!("\n❌ Cannot proceed with unification due to blocking issues.");
  } else if !plan.workspace_deps.is_empty() || !plan.member_edits.is_empty() {
    // Count total member edit operations
    let total_edits: usize = plan.member_edits.values().map(|v| v.len()).sum();
    if !plan.workspace_deps.is_empty() {
      println!(
        "\n✅ Ready to unify {} dependencies ({} member edits). Run 'cargo rail unify apply' to apply changes.",
        plan.workspace_deps.len(),
        total_edits
      );
    } else {
      println!(
        "\n✅ Ready to convert {} members to use workspace inheritance ({} edits). Run 'cargo rail unify apply' to apply changes.",
        plan.member_edits.len(),
        total_edits
      );
    }
  } else {
    println!("\n📊 No unification opportunities found.");
  }

  Ok(())
}

/// Apply dependency unification (modify files)
pub fn run_unify_apply(
  ctx: &WorkspaceContext,
  exclude: Vec<String>,
  include: Vec<String>,
  backup: bool,
) -> RailResult<()> {
  use crate::backup::{BackupManager, BackupMetadata};
  use std::path::PathBuf;

  println!("🔧 Applying workspace dependency unification...\n");

  // Create analyzer
  let mut analyzer = UnifyAnalyzer::new(ctx)?;

  // Apply CLI overrides to config
  if !exclude.is_empty() {
    analyzer.config.exclude.extend(exclude);
  }
  if !include.is_empty() {
    analyzer.config.include.extend(include);
  }

  // Run analysis
  let plan = analyzer.analyze()?;

  // Check for blockers
  if plan.has_blocking_issues() {
    println!("❌ Cannot apply unification due to blocking issues:");
    for issue in &plan.issues {
      if issue.severity == crate::cargo::IssueSeverity::Error {
        println!("  - {}: {}", issue.dep_name, issue.message);
      }
    }
    return Err(crate::error::RailError::message("Blocking issues prevent unification"));
  }

  // Check if there's any work to do (either new workspace deps or member edits)
  if plan.workspace_deps.is_empty() && plan.member_edits.is_empty() {
    println!("📊 No dependencies to unify.");
    return Ok(());
  }

  // Create backup if requested OR if this is the first unify run (safety feature)
  let backup_manager = BackupManager::new(ctx.workspace_root());
  let is_first_run = !backup_manager.has_backups();
  let should_backup = backup || is_first_run;

  if should_backup {
    if is_first_run && !backup {
      println!("📦 Creating safety backup (first unify run in this workspace)...");
    } else {
      println!("📦 Creating backup...");
    }

    // Collect all files that will be modified
    let mut files_to_backup = Vec::new();

    // Only include workspace Cargo.toml if we're adding new workspace deps
    if !plan.workspace_deps.is_empty() {
      files_to_backup.push(PathBuf::from("Cargo.toml"));
    }

    for member_name in plan.member_edits.keys() {
      if let Some(manifest_path) = plan.member_paths.get(member_name) {
        // Convert absolute path to relative path from workspace root
        if let Ok(rel_path) = manifest_path.strip_prefix(ctx.workspace_root()) {
          files_to_backup.push(rel_path.to_path_buf());
        }
      }
    }

    let metadata = BackupMetadata::new("cargo rail unify apply");
    let max_backups = ctx.config.as_ref().map(|c| c.unify.max_backups).unwrap_or(3);
    let backup_id = backup_manager.create_backup(&files_to_backup, metadata, max_backups)?;
    println!("   Backup created: {}", backup_id);
    println!();
  }

  // Apply changes
  let writer = ManifestWriter::new();

  // Write workspace dependencies only if there are new deps to add
  if !plan.workspace_deps.is_empty() {
    println!("📝 Writing [workspace.dependencies]...");
    writer.write_workspace_deps(&ctx.workspace_root().join("Cargo.toml"), &plan.workspace_deps)?;
  }

  // Update members
  println!("📝 Updating {} member manifests...", plan.member_edits.len());
  for (member_name, edits) in &plan.member_edits {
    // Get member path from the plan's mapping
    let member_path = plan
      .member_paths
      .get(member_name)
      .ok_or_else(|| crate::error::RailError::message(format!("Member path not found for {}", member_name)))?;

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
            target.as_deref(), // Pass target for correct section
            if local_features.is_empty() {
              None
            } else {
              Some(local_features.clone())
            },
            *is_optional,
          )?;
        }
      }
    }
  }

  // Handle transitive pins
  if !plan.transitive_pins.is_empty() {
    println!("📌 Pinning {} transitive dependencies...", plan.transitive_pins.len());

    // Determine host path based on config
    let host_path = match &ctx.config.as_ref().map(|c| &c.unify.transitive_host) {
      Some(crate::config::TransitiveFeatureHost::Path(p)) => ctx.workspace_root().join(p).join("Cargo.toml"),
      _ => ctx.workspace_root().join("Cargo.toml"),
    };

    writer.add_transitive_pins(&host_path, &plan.transitive_pins)?;
  }

  // Write computed MSRV to workspace manifest if enabled
  if let Some(ref msrv) = plan.computed_msrv {
    println!(
      "📋 Writing rust-version = \"{}.{}\" to [workspace.package]...",
      msrv.version.major, msrv.version.minor
    );
    writer.write_workspace_msrv(&ctx.workspace_root().join("Cargo.toml"), &msrv.version)?;
  }

  // Generate report
  println!("📄 Generating report...");
  let report_path = ctx
    .workspace_root()
    .join("target")
    .join("cargo-rail")
    .join("unify-report.md");
  UnifyReport::write_to_file(&plan, &report_path)?;
  println!("   Report saved to: {}", report_path.display());

  // Summary
  println!("\n✅ Unification complete!");
  println!("   - {} dependencies unified", plan.workspace_deps.len());
  println!("   - {} members updated", plan.member_edits.len());
  if !plan.transitive_pins.is_empty() {
    println!("   - {} transitives pinned", plan.transitive_pins.len());
  }
  if let Some(ref msrv) = plan.computed_msrv {
    println!(
      "   - MSRV set to {}.{} (from {})",
      msrv.version.major,
      msrv.version.minor,
      msrv.contributors.first().unwrap_or(&"unknown".to_string())
    );
  }

  println!("\nNext steps:");
  println!("  1. Run 'cargo check' to verify everything compiles");
  println!("  2. Test your workspace thoroughly");
  println!("  3. Commit the changes");

  Ok(())
}

/// Undo a unify operation by restoring a backup
pub fn run_unify_undo(workspace_root: &std::path::Path, list: bool, backup_id: Option<String>) -> RailResult<()> {
  use crate::backup::BackupManager;

  let backup_manager = BackupManager::new(workspace_root);

  // Handle --list flag
  if list {
    println!("📋 Available backups:\n");

    let backups = backup_manager.list_backups()?;

    if backups.is_empty() {
      println!("No backups found.");
      return Ok(());
    }

    for (i, backup) in backups.iter().enumerate() {
      let marker = if i == 0 { " (latest)" } else { "" };
      println!("{}. {}{}", i + 1, backup.id, marker);
      println!("   Date: {}", backup.metadata.timestamp);
      println!("   Command: {}", backup.metadata.command);
      println!("   Files: {}", backup.metadata.files_modified.len());
      println!();
    }

    println!("To restore a backup, run:");
    println!("  cargo rail unify undo");
    println!("  cargo rail unify undo --backup-id <id>");

    return Ok(());
  }

  // Determine which backup to restore
  let target_backup_id = if let Some(id) = backup_id {
    id
  } else {
    // Use latest backup
    match backup_manager.get_latest_backup()? {
      Some(backup) => backup.id,
      None => {
        println!("No backups found.");
        return Ok(());
      }
    }
  };

  // Restore the backup
  backup_manager.restore_backup(&target_backup_id)?;

  Ok(())
}
