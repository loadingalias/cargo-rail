//! `cargo rail unify` - Workspace dependency unification
//!
//! Implements resolution-based unification:
//! 1. Multi-target metadata for versions
//! 2. Manifest parsing for minimal features
//! 3. Intersection-based feature unification

use crate::cargo::{ManifestWriter, UnifyAnalyzer, UnifyReport};
use crate::commands::common::OutputFormat;
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;

/// Analyze workspace dependencies (check mode)
pub fn run_unify_analyze(
  ctx: &WorkspaceContext,
  exclude: Vec<String>,
  include: Vec<String>,
  pin_transitives_flag: bool,
  include_renamed_flag: bool,
  show_diff: bool,
  format: String,
) -> RailResult<()> {
  let output_format: OutputFormat = format.parse()?;
  let json = output_format.is_json();

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
  if include_renamed_flag {
    analyzer.config.include_renamed = true;
  }

  // Run analysis
  let plan = analyzer.analyze()?;

  // JSON output mode
  if json {
    let output = serde_json::json!({
      "workspace_deps": plan.workspace_deps.iter().map(|d| serde_json::json!({
        "name": d.name,
        "version": d.version_req,
        "features": d.features,
      })).collect::<Vec<_>>(),
      "member_edits_count": plan.member_edits.values().map(|v| v.len()).sum::<usize>(),
      "members_affected": plan.member_edits.len(),
      "transitive_pins": plan.transitive_pins.len(),
      "has_blocking_issues": plan.has_blocking_issues(),
      "issues": plan.issues.iter().map(|i| serde_json::json!({
        "dep_name": i.dep_name,
        "severity": format!("{:?}", i.severity),
        "message": i.message,
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
          println!("      features = {:?}", features);
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
              line.push_str(&format!(", features = {:?}", features));
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

  // Final message
  if plan.has_blocking_issues() {
    eprintln!("\nerror: blocking issues prevent unification");
  } else if !plan.workspace_deps.is_empty() || !plan.member_edits.is_empty() {
    let total_edits: usize = plan.member_edits.values().map(|v| v.len()).sum();
    if !plan.workspace_deps.is_empty() {
      println!(
        "\nready: {} dependencies, {} member edits\nrun 'cargo rail unify' to apply",
        plan.workspace_deps.len(),
        total_edits
      );
    } else {
      println!(
        "\nready: {} members, {} edits\nrun 'cargo rail unify' to apply",
        plan.member_edits.len(),
        total_edits
      );
    }
  } else {
    println!("\nno unification opportunities found");
  }

  Ok(())
}

/// Execute dependency unification
pub fn run_unify_apply(
  ctx: &WorkspaceContext,
  exclude: Vec<String>,
  include: Vec<String>,
  backup: bool,
  include_renamed_flag: bool,
  no_report: bool,
  report_path: Option<std::path::PathBuf>,
) -> RailResult<()> {
  use crate::backup::{BackupManager, BackupMetadata};
  use std::path::PathBuf;

  let mut analyzer = UnifyAnalyzer::new(ctx)?;

  // Apply CLI overrides
  if !exclude.is_empty() {
    analyzer.config.exclude.extend(exclude);
  }
  if !include.is_empty() {
    analyzer.config.include.extend(include);
  }
  if include_renamed_flag {
    analyzer.config.include_renamed = true;
  }

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

  if should_backup {
    if is_first_run && !backup {
      eprintln!("creating backup (first run)...");
    } else {
      eprintln!("creating backup...");
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
    eprintln!("backup: {}", backup_id);
  }

  let writer = ManifestWriter::new();

  if !plan.workspace_deps.is_empty() {
    eprintln!("writing [workspace.dependencies]...");
    writer.write_workspace_deps(&ctx.workspace_root().join("Cargo.toml"), &plan.workspace_deps)?;
  }

  eprintln!("updating {} members...", plan.member_edits.len());
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
      }
    }
  }

  if !plan.transitive_pins.is_empty() {
    eprintln!("pinning {} transitives...", plan.transitive_pins.len());
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
      let metadata = ctx.cargo.metadata();
      if let Some(pkg) = metadata.workspace_packages().iter().find(|p| p.name == *first_member) {
        let member_path = pkg.manifest_path.parent().unwrap();
        let relative_path = member_path.strip_prefix(ctx.workspace_root()).unwrap_or(member_path);
        eprintln!(
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
    eprintln!(
      "writing rust-version = \"{}.{}\"...",
      msrv.version.major, msrv.version.minor
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
    eprintln!("report: {}", actual_report_path.display());
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
    println!("  {} dead features pruned", plan.pruned_features.len());
  }
  if let Some(ref msrv) = plan.computed_msrv {
    println!(
      "  rust-version = {}.{} (from {})",
      msrv.version.major,
      msrv.version.minor,
      msrv.contributors.first().unwrap_or(&"unknown".to_string())
    );
  }

  println!("\nnext: cargo check && cargo test");

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
