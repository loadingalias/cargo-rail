//! `cargo rail release` - Release automation

use crate::commands::common::{OutputFormat, enforce_safety_gate};
use crate::error::{RailError, RailResult};
use crate::mutation::{self, MutationAction, MutationRisk, MutationTrace};
use crate::release::planner::{DependentPolicy, ReleasePlanner};
use crate::release::publisher::ReleasePublisher;
use crate::release::validator::ReleaseValidator;
use crate::release::version::BumpType;
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
  format: OutputFormat,
) -> RailResult<()> {
  let json = format.is_json();

  // JSON mode enables structured error output and suppresses progress
  if json {
    crate::output::set_json_mode(true);
  }

  let bump_type = bump.parse::<BumpType>()?;

  let workspace_members = ctx.graph.workspace_members();
  let validator = ReleaseValidator::new(ctx);

  let target_crates = crate_names;

  let config = ctx.config.as_ref().map(|c| &c.release);
  let release_config =
    config.ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;

  // Validate release config (tag_format, skip_changelog_for)
  let warnings = release_config.validate(workspace_members).map_err(RailError::Config)?;

  // Print warnings
  for warning in &warnings {
    crate::warn!("{}", warning);
  }

  let policy = dependent_policy(include_dependents);
  let planner = ReleasePlanner::new(ctx, release_config);
  let plan = planner.plan(target_crates, &bump_type, policy)?;
  let target_crates = plan.canonical_crate_order.clone();

  if release_config.require_clean {
    validator.validate(&target_crates, true)?;
  }

  // Validate changelog paths (catches path traversal issues early)
  validator.validate_changelog_paths(&target_crates, release_config)?;

  let mutation_plan = build_release_mutation_plan(ctx, &plan, skip_publish, skip_tag, release_config)?;

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
      println!("GitHub releases: enabled");
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
  /// Expand explicit crate selection to include the full dependent closure.
  pub include_dependents: bool,
  /// Skip interactive confirmation prompts.
  pub yes: bool,
  /// Apply using a previously generated mutation plan.
  pub plan_path: Option<std::path::PathBuf>,
}

/// Execute a release
pub fn run_release_publish(ctx: &WorkspaceContext, args: ReleasePublishArgs) -> RailResult<()> {
  let config = ctx.config.as_ref().map(|c| &c.release);
  let release_config =
    config.ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;

  let workspace_members = ctx.graph.workspace_members();
  for warning in release_config.validate(workspace_members).map_err(RailError::Config)? {
    crate::warn!("{}", warning);
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

  let bump_type = args.bump.parse::<BumpType>()?;

  let policy = dependent_policy(args.include_dependents);
  let planner = ReleasePlanner::new(ctx, release_config);
  let plan = planner.plan(targets, &bump_type, policy)?;
  let target_crates = plan.canonical_crate_order.clone();
  validator.validate(&target_crates, release_config.require_clean)?;

  // Validate branch state (detached HEAD = error, non-default branch = error unless --yes)
  if let Some(warning) = validator.validate_branch(args.yes)? {
    crate::warn!("{}", warning);
  }

  // Validate changelog paths
  validator.validate_changelog_paths(&target_crates, release_config)?;

  let expected_mutation_plan =
    build_release_mutation_plan(ctx, &plan, args.skip_publish, args.skip_tag, release_config)?;
  let mutation_plan = if let Some(path) = args.plan_path.as_ref() {
    let from_file = mutation::read_plan_file(path)?;
    if !from_file.operation_id.starts_with("release-") {
      return Err(RailError::with_help(
        format!("plan '{}' is not a release plan", path.display()),
        "generate a release plan using 'cargo rail release run --check --json'".to_string(),
      ));
    }
    mutation::validate_pre_apply(ctx, &from_file)?;
    if from_file.inputs_fingerprint != expected_mutation_plan.inputs_fingerprint {
      return Err(RailError::with_help(
        "provided release plan does not match current requested operation",
        "regenerate plan and re-run with --plan",
      ));
    }
    from_file
  } else {
    mutation::validate_pre_apply(ctx, &expected_mutation_plan)?;
    expected_mutation_plan
  };

  println!("{}", plan.format_summary_with_flags(args.skip_publish, args.skip_tag));

  enforce_safety_gate(
    "release apply",
    args.yes,
    args.plan_path.as_deref(),
    io::stdin().is_terminal(),
  )?;

  // Skip confirmation if --yes flag is set
  if !args.yes && io::stdin().is_terminal() {
    println!("\nthis will:");
    println!("  - modify Cargo.toml (version bumps)");
    println!("  - update changelogs");
    println!("  - create git commits");
    if !args.skip_tag {
      println!("  - create {} tag(s)", plan.crates.len());
    }
    if !args.skip_publish {
      println!("  - publish to crates.io (irreversible)");
    }

    if !crate::utils::prompt_for_confirmation("\nproceed? [Enter/Ctrl+C]")? {
      println!("cancelled");
      return Ok(());
    }
  }

  validator.validate_apply_preconditions(
    &plan,
    args.skip_publish,
    args.skip_tag,
    release_config.require_clean,
    release_config.require_release_notes,
  )?;
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
  publisher.execute(&plan, args.skip_publish, args.skip_tag)?;

  let apply_receipt = mutation::write_receipt(
    ctx.workspace_root(),
    "release",
    "apply",
    "applied",
    mutation_plan,
    vec![
      MutationTrace::new("RELEASE_APPLY_STARTED", "started release apply"),
      MutationTrace::new("RELEASE_APPLY_COMPLETED", "completed release apply"),
    ],
  )?;
  crate::progress!("receipt: {}", apply_receipt.display());

  Ok(())
}

/// Validate release readiness
pub fn run_release_check(
  ctx: &WorkspaceContext,
  crate_names: Option<Vec<String>>,
  all: bool,
  extended: bool,
  include_dependents: bool,
  format: OutputFormat,
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

    let ext_results = validator.validate_extended(&target_crates);

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
      "status": if has_extended_failures { "failed" } else { "passed" },
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

    let exit_code = if has_extended_failures { 2 } else { 0 };
    let result = if has_extended_failures { "failed" } else { "success" };
    let output = crate::output::machine_json_envelope("release", "validate", result, exit_code, payload);

    println!(
      "{}",
      serde_json::to_string_pretty(&output).map_err(|e| RailError::message(format!("JSON error: {}", e)))?
    );
  } else if has_extended_failures {
    return Err(RailError::message("extended validation failed"));
  } else {
    println!("\nall checks passed");
  }

  if has_extended_failures && json {
    return Err(RailError::ExitWithCode { code: 2 });
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

fn build_release_mutation_plan(
  ctx: &WorkspaceContext,
  plan: &crate::release::planner::ReleasePlan,
  skip_publish: bool,
  skip_tag: bool,
  release_config: &crate::config::ReleaseConfig,
) -> RailResult<mutation::MutationPlan> {
  // Pre-allocate for expected actions: ~5 per crate (bump, changelog, commit, tag, publish)
  let mut actions = Vec::with_capacity(plan.crates.len() * 5);
  let mut sorted = plan.crates.clone();
  sorted.sort_by(|a, b| a.name.cmp(&b.name));

  for crate_plan in &sorted {
    actions.push(MutationAction::new(
      "BUMP_VERSION",
      crate_plan.name.clone(),
      Some(format!("{} -> {}", crate_plan.current_version, crate_plan.new_version)),
    ));
    actions.push(MutationAction::new(
      "UPDATE_CHANGELOG",
      crate_plan.changelog_path.display().to_string(),
      Some(format!("crate={}", crate_plan.name)),
    ));
    actions.push(MutationAction::new(
      "COMMIT_RELEASE",
      crate_plan.name.clone(),
      Some(format!("tag={}", crate_plan.tag_name)),
    ));
    if !skip_tag {
      actions.push(MutationAction::new(
        "CREATE_TAG",
        crate_plan.tag_name.clone(),
        Some(format!("crate={}", crate_plan.name)),
      ));
    }
    if release_config.push {
      actions.push(MutationAction::new(
        "PUSH_RELEASE_REFS",
        crate_plan.tag_name.clone(),
        Some("remote=origin".to_string()),
      ));
    }
    if release_config.create_github_release && !skip_tag {
      actions.push(MutationAction::new(
        "CREATE_GITHUB_DRAFT",
        crate_plan.tag_name.clone(),
        Some("forge=github".to_string()),
      ));
    }
    if !skip_publish && crate_plan.publish {
      actions.push(MutationAction::new(
        "PUBLISH_CRATE",
        crate_plan.name.clone(),
        Some("registry=crates-io".to_string()),
      ));
    }
    if release_config.create_github_release && !skip_tag {
      actions.push(MutationAction::new(
        "PUBLISH_GITHUB_RELEASE",
        crate_plan.tag_name.clone(),
        Some("forge=github".to_string()),
      ));
    }
  }

  let mut risks = Vec::new();
  if !skip_publish {
    risks.push(MutationRisk::new(
      "CRATES_IO_PUBLISH",
      "high",
      "publishing to crates.io is irreversible",
    ));
  }
  if release_config.require_clean {
    risks.push(MutationRisk::new(
      "REQUIRE_CLEAN_WORKTREE",
      "low",
      "release requires a clean worktree",
    ));
  }
  if release_config.push {
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

  mutation::build_plan(ctx, "release", actions, risks, trace)
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
