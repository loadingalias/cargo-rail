//! `cargo rail release` - Release automation

use crate::commands::common::{TextJsonOutputFormat, enforce_safety_gate};
use crate::config::{CommitPolicy, ReleaseForgeConfig};
use crate::error::{RailError, RailResult};
use crate::mutation::{
  self, ExpectedMutation, MutationAction, MutationEffect, MutationInput, MutationObject, MutationRisk, MutationTrace,
};
use crate::release::planner::{DependentPolicy, ReleasePlanner};
use crate::release::publisher::ReleasePublisher;
use crate::release::validator::ReleaseValidator;
use crate::release::version::BumpRequest;
use crate::workspace::WorkspaceContext;
use std::io::{self, IsTerminal};

/// Plan a release (check mode)
pub fn run_release_plan(
  ctx: &WorkspaceContext,
  crate_names: Option<Vec<String>>,
  bump: String,
  skip_publish: bool,
  skip_tag: bool,
  include_dependents: bool,
  format: TextJsonOutputFormat,
) -> RailResult<()> {
  let json = format.is_json();

  // JSON mode enables structured error output and suppresses progress
  if json {
    crate::output::set_json_mode(true);
  }

  let bump_request = bump.parse::<BumpRequest>()?;

  let workspace_members = ctx.graph.workspace_members();
  let validator = ReleaseValidator::new(ctx);

  let target_crates = crate_names;

  let config = ctx.config.as_ref().map(|c| &c.release);
  let release_config =
    config.ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;

  // Validate release config (tag format, changelog shape, release policies)
  let warnings = release_config.validate(workspace_members).map_err(RailError::Config)?;

  // Print warnings
  for warning in &warnings {
    crate::warn!("{}", warning);
  }

  let policy = dependent_policy(include_dependents);
  let planner = ReleasePlanner::new(ctx, release_config);
  let plan = planner.plan(target_crates, &bump_request, policy)?;
  let target_crates = plan.canonical_crate_order.clone();
  let mutation_plan = build_release_mutation_plan(ctx, &plan, skip_publish, skip_tag, false, release_config)?;
  let has_pending_changes = !mutation_plan.actions.is_empty();

  if !has_pending_changes {
    if json {
      let payload = serde_json::json!({
        "release_plan": plan,
        "mutation_plan": mutation_plan,
        "check": true,
      });
      let output = crate::output::machine_json_envelope("release", "check", "no_changes", 0, payload);
      let json_output = serde_json::to_string_pretty(&output)
        .map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))?;
      println!("{}", json_output);
    } else {
      println!("{}", plan.format_summary_with_flags(skip_publish, skip_tag));
      println!("\nNo release-worthy changes detected.");
    }

    return Ok(());
  }

  if release_config.require_clean {
    validator.validate(&target_crates, true)?;
  }

  // Validate changelog paths (catches path traversal issues early)
  validator.validate_changelog_paths(&target_crates, release_config)?;

  if json {
    let payload = serde_json::json!({
      "release_plan": plan,
      "mutation_plan": mutation_plan,
      "check": true,
    });
    let output = crate::output::machine_json_envelope("release", "check", "pending_changes", 1, payload);
    let json_output = serde_json::to_string_pretty(&output)
      .map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))?;
    println!("{}", json_output);
  } else {
    // Show publish_delay in the plan output
    println!("{}", plan.format_summary_with_flags(skip_publish, skip_tag));

    // Show additional config info
    if !skip_publish && plan.summary.crates_to_publish > 1 {
      println!("Publish delay: {}s between crates", release_config.publish_delay);
    }
    if release_config.create_github_release && !skip_tag {
      println!(
        "Forge releases: enabled ({})",
        release_forge_detail(release_config.forge)
      );
    }
    if release_config.sign_tags && !skip_tag {
      println!("Tag signing: enabled");
    }

    println!("\nChanges detected. Run without --check to apply.");
  }

  // Exit code 1 in --check mode indicates changes are pending (consistent across text/json)
  Err(RailError::CheckHasPendingChanges)
}

/// Arguments for release execution.
pub struct ReleasePublishArgs {
  /// Explicit crate names to release; ignored when `all` is true.
  pub crate_names: Option<Vec<String>>,
  /// Release all publishable workspace crates.
  pub all: bool,
  /// Version bump strategy.
  pub bump: String,
  /// Skip publishing crates to the registry.
  pub skip_publish: bool,
  /// Skip creating git tags.
  pub skip_tag: bool,
  /// Prepare a release PR branch without tags or publish.
  pub pr: bool,
  /// Expand explicit crate selection to include the full dependent closure.
  pub include_dependents: bool,
  /// Skip interactive confirmation prompts.
  pub yes: bool,
  /// Apply using a previously generated mutation plan.
  pub plan_path: Option<std::path::PathBuf>,
  /// Output format.
  pub format: TextJsonOutputFormat,
}

/// Execute a release
pub fn run_release_publish(ctx: &WorkspaceContext, args: ReleasePublishArgs) -> RailResult<()> {
  let json = args.format.is_json();
  if json {
    crate::output::set_json_mode(true);
  }

  let config = ctx.config.as_ref().map(|c| &c.release);
  let release_config =
    config.ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;

  let workspace_members = ctx.graph.workspace_members();
  let mut warnings = release_config.validate(workspace_members).map_err(RailError::Config)?;
  if !json {
    for warning in &warnings {
      crate::warn!("{}", warning);
    }
  }

  let validator = ReleaseValidator::new(ctx);

  let targets = if args.all {
    None
  } else if let Some(names) = args.crate_names {
    Some(names)
  } else {
    return Err(RailError::with_help(
      "must specify crate name(s) or --all",
      "cargo rail release my-crate\ncargo rail release --all",
    ));
  };

  let bump_request = args.bump.parse::<BumpRequest>()?;

  let policy = dependent_policy(args.include_dependents);
  let planner = ReleasePlanner::new(ctx, release_config);
  let plan = planner.plan(targets, &bump_request, policy)?;
  if plan.crates.is_empty() {
    if json {
      let payload = serde_json::json!({
        "release_plan": plan,
        "warnings": warnings,
      });
      let output = crate::output::machine_json_envelope("release", "apply", "no_changes", 0, payload);
      println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
      println!(
        "{}",
        plan.format_summary_with_flags(args.skip_publish || args.pr, args.skip_tag || args.pr)
      );
      println!("\nNo release-worthy changes detected.");
    }
    return Ok(());
  }

  let target_crates = plan.canonical_crate_order.clone();
  let effective_skip_publish = args.skip_publish || args.pr;
  let effective_skip_tag = args.skip_tag || args.pr;
  validator.validate(&target_crates, release_config.require_clean)?;

  // Validate branch state (detached HEAD = error, non-default branch = error unless --yes)
  if let Some(warning) = validator.validate_branch(args.yes)? {
    if json {
      warnings.push(warning);
    } else {
      crate::warn!("{}", warning);
    }
  }

  // Validate changelog paths
  validator.validate_changelog_paths(&target_crates, release_config)?;

  let expected_mutation_plan = build_release_mutation_plan(
    ctx,
    &plan,
    effective_skip_publish,
    effective_skip_tag,
    args.pr,
    release_config,
  )?;
  let mutation_plan = if let Some(path) = args.plan_path.as_ref() {
    let from_file = mutation::read_plan_file(path)?;
    if !from_file.operation_id.starts_with("release-") {
      return Err(RailError::with_help(
        format!("plan '{}' is not a release plan", path.display()),
        "generate a release plan using 'cargo rail release run --check --json'".to_string(),
      ));
    }
    mutation::validate_pre_apply_with_allowed_paths(ctx, &from_file, std::slice::from_ref(path))?;
    mutation::validate_requested_operation(&from_file, &expected_mutation_plan)?;
    from_file
  } else {
    mutation::validate_pre_apply(ctx, &expected_mutation_plan)?;
    expected_mutation_plan
  };

  let plan_control_paths = args.plan_path.iter().cloned().collect::<Vec<_>>();
  let allowed_unstaged_paths = plan_control_paths
    .iter()
    .cloned()
    .chain(mutation::declared_input_paths(&mutation_plan))
    .collect::<Vec<_>>();
  mutation::validate_changed_paths_with_allowed_paths(ctx, &mutation_plan, &allowed_unstaged_paths)?;

  if !json {
    println!(
      "{}",
      plan.format_summary_with_flags(effective_skip_publish, effective_skip_tag)
    );
  }

  enforce_safety_gate(
    if args.pr { "release PR" } else { "release apply" },
    args.yes,
    args.plan_path.as_deref(),
    io::stdin().is_terminal() && !json,
  )?;

  // Skip confirmation if --yes flag is set
  if !args.yes && io::stdin().is_terminal() && !json {
    println!("\nthis will:");
    println!("  - modify Cargo.toml (version bumps)");
    println!("  - update changelogs");
    if args.pr {
      println!("  - create and push a release PR branch");
    } else {
      println!("  - create git commits");
    }
    if !effective_skip_tag {
      println!("  - create {} tag(s)", plan.crates.len());
    }
    if !effective_skip_publish {
      println!("  - publish to crates.io (irreversible)");
    }

    if !crate::utils::prompt_for_confirmation("\nproceed? [Enter/Ctrl+C]")? {
      println!("cancelled");
      return Ok(());
    }
  }

  validator.validate_apply_preconditions(
    &plan,
    effective_skip_publish,
    effective_skip_tag,
    release_config.require_clean,
    release_config.require_release_notes,
  )?;
  mutation::validate_pre_apply_with_allowed_paths(ctx, &mutation_plan, &plan_control_paths)?;
  mutation::validate_changed_paths_with_allowed_paths(ctx, &mutation_plan, &allowed_unstaged_paths)?;
  let plan_receipt = mutation::write_receipt(
    ctx.workspace_root(),
    "release",
    "plan",
    "planned",
    mutation_plan.clone(),
    vec![MutationTrace::new(
      "RELEASE_PLAN_CREATED",
      format!("planned release for {} crate(s)", plan.summary.total_crates),
    )],
  )?;
  crate::progress!("receipt: {}", plan_receipt.display());

  let publisher = ReleasePublisher::new(ctx, release_config);
  let planned_paths = mutation::expected_paths(&mutation_plan);
  if args.pr {
    publisher.execute_pr(&plan, &planned_paths, &allowed_unstaged_paths)?;
  } else {
    publisher.execute(
      &plan,
      args.skip_publish,
      args.skip_tag,
      &planned_paths,
      &allowed_unstaged_paths,
    )?;
  }

  let resulting_objects = collect_release_objects(ctx, &mutation_plan, &plan, effective_skip_tag)?;

  let apply_receipt = mutation::write_receipt_with_objects(
    ctx.workspace_root(),
    "release",
    "apply",
    "applied",
    mutation_plan,
    vec![
      MutationTrace::new("RELEASE_APPLY_STARTED", "started release apply"),
      MutationTrace::new("RELEASE_APPLY_COMPLETED", "completed release apply"),
    ],
    resulting_objects,
  )?;
  crate::progress!("receipt: {}", apply_receipt.display());

  if json {
    let payload = serde_json::json!({
      "release_plan": plan,
      "warnings": warnings,
      "plan_receipt": plan_receipt,
      "apply_receipt": apply_receipt,
      "release_pr": args.pr,
    });
    let output = crate::output::machine_json_envelope(
      "release",
      if args.pr { "release_pr" } else { "apply" },
      "success",
      0,
      payload,
    );
    println!("{}", serde_json::to_string_pretty(&output)?);
  }

  Ok(())
}

/// Validate release readiness
pub fn run_release_check(
  ctx: &WorkspaceContext,
  crate_names: Option<Vec<String>>,
  all: bool,
  extended: bool,
  include_dependents: bool,
  format: TextJsonOutputFormat,
) -> RailResult<()> {
  let json = format.is_json();

  // JSON mode enables structured error output and suppresses progress
  if json {
    crate::output::set_json_mode(true);
  }

  let config = ctx.config.as_ref().map(|c| &c.release);
  let release_config =
    config.ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;

  let workspace_members = ctx.graph.workspace_members();
  for warning in release_config.validate(workspace_members).map_err(RailError::Config)? {
    if !json {
      crate::warn!("{}", warning);
    }
  }

  let validator = ReleaseValidator::new(ctx);

  // Track skipped crates for reporting
  let mut skipped_crates: Vec<(String, String)> = Vec::with_capacity(8);

  let mut target_crates = if all {
    // Filter to only publishable crates when using --all
    let (publishable, skipped) = validator.publishable_members();
    skipped_crates = skipped;

    if publishable.is_empty() {
      return Err(RailError::with_help(
        "no publishable crates found",
        "All workspace crates have publish = false. Check Cargo.toml or rail.toml settings.",
      ));
    }

    publishable
  } else if let Some(names) = crate_names {
    names
  } else {
    return Err(RailError::with_help(
      "must specify crate name(s) or --all",
      "cargo rail release check my-crate\ncargo rail release check --all",
    ));
  };

  // Report skipped crates (non-JSON mode only, before validation output)
  if !skipped_crates.is_empty() && !json {
    crate::status!("skipped {} crate(s) (not publishable):", skipped_crates.len());
    for (name, reason) in &skipped_crates {
      crate::status!("  {}: {}", name, reason);
    }
    crate::status!("");
  }

  if !all {
    let policy = dependent_policy(include_dependents);
    let planner = ReleasePlanner::new(ctx, release_config);
    target_crates = planner.resolve_targets(Some(target_crates), policy)?;
  }
  validator.validate(&target_crates, release_config.require_clean)?;

  // Validate changelog paths
  validator.validate_changelog_paths(&target_crates, release_config)?;

  // One attribution pass covers commit diagnostics and change-file coverage.
  let insights = ReleasePlanner::new(ctx, release_config).release_check_insights(&target_crates)?;
  let commit_diagnostics = insights.commit_diagnostics;
  let missing_change_files = insights.missing_change_files;
  let shallow_repository = insights.shallow_repository;
  let has_commit_diagnostic_failures =
    release_config.unconventional_commits == CommitPolicy::Deny && !commit_diagnostics.is_empty();
  let has_change_file_failures = !missing_change_files.is_empty();
  let has_shallow_failures = shallow_repository;

  if !json && shallow_repository {
    println!("\nrelease history:");
    println!("  shallow clone: fetch tags: git fetch --unshallow --tags, or set fetch-depth: 0");
  }

  if !json && !commit_diagnostics.is_empty() {
    let label = if has_commit_diagnostic_failures {
      "commit diagnostics failed"
    } else {
      "commit diagnostics"
    };
    println!("\n{}:", label);
    for (crate_name, diagnostics) in &commit_diagnostics {
      for diagnostic in diagnostics {
        println!("  {}: {}", crate_name, diagnostic.describe());
      }
    }
  }
  if !json && !missing_change_files.is_empty() {
    println!("\nmissing change files:");
    for crate_name in &missing_change_files {
      println!(
        "  {}: code changes require {} coverage",
        crate_name, release_config.change_dir
      );
    }
  }

  let mut results = Vec::with_capacity(target_crates.len());
  for crate_name in &target_crates {
    // For explicitly named crates, check publishability and report
    // (for --all, we already filtered, so this is a no-op)
    if !validator.is_publishable(crate_name) {
      if !json {
        let reason = validator
          .unpublishable_reason(crate_name)
          .unwrap_or_else(|| "unknown".to_string());
        println!("{}: not publishable ({})", crate_name, reason);
      }
      continue;
    }

    validator.validate_publishable(crate_name)?;
    results.push(crate_name.clone());
    if !json {
      println!("{}: ready", crate_name);
    }
  }

  // Extended validation: cargo publish --dry-run and MSRV check
  let mut extended_results = Vec::with_capacity(target_crates.len());
  let mut has_extended_failures = false;

  if extended {
    if !json {
      println!("\nrunning extended checks...");
    }

    let ext_results = validator.validate_extended(&target_crates, release_config.semver_check);

    for (crate_name, checks) in ext_results {
      let mut crate_checks = Vec::with_capacity(checks.len());

      for check in checks {
        if check.passed {
          if !json {
            println!(
              "  {}: {} - {}",
              crate_name,
              check.check_name,
              check.details.as_deref().unwrap_or("ok")
            );
          }
        } else {
          has_extended_failures = true;
          if !json {
            crate::error!(
              "  {}: {} - FAILED: {}",
              crate_name,
              check.check_name,
              check.error.as_deref().unwrap_or("unknown error")
            );
          }
        }

        crate_checks.push(serde_json::json!({
          "check": check.check_name,
          "passed": check.passed,
          "details": check.details,
          "error": check.error
        }));
      }

      extended_results.push(serde_json::json!({
        "crate": crate_name,
        "checks": crate_checks
      }));
    }
  }

  if json {
    let mut payload = serde_json::json!({
      "action": "check",
      "status": if has_extended_failures || has_commit_diagnostic_failures || has_change_file_failures || has_shallow_failures { "failed" } else { "passed" },
      "crates": results,
      "count": results.len()
    });

    // Include skipped crates in JSON output
    if !skipped_crates.is_empty() {
      payload["skipped"] = serde_json::json!(
        skipped_crates
          .iter()
          .map(|(name, reason)| serde_json::json!({"crate": name, "reason": reason}))
          .collect::<Vec<_>>()
      );
    }

    if extended {
      payload["extended"] = serde_json::json!(extended_results);
    }

    if !commit_diagnostics.is_empty() {
      payload["commit_diagnostics"] = serde_json::json!(
        commit_diagnostics
          .iter()
          .map(|(name, diagnostics)| serde_json::json!({"crate": name, "diagnostics": diagnostics}))
          .collect::<Vec<_>>()
      );
    }
    if !missing_change_files.is_empty() {
      payload["missing_change_files"] = serde_json::json!(missing_change_files);
    }
    if shallow_repository {
      payload["release_history"] = serde_json::json!({
        "shallow_repository": true,
        "help": "fetch tags: git fetch --unshallow --tags, or set fetch-depth: 0"
      });
    }

    let exit_code =
      if has_extended_failures || has_commit_diagnostic_failures || has_change_file_failures || has_shallow_failures {
        2
      } else {
        0
      };
    let result =
      if has_extended_failures || has_commit_diagnostic_failures || has_change_file_failures || has_shallow_failures {
        "failed"
      } else {
        "success"
      };
    let output = crate::output::machine_json_envelope("release", "validate", result, exit_code, payload);

    println!(
      "{}",
      serde_json::to_string_pretty(&output).map_err(|e| RailError::message(format!("JSON error: {}", e)))?
    );
  } else if has_extended_failures || has_commit_diagnostic_failures || has_change_file_failures || has_shallow_failures
  {
    return Err(RailError::message(if has_shallow_failures {
      "release history check failed"
    } else if has_change_file_failures {
      "change file coverage failed"
    } else if has_commit_diagnostic_failures {
      "commit diagnostics failed"
    } else {
      "extended validation failed"
    }));
  } else {
    println!("\nall checks passed");
  }

  if (has_extended_failures || has_commit_diagnostic_failures || has_change_file_failures || has_shallow_failures)
    && json
  {
    return Err(RailError::ExitWithCode { code: 2 });
  }

  Ok(())
}

/// Finalize a merged release PR by tagging, pushing, publishing, and creating forge releases.
#[allow(clippy::too_many_arguments)]
pub fn run_release_finalize(
  ctx: &WorkspaceContext,
  crate_names: Option<Vec<String>>,
  all: bool,
  skip_publish: bool,
  skip_tag: bool,
  include_dependents: bool,
  yes: bool,
  format: TextJsonOutputFormat,
) -> RailResult<()> {
  let json = format.is_json();
  if json {
    crate::output::set_json_mode(true);
  }

  let release_config = ctx
    .config
    .as_ref()
    .map(|config| &config.release)
    .ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;

  let workspace_members = ctx.graph.workspace_members();
  let mut warnings = release_config.validate(workspace_members).map_err(RailError::Config)?;
  if !json {
    for warning in &warnings {
      crate::warn!("{}", warning);
    }
  }

  let targets = if all {
    None
  } else if crate_names.is_some() {
    crate_names
  } else {
    return Err(RailError::with_help(
      "must specify crate name(s) or --all",
      "cargo rail release finalize my-crate\ncargo rail release finalize --all",
    ));
  };
  let planner = ReleasePlanner::new(ctx, release_config);
  let plan = planner.finalize_plan(targets, dependent_policy(include_dependents))?;
  let target_crates = plan.canonical_crate_order.clone();
  let validator = ReleaseValidator::new(ctx);
  validator.validate(&target_crates, release_config.require_clean)?;
  if let Some(warning) = validator.validate_branch(yes)? {
    if json {
      warnings.push(warning);
    } else {
      crate::warn!("{}", warning);
    }
  }

  if !json {
    println!("{}", plan.format_summary_with_flags(skip_publish, skip_tag));
  }

  enforce_safety_gate("release finalize", yes, None, io::stdin().is_terminal() && !json)?;
  let publisher = ReleasePublisher::new(ctx, release_config);
  publisher.execute_finalize(&plan, skip_publish, skip_tag)?;

  if json {
    let payload = serde_json::json!({
      "release_plan": plan,
      "warnings": warnings,
      "finalize": true,
    });
    let output = crate::output::machine_json_envelope("release", "finalize", "success", 0, payload);
    println!("{}", serde_json::to_string_pretty(&output)?);
  }

  Ok(())
}

fn dependent_policy(include_dependents: bool) -> DependentPolicy {
  if include_dependents {
    DependentPolicy::IncludeDependents
  } else {
    DependentPolicy::RejectPartialClosure
  }
}

/// Resume an interrupted release from durable state.
pub fn run_release_resume(ctx: &WorkspaceContext, state: &std::path::Path) -> RailResult<()> {
  let release_config = ctx
    .config
    .as_ref()
    .map(|config| &config.release)
    .ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;
  ReleasePublisher::new(ctx, release_config).resume(state)
}

/// Abort an active release before any external side effect has occurred.
pub fn run_release_abort(ctx: &WorkspaceContext, state: &std::path::Path, yes: bool) -> RailResult<()> {
  enforce_safety_gate("release abort", yes, None, io::stdin().is_terminal())?;
  let release_config = ctx
    .config
    .as_ref()
    .map(|config| &config.release)
    .ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;
  ReleasePublisher::new(ctx, release_config).abort(state)
}

fn build_release_mutation_plan(
  ctx: &WorkspaceContext,
  plan: &crate::release::planner::ReleasePlan,
  skip_publish: bool,
  skip_tag: bool,
  pr: bool,
  release_config: &crate::config::ReleaseConfig,
) -> RailResult<mutation::MutationPlan> {
  let mut actions = Vec::with_capacity(plan.crates.len() * 7 + plan.change_files_to_delete.len() + 3);

  for (index, crate_plan) in plan.crates.iter().enumerate() {
    actions.push(
      MutationAction::new(
        "BUMP_VERSION",
        crate_plan.name.clone(),
        Some(format!("{} -> {}", crate_plan.current_version, crate_plan.new_version)),
      )
      .with_payload(serde_json::json!({
        "crate": crate_plan.name,
        "from": crate_plan.current_version,
        "to": crate_plan.new_version,
      }))
      .with_mutations(vec![release_mutation(
        ctx,
        &crate_plan.manifest_path,
        MutationEffect::Write,
      )?]),
    );
    if !crate_plan.affected_dependents.is_empty() {
      let mut mutations = vec![release_mutation(
        ctx,
        &ctx.workspace_root().join("Cargo.toml"),
        MutationEffect::Write,
      )?];
      for dependent in &crate_plan.affected_dependents {
        let package = ctx.cargo.get_package(dependent).ok_or_else(|| {
          RailError::message(format!(
            "release plan references unknown dependent crate '{}'",
            dependent
          ))
        })?;
        mutations.push(release_mutation(
          ctx,
          &package.manifest_path.clone().into_std_path_buf(),
          MutationEffect::Write,
        )?);
      }
      actions.push(
        MutationAction::new(
          "UPDATE_DEPENDENTS",
          crate_plan.name.clone(),
          Some(crate_plan.affected_dependents.join(",")),
        )
        .with_payload(serde_json::json!({
          "dependency": crate_plan.name,
          "version": crate_plan.new_version,
          "dependents": crate_plan.affected_dependents,
        }))
        .with_mutations(mutations),
      );
    }
    if crate_plan.generate_changelog && !crate_plan.changelog_body.trim().is_empty() {
      actions.push(
        MutationAction::new(
          "UPDATE_CHANGELOG",
          crate_plan.changelog_path.display().to_string(),
          Some(format!("crate={}", crate_plan.name)),
        )
        .with_payload(serde_json::json!({
          "crate": crate_plan.name,
          "version": crate_plan.new_version,
          "body": crate_plan.changelog_body,
        }))
        .with_mutations(vec![release_mutation(
          ctx,
          &crate_plan.changelog_path,
          MutationEffect::Write,
        )?]),
      );
    }
    if index == 0 {
      for path in &plan.change_files_to_delete {
        actions.push(
          MutationAction::new("DELETE_CHANGE_FILE", path.display().to_string(), None)
            .with_payload(serde_json::json!({ "path": path }))
            .with_mutations(vec![release_mutation(ctx, path, MutationEffect::Delete)?]),
        );
      }
    }
    actions.push(
      MutationAction::new(
        "UPDATE_LOCKFILE",
        "Cargo.lock",
        Some(format!("package={}", crate_plan.name)),
      )
      .with_payload(serde_json::json!({ "package": crate_plan.name }))
      .with_mutations(vec![release_mutation(
        ctx,
        &ctx.workspace_root().join("Cargo.lock"),
        MutationEffect::Write,
      )?]),
    );
    if !pr {
      actions.push(
        MutationAction::new(
          "COMMIT_RELEASE",
          crate_plan.name.clone(),
          Some(format!("tag={}", crate_plan.tag_name)),
        )
        .with_payload(serde_json::json!({
          "message": format!("chore(release): {} v{}", crate_plan.name, crate_plan.new_version),
        })),
      );
      if !skip_tag {
        actions.push(
          MutationAction::new(
            "CREATE_TAG",
            crate_plan.tag_name.clone(),
            Some(format!("crate={}", crate_plan.name)),
          )
          .with_payload(serde_json::json!({
            "crate": crate_plan.name,
            "tag": crate_plan.tag_name,
            "signed": release_config.sign_tags,
          })),
        );
      }
    }
  }

  if pr {
    actions.push(
      MutationAction::new("COMMIT_RELEASE_PR", "release-pr", None)
        .with_payload(serde_json::json!({ "crates": plan.canonical_crate_order })),
    );
    actions.push(
      MutationAction::new("PUSH_RELEASE_PR", "origin", None).with_payload(serde_json::json!({ "remote": "origin" })),
    );
    actions.push(MutationAction::new("OPEN_RELEASE_PR", "origin", None));
  } else {
    if release_config.push {
      actions.push(
        MutationAction::new("PUSH_RELEASE_REFS", "origin", None).with_payload(serde_json::json!({
          "remote": "origin",
          "tags": if skip_tag {
            Vec::<String>::new()
          } else {
            plan.crates.iter().map(|crate_plan| crate_plan.tag_name.clone()).collect()
          },
        })),
      );
    }
    if release_config.create_github_release && !skip_tag {
      for crate_plan in &plan.crates {
        actions.push(
          MutationAction::new("CREATE_FORGE_RELEASE", crate_plan.tag_name.clone(), None).with_payload(
            serde_json::json!({ "forge": release_forge_detail(release_config.forge), "tag": crate_plan.tag_name }),
          ),
        );
      }
    }
    if !skip_publish {
      for crate_plan in plan.crates.iter().filter(|crate_plan| crate_plan.publish) {
        actions.push(
          MutationAction::new("PUBLISH_CRATE", crate_plan.name.clone(), None)
            .with_payload(serde_json::json!({ "crate": crate_plan.name, "registry": "crates-io" })),
        );
      }
    }
    if release_config.create_github_release && !skip_tag {
      for crate_plan in &plan.crates {
        actions.push(
          MutationAction::new("PUBLISH_FORGE_RELEASE", crate_plan.tag_name.clone(), None).with_payload(
            serde_json::json!({ "forge": release_forge_detail(release_config.forge), "tag": crate_plan.tag_name }),
          ),
        );
      }
    }
  }

  let mut risks = Vec::new();
  if !actions.is_empty() && !skip_publish {
    risks.push(MutationRisk::new(
      "CRATES_IO_PUBLISH",
      "high",
      "publishing to crates.io is irreversible",
    ));
  }
  if !actions.is_empty() {
    risks.push(MutationRisk::new(
      "REJECT_UNPLANNED_WORKTREE_CHANGES",
      "low",
      "release rejects worktree changes outside explicitly planned paths",
    ));
  }
  if !actions.is_empty() && (release_config.push || pr) {
    risks.push(MutationRisk::new(
      "REMOTE_PUSH",
      "medium",
      "release commits and tags are pushed before public publishing",
    ));
  }

  let trace = vec![MutationTrace::new(
    "RELEASE_PLAN_RESOLVED",
    format!(
      "resolved {} crate(s), {} publish candidate(s), skip_tag={}, skip_publish={}",
      plan.summary.total_crates, plan.summary.crates_to_publish, skip_tag, skip_publish
    ),
  )];

  mutation::build_plan_with_inputs(
    ctx,
    "release",
    actions,
    release_declared_inputs(ctx, plan, release_config)?,
    risks,
    trace,
  )
}

fn release_declared_inputs(
  ctx: &WorkspaceContext,
  plan: &crate::release::planner::ReleasePlan,
  release_config: &crate::config::ReleaseConfig,
) -> RailResult<Vec<MutationInput>> {
  let git = ctx.git()?.git();
  let git_root = &git.worktree_root;
  let mut paths = Vec::new();
  if let Some(config_path) = crate::config::RailConfig::find_config_path(ctx.workspace_root()) {
    paths.push(config_path);
  }

  let notes_dir = ctx.workspace_root().join(&release_config.release_notes_dir);
  for crate_plan in &plan.crates {
    let version_path = notes_dir.join(format!("v{}.md", crate_plan.new_version));
    let tag_path = notes_dir.join(format!("{}.md", crate_plan.tag_name));
    if version_path.exists() {
      paths.push(version_path);
    } else if tag_path.exists() {
      paths.push(tag_path);
    }
  }

  paths.sort();
  paths.dedup();
  paths
    .into_iter()
    .map(|path| {
      let relative = path.strip_prefix(git_root).map_err(|_| {
        RailError::message(format!(
          "release input '{}' is outside git worktree '{}'",
          path.display(),
          git_root.display()
        ))
      })?;
      MutationInput::capture(git, git_root, relative.to_path_buf())
    })
    .collect()
}

fn release_mutation(
  ctx: &WorkspaceContext,
  path: &std::path::Path,
  effect: MutationEffect,
) -> RailResult<ExpectedMutation> {
  let git_root = &ctx.git()?.git().worktree_root;
  let absolute = if path.is_absolute() {
    path.to_path_buf()
  } else {
    ctx.workspace_root().join(path)
  };
  let relative = absolute.strip_prefix(git_root).map_err(|_| {
    RailError::message(format!(
      "release path '{}' is outside git worktree '{}'",
      absolute.display(),
      git_root.display()
    ))
  })?;
  Ok(ExpectedMutation::capture(git_root, relative.to_path_buf(), effect))
}

fn collect_release_objects(
  ctx: &WorkspaceContext,
  mutation_plan: &mutation::MutationPlan,
  release_plan: &crate::release::planner::ReleasePlan,
  skip_tag: bool,
) -> RailResult<Vec<MutationObject>> {
  let git = ctx.git()?.git();
  let current_head = git.head_commit()?;
  let mut objects = git
    .run_git_stdout(&[
      "rev-list",
      "--reverse",
      &format!("{}..{}", mutation_plan.pre_apply.git_head, current_head),
    ])?
    .lines()
    .enumerate()
    .map(|(index, oid)| MutationObject {
      kind: "commit".to_string(),
      name: format!("release-commit-{}", index + 1),
      oid: oid.to_string(),
    })
    .collect::<Vec<_>>();

  if !skip_tag {
    for crate_plan in &release_plan.crates {
      objects.push(MutationObject {
        kind: "tag".to_string(),
        name: crate_plan.tag_name.clone(),
        oid: git.run_git_stdout(&["rev-parse", &format!("refs/tags/{}", crate_plan.tag_name)])?,
      });
    }
  }
  Ok(objects)
}

fn release_forge_detail(forge: ReleaseForgeConfig) -> &'static str {
  match forge {
    ReleaseForgeConfig::Auto => "auto",
    ReleaseForgeConfig::Github => "github",
    ReleaseForgeConfig::Gitlab => "gitlab",
  }
}

/// Initialize release configuration
pub fn run_release_init(ctx: &WorkspaceContext, crates: Option<Vec<String>>, check: bool) -> RailResult<()> {
  use crate::config::{ChangelogConfig, CrateReleaseConfig, RailConfig};
  use std::fs;

  let requested_crates = crates;

  let members = ctx.cargo.workspace_members();
  let workspace_root = ctx.workspace_root();

  let target_crates: Vec<_> = members
    .iter()
    .filter(|pkg| {
      requested_crates
        .as_ref()
        .map(|requested| requested.contains(&pkg.name))
        .unwrap_or(true)
    })
    .collect();

  if target_crates.is_empty() {
    if let Some(requested) = requested_crates {
      return Err(crate::error::RailError::message(format!(
        "no matching crates: {}",
        requested.join(", ")
      )));
    } else {
      return Err(crate::error::RailError::message("no workspace members found"));
    }
  }

  let existing_config = RailConfig::load(workspace_root).ok();

  let mut config = existing_config.unwrap_or_else(|| RailConfig {
    targets: vec![],
    unify: crate::config::UnifyConfig::default(),
    release: crate::config::ReleaseConfig::default(),
    change_detection: crate::config::ChangeDetectionConfig::default(),
    run: crate::config::RunConfig::default(),
    crates: Default::default(),
  });

  let mut new_crates = Vec::with_capacity(target_crates.len());
  let mut existing_crates = Vec::with_capacity(target_crates.len());

  for pkg in target_crates {
    if config.crates.contains_key(pkg.name.as_str()) && config.crates[pkg.name.as_str()].release.is_some() {
      existing_crates.push(pkg.name.clone());
      continue;
    }

    new_crates.push(pkg.name.clone());

    let Some(crate_dir) = pkg.manifest_path.parent() else {
      // manifest_path always has a parent directory - skip if somehow malformed
      continue;
    };
    let changelog_path = crate::utils::detect_crate_changelog(crate_dir);

    let crate_config = config.crates.entry(pkg.name.to_string()).or_default();

    crate_config.release = Some(CrateReleaseConfig {
      publish: crate::workspace::CargoState::is_package_publishable(pkg),
    });

    if let Some(path) = changelog_path {
      crate_config.changelog = Some(ChangelogConfig {
        path: Some(path),
        skip: false,
        ..ChangelogConfig::default()
      });
    }
  }

  if !existing_crates.is_empty() {
    println!("skipping {} with existing config:", existing_crates.len());
    for name in &existing_crates {
      println!("  {}", name);
    }
  }

  if new_crates.is_empty() {
    println!("all crates already configured");
    return Ok(());
  }

  println!("adding release config for {} crate(s):", new_crates.len());
  for name in &new_crates {
    println!("  {}", name);
  }
  print_changelog_migration_hint(workspace_root);

  let config_toml = toml_edit::ser::to_string_pretty(&config)
    .map_err(|e| crate::error::RailError::message(format!("config serialization failed: {}", e)))?;

  if check {
    println!("\n{}", config_toml);
  } else {
    let config_path =
      RailConfig::find_config_path(workspace_root).unwrap_or_else(|| workspace_root.join(".config/rail.toml"));

    if let Some(parent) = config_path.parent() {
      fs::create_dir_all(parent)?;
    }

    fs::write(&config_path, config_toml)?;
    println!("updated: {}", config_path.display());
  }

  Ok(())
}

fn print_changelog_migration_hint(workspace_root: &std::path::Path) {
  let existing: Vec<_> = ["cliff.toml", "release-plz.toml"]
    .into_iter()
    .filter(|path| workspace_root.join(path).exists())
    .collect();
  if existing.is_empty() {
    return;
  }

  println!("\nfound existing changelog/release config: {}", existing.join(", "));
  println!("migration guide: docs/migrate-git-cliff.md");
}
