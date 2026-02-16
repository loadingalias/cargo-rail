//! `cargo rail unify` - Workspace dependency unification
//!
//! Implements resolution-based unification:
//! 1. Multi-target metadata for versions
//! 2. Manifest parsing for minimal features
//! 3. Intersection-based feature unification

use crate::cargo::{ManifestWriter, UnifyAnalyzer, UnifyReport};
use crate::commands::common::UnifyOutputFormat;
use crate::error::{RailError, RailResult};
use crate::mutation::{self, MutationAction, MutationRisk, MutationTrace};
use crate::progress;
use crate::workspace::WorkspaceContext;
use std::path::PathBuf;

struct UnifyTextSink {
  buffer: Option<String>,
}

impl UnifyTextSink {
  fn new(capture: bool) -> Self {
    Self {
      buffer: capture.then_some(String::new()),
    }
  }

  fn push_line(&mut self, args: std::fmt::Arguments<'_>) {
    if let Some(ref mut buf) = self.buffer {
      use std::fmt::Write as _;
      let _ = buf.write_fmt(args);
      buf.push('\n');
    } else {
      println!("{}", args);
    }
  }

  fn finish(self) -> Option<String> {
    self.buffer
  }
}

macro_rules! outln {
  ($sink:expr $(,)?) => {{
    $sink.push_line(format_args!(""));
  }};
  ($sink:expr, $($arg:tt)*) => {{
    $sink.push_line(format_args!($($arg)*));
  }};
}

fn write_output(content: &str, output_file: Option<&PathBuf>) -> RailResult<()> {
  use std::io::Write as _;

  let needs_trailing_newline = !content.ends_with('\n');
  match output_file {
    Some(path) => {
      let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| RailError::message(format!("failed to open '{}': {}", path.display(), e)))?;
      file
        .write_all(content.as_bytes())
        .map_err(|e| RailError::message(format!("failed to write '{}': {}", path.display(), e)))?;
      if needs_trailing_newline {
        file
          .write_all(b"\n")
          .map_err(|e| RailError::message(format!("failed to write '{}': {}", path.display(), e)))?;
      }
      progress!("output: {}", path.display());
      Ok(())
    }
    None => {
      print!("{}", content);
      if needs_trailing_newline {
        println!();
      }
      Ok(())
    }
  }
}

/// Analyze workspace dependencies (check mode)
pub fn run_unify_analyze(
  ctx: &WorkspaceContext,
  show_diff: bool,
  explain: bool,
  format: UnifyOutputFormat,
  output: Option<&PathBuf>,
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
  let msrv_write_needed = if let Some(msrv) = plan.computed_msrv.as_ref() {
    workspace_msrv_write_needed(ctx.workspace_root(), &msrv.version)?
  } else {
    false
  };

  let has_changes = plan.has_planned_changes(msrv_write_needed);
  let mutation_plan = build_unify_mutation_plan(ctx, &plan, msrv_write_needed, false, true, output)?;

  // JSON output mode (but still honor exit codes)
  if json {
    let mut canonical_actions = mutation_plan.actions.clone();
    canonical_actions.sort_by(|a, b| {
      a.code
        .cmp(&b.code)
        .then_with(|| a.target.cmp(&b.target))
        .then_with(|| a.detail.cmp(&b.detail))
    });
    let mut reason_codes: Vec<String> = mutation_plan
      .trace
      .iter()
      .map(|t| t.code.clone())
      .chain(mutation_plan.risks.iter().map(|r| r.code.clone()))
      .collect();
    reason_codes.sort();
    reason_codes.dedup();

    let output_json = serde_json::json!({
      "command": "unify",
      "check": true,
      "msrv_write_needed": msrv_write_needed,
      "has_changes": has_changes,
      "workspace_deps": plan.workspace_deps.iter().map(|d| {
        let features: Vec<&str> = d.features.iter().map(|f| &**f).collect();
        serde_json::json!({
          "name": &*d.name,
          "version": d.version_req,
          "features": features,
        })
      }).collect::<Vec<_>>(),
      "summary": {
        "workspace_deps_count": plan.workspace_deps.len(),
        "member_edits_count": plan.member_edit_count(),
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
        "dep_name": &*i.dep_name,
        "severity": format!("{:?}", i.severity),
        "message": &*i.message,
      })).collect::<Vec<_>>(),
      "action_plan": canonical_actions,
      "reason_codes": reason_codes,
      "mutation_plan": mutation_plan,
    });

    let rendered =
      serde_json::to_string_pretty(&output_json).map_err(|e| RailError::message(format!("JSON error: {}", e)))?;
    write_output(&rendered, output)?;

    if plan.has_blocking_issues() {
      return Err(RailError::message("blocking issues prevent unification"));
    }
    if has_changes {
      return Err(RailError::CheckHasPendingChanges);
    }
    return Ok(());
  }

  let mut sink = UnifyTextSink::new(output.is_some());

  // Display summary
  outln!(sink, "{}", plan.summary());

  // Show explain output if requested
  if explain {
    display_explain(&mut sink, &plan);
  }

  // Show diff if requested
  if show_diff && has_changes {
    outln!(sink);
    outln!(sink, "planned changes:");
    outln!(sink);

    // Show MSRV update first (workspace manifest)
    if let Some(msrv) = plan.computed_msrv.as_ref()
      && msrv_write_needed
    {
      outln!(sink, "[workspace.package]:");
      outln!(
        sink,
        "  rust-version = \"{}.{}.{}\"",
        msrv.version.major,
        msrv.version.minor,
        msrv.version.patch
      );
      outln!(sink);
    }

    // Show workspace deps that will be added
    if !plan.workspace_deps.is_empty() || !plan.transitive_pins.is_empty() {
      outln!(sink, "[workspace.dependencies]:");
      for dep in &plan.workspace_deps {
        outln!(sink, "  + {} = \"{}\"", dep.name, dep.version_req);
        if !dep.features.is_empty() {
          let mut features = dep.features.clone();
          features.sort();
          outln!(
            sink,
            "      features = [{}]",
            features
              .iter()
              .map(|f| format!("\"{}\"", f))
              .collect::<Vec<_>>()
              .join(", ")
          );
        }
      }

      // Show transitive pins (also written to workspace.dependencies)
      for pin in &plan.transitive_pins {
        outln!(sink, "  + {} = \"{}\"  # transitive pin", pin.name, pin.version);
        if !pin.features.is_empty() {
          let mut features = pin.features.clone();
          features.sort();
          outln!(
            sink,
            "      features = [{}]",
            features
              .iter()
              .map(|f| format!("\"{}\"", f))
              .collect::<Vec<_>>()
              .join(", ")
          );
        }
      }
      outln!(sink);
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
        .unwrap_or_else(|| member.to_string());
      outln!(sink, "{}:", path);
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
            outln!(sink, "{}", line);
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
            outln!(sink, "  {} {} -> REMOVE (unused)", section, dep_name);
          }
          crate::cargo::MemberEdit::RemoveFeature { feature_name } => {
            outln!(sink, "  [features] {} -> REMOVE (dead/empty)", feature_name);
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
            outln!(
              sink,
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
          crate::cargo::MemberEdit::EnforceMsrvInheritance => {
            outln!(sink, "  [package] rust-version = {{ workspace = true }}");
          }
        }
      }
      outln!(sink);
    }
  }

  // Show validation results if any failed
  let failed_validations: Vec<_> = plan.validation_results.iter().filter(|v| !v.success).collect();

  if !failed_validations.is_empty() {
    eprintln!();
    crate::error!("validation errors:");
    for val in failed_validations {
      eprintln!("  {}: {}", val.target, val.error.as_deref().unwrap_or("unknown"));
    }
  }

  // Final message and exit code
  if plan.has_blocking_issues() {
    eprintln!();
    crate::error!("blocking issues prevent unification");
    if let Some(content) = sink.finish() {
      write_output(&content, output)?;
    }
    return Err(RailError::message("blocking issues prevent unification"));
  } else if has_changes {
    let total_edits = plan.member_edit_count();
    if !plan.workspace_deps.is_empty() || !plan.transitive_pins.is_empty() || msrv_write_needed {
      outln!(
        sink,
        "\nready: {} dependencies, {} member edits",
        plan.workspace_deps.len(),
        total_edits
      );
    } else {
      outln!(
        sink,
        "\nready: {} members, {} edits",
        plan.member_edits.len(),
        total_edits
      );
    }
    outln!(sink, "Changes detected. Run without --check to apply.");
    if let Some(content) = sink.finish() {
      write_output(&content, output)?;
    }
    return Err(RailError::CheckHasPendingChanges);
  } else {
    outln!(sink, "\nno unification opportunities found");
  }

  if let Some(content) = sink.finish() {
    write_output(&content, output)?;
  }

  Ok(())
}

/// Execute dependency unification
pub fn run_unify_apply(
  ctx: &WorkspaceContext,
  backup: bool,
  no_report: bool,
  report_path: Option<std::path::PathBuf>,
  plan_path: Option<std::path::PathBuf>,
) -> RailResult<()> {
  use crate::backup::{BackupManager, BackupMetadata};
  use std::path::PathBuf;

  // Create analyzer (config comes from rail.toml via ctx)
  let analyzer = UnifyAnalyzer::new(ctx)?;

  let plan = analyzer.analyze()?;
  let msrv_write_needed = if let Some(msrv) = plan.computed_msrv.as_ref() {
    workspace_msrv_write_needed(ctx.workspace_root(), &msrv.version)?
  } else {
    false
  };

  // Check for blockers
  if plan.has_blocking_issues() {
    crate::error!("blocking issues prevent unification:");
    for issue in &plan.issues {
      if issue.severity == crate::cargo::IssueSeverity::Error {
        eprintln!("  {}: {}", issue.dep_name, issue.message);
      }
    }
    return Err(crate::error::RailError::message("blocking issues prevent unification"));
  }

  if !plan.has_planned_changes(msrv_write_needed) {
    println!("nothing to unify");
    return Ok(());
  }

  let expected_mutation_plan =
    build_unify_mutation_plan(ctx, &plan, msrv_write_needed, backup, no_report, report_path.as_ref())?;
  let mutation_plan = if let Some(path) = plan_path.as_ref() {
    let from_file = mutation::read_plan_file(path)?;
    if from_file.contract_version != mutation::MUTATION_CONTRACT_VERSION {
      return Err(RailError::with_help(
        format!(
          "unsupported mutation plan contract version: {} (expected {})",
          from_file.contract_version,
          mutation::MUTATION_CONTRACT_VERSION
        ),
        "regenerate the plan using the current cargo-rail version".to_string(),
      ));
    }
    if !from_file.operation_id.starts_with("unify-") {
      return Err(RailError::with_help(
        format!("plan '{}' is not a unify plan", path.display()),
        "use a plan generated from 'cargo rail unify --check -f json'".to_string(),
      ));
    }
    mutation::validate_pre_apply(ctx, &from_file)?;
    if from_file.inputs_fingerprint != expected_mutation_plan.inputs_fingerprint {
      return Err(RailError::with_help(
        "provided plan does not match current requested unify operation".to_string(),
        "regenerate the plan and re-run apply with --plan".to_string(),
      ));
    }
    from_file
  } else {
    mutation::validate_pre_apply(ctx, &expected_mutation_plan)?;
    expected_mutation_plan
  };
  let plan_receipt = mutation::write_receipt(
    ctx.workspace_root(),
    "unify",
    "plan",
    "planned",
    mutation_plan.clone(),
    vec![MutationTrace::new(
      "UNIFY_PLAN_CREATED",
      "created deterministic unify mutation plan",
    )],
  )?;
  progress!("receipt: {}", plan_receipt.display());

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

    // Estimate: root Cargo.toml + one per member with edits
    let mut files_to_backup = Vec::with_capacity(1 + plan.member_edits.len());
    if !plan.workspace_deps.is_empty() || !plan.transitive_pins.is_empty() || msrv_write_needed {
      files_to_backup.push(PathBuf::from("Cargo.toml"));
    }

    // Transitive pins may also modify the configured host's Cargo.toml.
    if !plan.transitive_pins.is_empty() {
      let host_path = transitive_pins_host_manifest_path(ctx)?;
      if let Ok(rel_path) = host_path.strip_prefix(ctx.workspace_root()) {
        // Avoid duplicating root Cargo.toml when host is root.
        if rel_path != std::path::Path::new("Cargo.toml") {
          files_to_backup.push(rel_path.to_path_buf());
        }
      }
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

  let sort_mode = ctx.config.as_ref().map(|c| c.unify.sort_dependencies).unwrap_or(true);
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
              Some(local_features.as_slice())
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
        crate::cargo::MemberEdit::EnforceMsrvInheritance => {
          writer.enforce_member_msrv_inheritance(member_path)?;
        }
      }
    }
  }

  if !plan.transitive_pins.is_empty() {
    progress!("pinning {} transitives...", plan.transitive_pins.len());

    // Add transitive deps to [workspace.dependencies] first
    // This is required before we can reference them with `workspace = true`
    progress!("  adding to [workspace.dependencies]...");
    writer.write_transitive_workspace_deps(&ctx.workspace_root().join("Cargo.toml"), &plan.transitive_pins)?;

    // Add to host's [dev-dependencies] with workspace = true
    let host_path = transitive_pins_host_manifest_path(ctx)?;
    let host_dir = host_path.parent().unwrap_or(&host_path);
    let relative_path = host_dir.strip_prefix(ctx.workspace_root()).unwrap_or(host_dir);
    if relative_path != std::path::Path::new("") && host_path != ctx.workspace_root().join("Cargo.toml") {
      progress!("  host: {}", relative_path.display());
    }
    writer.add_transitive_pins(&host_path, &plan.transitive_pins)?;
  }

  if let Some(ref msrv) = plan.computed_msrv {
    // Show warning if workspace mode has compatibility issues
    if let Some(ref warning) = msrv.warning {
      crate::warn!("{}", warning);
    }
    if msrv_write_needed {
      progress!(
        "writing rust-version = \"{}.{}.{}\"...",
        msrv.version.major,
        msrv.version.minor,
        msrv.version.patch
      );
      writer.write_workspace_msrv(&ctx.workspace_root().join("Cargo.toml"), &msrv.version)?;
    }
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
      "  rust-version = {}.{}.{} ({})",
      msrv.version.major, msrv.version.minor, msrv.version.patch, source_desc
    );
  }

  println!("\nnext: cargo check && cargo test");

  // Show undo hint if backup was created
  if let Some(backup_id) = created_backup_id {
    println!("undo: cargo rail unify undo  (backup: {})", backup_id);
  }

  let apply_receipt = mutation::write_receipt(
    ctx.workspace_root(),
    "unify",
    "apply",
    "applied",
    mutation_plan,
    vec![
      MutationTrace::new("UNIFY_APPLY_STARTED", "started applying unify plan"),
      MutationTrace::new("UNIFY_APPLY_COMPLETED", "completed unify apply"),
    ],
  )?;
  progress!("receipt: {}", apply_receipt.display());

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
fn display_explain(sink: &mut UnifyTextSink, plan: &crate::cargo::UnificationPlan) {
  use std::collections::BTreeMap;

  outln!(sink);
  outln!(sink, "=== Explanation ===");
  outln!(sink);

  // Explain dependencies being unified
  if !plan.workspace_deps.is_empty() {
    outln!(sink, "Dependencies unified to [workspace.dependencies]:");
    outln!(sink);
    for dep in &plan.workspace_deps {
      outln!(sink, "  {} = \"{}\"", dep.name, dep.version_req);
      outln!(sink, "    reason: used by {} workspace member(s)", dep.used_by.len());
      if dep.used_by.len() <= 5 {
        for member in &dep.used_by {
          outln!(sink, "      - {}", member);
        }
      } else {
        for member in dep.used_by.iter().take(3) {
          outln!(sink, "      - {}", member);
        }
        outln!(sink, "      ... and {} more", dep.used_by.len() - 3);
      }
      if !dep.features.is_empty() {
        outln!(sink, "    features: union of features from all uses");
        let mut sorted_features = dep.features.clone();
        sorted_features.sort();
        outln!(sink, "      [{}]", sorted_features.join(", "));
      }
      outln!(sink);
    }
  }

  // Explain transitive pins
  if !plan.transitive_pins.is_empty() {
    outln!(sink, "Transitive dependencies pinned:");
    outln!(sink);
    for pin in &plan.transitive_pins {
      outln!(sink, "  {} = \"{}\"", pin.name, pin.version);
      outln!(sink, "    reason: transitive dep with target-specific features");
      if !pin.features.is_empty() {
        let mut sorted = pin.features.clone();
        sorted.sort();
        outln!(sink, "    features: [{}]", sorted.join(", "));
      }
      outln!(sink);
    }
  }

  // Explain unused deps being removed
  if !plan.unused_deps.is_empty() {
    outln!(sink, "Unused dependencies flagged for removal:");
    outln!(sink);
    for unused in &plan.unused_deps {
      outln!(sink, "  {} in {}", unused.dep_name, unused.member);
      outln!(sink, "    reason: {:?}", unused.reason);
      outln!(sink);
    }
  }

  // Explain pruned features
  if !plan.pruned_features.is_empty() {
    outln!(sink, "Dead features pruned:");
    outln!(sink);
    for pruned in &plan.pruned_features {
      outln!(sink, "  [features].{} in {}", pruned.feature_name, pruned.crate_name);
      outln!(sink, "    reason: empty feature (no dependencies, no sub-features)");
      outln!(sink);
    }
  }

  // Explain issues/blockers
  if !plan.issues.is_empty() {
    outln!(sink, "Issues detected:");
    outln!(sink);

    // Group by severity
    let mut by_severity: BTreeMap<String, Vec<_>> = BTreeMap::new();
    for issue in &plan.issues {
      let severity = format!("{:?}", issue.severity);
      by_severity.entry(severity).or_default().push(issue);
    }

    for (severity, issues) in &by_severity {
      outln!(sink, "  {} ({}):", severity, issues.len());
      for issue in issues {
        outln!(sink, "    {}: {}", issue.dep_name, issue.message);
      }
      outln!(sink);
    }
  }

  // Explain undeclared features
  if !plan.undeclared_features.is_empty() {
    outln!(sink, "Undeclared features detected:");
    outln!(sink);
    for uf in &plan.undeclared_features {
      outln!(sink, "  {} in {}", uf.dep_name, uf.member);
      outln!(sink, "    undeclared: [{}]", uf.undeclared_features.join(", "));
      if !uf.borrowed_from.is_empty() {
        outln!(sink, "    borrowed from: {}", uf.borrowed_from.join(", "));
      }
      outln!(
        sink,
        "    reason: features enabled via resolver but not declared in Cargo.toml"
      );
      outln!(sink);
    }
  }

  // Explain why no changes if plan is empty
  if plan.workspace_deps.is_empty()
    && plan.member_edits.is_empty()
    && plan.transitive_pins.is_empty()
    && plan.unused_deps.is_empty()
  {
    outln!(sink, "No unification opportunities found.");
    outln!(sink);
    outln!(sink, "Possible reasons:");
    outln!(sink, "  - Dependencies are already unified");
    outln!(sink, "  - Dependencies have incompatible versions (see issues above)");
    outln!(sink, "  - Dependencies are excluded via [unify].exclude config");
    outln!(sink, "  - Dependencies are renamed (use include_renamed = true)");
    outln!(sink, "  - Single-use dependencies (not shared across crates)");
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

fn workspace_msrv_write_needed(workspace_root: &std::path::Path, msrv: &semver::Version) -> RailResult<bool> {
  use std::fs;

  let root_manifest = workspace_root.join("Cargo.toml");
  let Ok(content) = fs::read_to_string(&root_manifest) else {
    return Ok(true);
  };
  let Ok(doc) = content.parse::<toml_edit::DocumentMut>() else {
    return Ok(true);
  };

  let desired = format!("{}.{}.{}", msrv.major, msrv.minor, msrv.patch);
  let current = doc
    .get("workspace")
    .and_then(|ws| ws.get("package"))
    .and_then(|pkg| pkg.get("rust-version"))
    .and_then(|v| v.as_str());

  Ok(current != Some(desired.as_str()))
}

fn transitive_pins_host_manifest_path(ctx: &WorkspaceContext) -> RailResult<std::path::PathBuf> {
  let transitive_host_setting = ctx.config.as_ref().map(|c| &c.unify.transitive_host);
  let is_root_host = matches!(
    transitive_host_setting,
    None | Some(crate::config::TransitiveFeatureHost::Root)
  );

  // For virtual workspaces (no [package] section), we can't use the root as the transitive host
  // because virtual manifests can't have [dev-dependencies].
  if is_root_host && is_virtual_workspace(ctx.workspace_root()) {
    // Auto-select first workspace member as the host
    let members = ctx.graph.workspace_members();
    if members.is_empty() {
      return Err(RailError::with_help(
        "transitive_host = \"root\" is incompatible with virtual workspaces".to_string(),
        "Virtual workspaces cannot have [dev-dependencies]. Set transitive_host to a workspace member path in your rail.toml:\n  \
           [unify]\n  \
           transitive_host = \"crates/some-crate\""
          .to_string(),
      ));
    }

    let first_member = &members[0];
    if let Some(pkg) = ctx.cargo.get_package(first_member) {
      let member_path = pkg
        .manifest_path
        .parent()
        .ok_or_else(|| RailError::message(format!("Invalid manifest path: {}", pkg.manifest_path)))?;
      return Ok(member_path.join("Cargo.toml").into_std_path_buf());
    }

    return Err(RailError::message("Failed to find a suitable transitive host member"));
  }

  Ok(match transitive_host_setting {
    Some(crate::config::TransitiveFeatureHost::Path(p)) => ctx.workspace_root().join(p).join("Cargo.toml"),
    _ => ctx.workspace_root().join("Cargo.toml"),
  })
}

fn build_unify_mutation_plan(
  ctx: &WorkspaceContext,
  plan: &crate::cargo::UnificationPlan,
  msrv_write_needed: bool,
  backup_enabled: bool,
  no_report: bool,
  report_path: Option<&std::path::PathBuf>,
) -> RailResult<mutation::MutationPlan> {
  let mut actions = Vec::with_capacity(8); // Typically few distinct action types
  let mut risks = Vec::with_capacity(4);
  let mut trace = Vec::with_capacity(8);

  if !plan.workspace_deps.is_empty() {
    actions.push(MutationAction::new(
      "WRITE_WORKSPACE_DEPS",
      "Cargo.toml:[workspace.dependencies]",
      Some(format!("{} dependencies", plan.workspace_deps.len())),
    ));
  }

  if !plan.member_edits.is_empty() {
    actions.push(MutationAction::new(
      "APPLY_MEMBER_EDITS",
      "workspace member manifests",
      Some(format!("{} member(s)", plan.member_edits.len())),
    ));
  }

  if !plan.transitive_pins.is_empty() {
    actions.push(MutationAction::new(
      "APPLY_TRANSITIVE_PINS",
      "transitive host manifest",
      Some(format!("{} pinned dependencies", plan.transitive_pins.len())),
    ));
  }

  if msrv_write_needed {
    actions.push(MutationAction::new(
      "WRITE_WORKSPACE_MSRV",
      "Cargo.toml:[workspace.package.rust-version]",
      None,
    ));
  }

  if backup_enabled {
    actions.push(MutationAction::new("CREATE_BACKUP", "target/cargo-rail/backups", None));
  }

  if !no_report {
    let target = report_path
      .map(|p| p.display().to_string())
      .unwrap_or_else(|| "target/cargo-rail/unify-report.md".to_string());
    actions.push(MutationAction::new("WRITE_REPORT", target, None));
  }

  let error_count = plan
    .issues
    .iter()
    .filter(|issue| issue.severity == crate::cargo::IssueSeverity::Error)
    .count();
  if error_count > 0 {
    risks.push(MutationRisk::new(
      "BLOCKING_ISSUES",
      "high",
      format!("{} blocking issue(s) detected", error_count),
    ));
  }

  if !plan.transitive_pins.is_empty() {
    risks.push(MutationRisk::new(
      "TRANSITIVE_PIN_SIDE_EFFECTS",
      "medium",
      "transitive pinning mutates host dev-dependencies",
    ));
  }

  trace.push(MutationTrace::new(
    "UNIFY_ANALYSIS_COMPLETE",
    format!(
      "planned {} workspace dep(s), {} member edit(s), {} transitive pin(s)",
      plan.workspace_deps.len(),
      plan.member_edit_count(),
      plan.transitive_pins.len()
    ),
  ));
  if !plan.workspace_deps.is_empty() {
    trace.push(MutationTrace::new(
      "UNIFY_VERSION_DECISIONS",
      format!(
        "resolved versions for {} workspace dependency entries",
        plan.workspace_deps.len()
      ),
    ));
  }
  let feature_edit_count: usize = plan
    .member_edits
    .values()
    .flat_map(|edits| edits.iter())
    .filter(|edit| {
      matches!(
        edit,
        crate::cargo::MemberEdit::AddFeatures { .. } | crate::cargo::MemberEdit::RemoveFeature { .. }
      )
    })
    .count();
  if feature_edit_count > 0 {
    trace.push(MutationTrace::new(
      "UNIFY_FEATURE_DECISIONS",
      format!("planned {} feature-level member edit(s)", feature_edit_count),
    ));
  }
  if msrv_write_needed || plan.computed_msrv.is_some() {
    trace.push(MutationTrace::new(
      "UNIFY_MSRV_DECISIONS",
      format!(
        "msrv evaluated: computed={}, write_needed={}",
        plan.computed_msrv.is_some(),
        msrv_write_needed
      ),
    ));
  }
  if !plan.transitive_pins.is_empty() {
    trace.push(MutationTrace::new(
      "UNIFY_TRANSITIVE_DECISIONS",
      format!("planned {} transitive pin decision(s)", plan.transitive_pins.len()),
    ));
  }

  mutation::build_plan(ctx, "unify", actions, risks, trace)
}
