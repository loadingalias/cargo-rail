//! Plan, execute, and recover durable exact-SHA releases.

use crate::commands::common::{TextJsonOutputFormat, enforce_safety_gate};
use crate::config::ReleaseRemoteEffects;
use crate::error::{RailError, RailResult};
use crate::mutation::{
    self, ExpectedMutation, MutationAction, MutationEffect, MutationInput, MutationObject, MutationRisk, MutationTrace,
};
use crate::release::planner::{DependentPolicy, RELEASE_REGISTRY, ReleasePlan, ReleasePlanner};
use crate::release::publisher::ReleasePublisher;
use crate::release::state::{Preparation, ReleaseState, ReleaseStatus, StepStatus, state_dir, validate_state_path};
use crate::release::validator::ReleaseValidator;
use crate::release::version::BumpRequest;
use crate::utils;
use crate::workspace::WorkspaceContext;
use std::collections::HashSet;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(serde::Serialize)]
struct ReleaseStatusReport {
    transaction_id: String,
    state: String,
    exact_sha: Option<String>,
    packages: Vec<String>,
    completed_effect: Option<String>,
    next_effect: Option<String>,
    observations: Vec<String>,
    ambiguity: bool,
    recoverability: String,
    safe_operator_command: String,
    journal: Option<PathBuf>,
}

#[derive(Clone)]
struct GitReleaseTransaction {
    transaction_id: String,
    exact_sha: String,
    ambiguity: Option<String>,
}

/// Plan a release (check mode)
pub fn run_release_plan(
    ctx: &WorkspaceContext,
    crate_names: Option<Vec<String>>,
    bump: String,
    extended: bool,
    skip_tag: bool,
    include_dependents: bool,
    format: TextJsonOutputFormat,
) -> RailResult<()> {
    ctx.snapshot()?;
    let json = format.is_json();

    // JSON mode enables structured error output and suppresses progress

    let bump_request = bump.parse::<BumpRequest>()?;

    let workspace_members = ctx.graph().workspace_members();
    let validator = ReleaseValidator::new(ctx);

    let target_crates = crate_names;

    let config = ctx.config().map(|c| &c.release);
    let release_config =
        config.ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;
    let skip_publish = true;

    // Validate release config (tag format, changelog shape, release policies)
    let warnings = release_config.validate(workspace_members).map_err(RailError::Config)?;

    for warning in &warnings {
        crate::warn!("{}", warning);
    }

    let policy = dependent_policy(include_dependents);
    let planner = ReleasePlanner::new(ctx, release_config);
    let plan = planner.plan(target_crates, &bump_request, policy)?;
    let target_crates = plan.canonical_crate_order.clone();
    let mutation_plan = build_release_mutation_plan(ctx, &plan, skip_publish, skip_tag, false, release_config)?;
    let has_pending_changes = !mutation_plan.actions.is_empty();
    let readiness = release_check_readiness(has_pending_changes, skip_publish, skip_tag, release_config);

    if !has_pending_changes {
        if json {
            let payload = serde_json::json!({
              "release_plan": plan,
              "mutation_plan": mutation_plan,
              "check": true,
              "readiness": readiness,
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

    validator.validate(&target_crates, false)?;

    // Validate changelog paths (catches path traversal issues early)
    validator.validate_changelog_paths(&target_crates, release_config)?;
    validator.validate_apply_preconditions(&plan, true, skip_tag, false)?;

    if !json {
        println!("{}", plan.format_summary_with_flags(skip_publish, skip_tag));

        // Show additional config info
        if release_config.remote_effects.creates_forge_release() && !skip_tag {
            println!(
                "Forge releases: enabled ({})",
                release_forge_detail(release_config.remote_effects)
            );
        }
        if release_config.sign_tags && !skip_tag {
            println!("Tag signing: enabled");
        }
    }

    let (extended_results, has_extended_failures) = if extended {
        run_extended_checks(&validator, &plan, json)?
    } else {
        (Vec::new(), false)
    };

    if json {
        let mut payload = serde_json::json!({
          "release_plan": plan,
          "mutation_plan": mutation_plan,
          "check": true,
          "readiness": readiness,
        });
        if extended {
            payload["extended"] = serde_json::json!(extended_results);
        }
        let (result, exit_code) = if has_extended_failures {
            ("failed", 2)
        } else {
            ("pending_changes", 1)
        };
        let output = crate::output::machine_json_envelope("release", "check", result, exit_code, payload);
        let json_output = serde_json::to_string_pretty(&output)
            .map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))?;
        println!("{}", json_output);
    } else if !has_extended_failures {
        println!("\nChanges detected. Run without --check to apply.");
    }

    if has_extended_failures {
        if json {
            return Err(RailError::ExitWithCode { code: 2 });
        }
        return Err(RailError::message("extended validation failed"));
    }

    // Exit code 1 in --check mode indicates changes are pending (consistent across text/json)
    Err(RailError::CheckHasPendingChanges)
}

/// Arguments for release execution.
#[derive(Debug)]
pub struct ReleasePublishArgs {
    /// Explicit crate names to release; ignored when `all` is true.
    pub crate_names: Option<Vec<String>>,
    /// Release all publishable workspace crates.
    pub all: bool,
    /// Version bump strategy.
    pub bump: String,
    /// Positively authorize irreversible crates.io publication.
    pub publish: bool,
    /// Skip creating git tags.
    pub skip_tag: bool,
    /// Review the preparation before publication.
    pub pr: bool,
    /// Execute locally instead of submitting a hosted request.
    pub local: bool,
    /// Continue from the configured GitHub workflow.
    pub executor: bool,
    /// Stop at the prepared boundary for a hosted executor handoff.
    pub prepare: bool,
    /// Persist original intent and progress under the repository release lease.
    pub retain_remote: bool,
    /// Expand explicit crate selection to include the full dependent closure.
    pub include_dependents: bool,
    /// Skip interactive confirmation prompts.
    pub yes: bool,
    /// Authorize release execution from a non-default branch.
    pub allow_non_default_branch: bool,
    /// Apply using a previously generated mutation plan.
    pub plan_path: Option<std::path::PathBuf>,
    /// Output format.
    pub format: TextJsonOutputFormat,
}

struct ReleaseOperationOptions {
    crate_names: Option<Vec<String>>,
    all: bool,
    bump: String,
    skip_publish: bool,
    skip_tag: bool,
    pr: bool,
    include_dependents: bool,
}

fn plan_release_operation(
    ctx: &WorkspaceContext,
    release_config: &crate::config::ReleaseConfig,
    options: ReleaseOperationOptions,
) -> RailResult<(crate::release::planner::ReleasePlan, mutation::MutationPlan)> {
    let targets = if options.all {
        None
    } else if let Some(names) = options.crate_names {
        Some(names)
    } else {
        return Err(RailError::with_help(
            "must specify crate name(s) or --all",
            "cargo rail release check my-crate\ncargo rail release check --all",
        ));
    };
    let bump_request = options.bump.parse::<BumpRequest>()?;
    let planner = ReleasePlanner::new(ctx, release_config);
    let plan = planner.plan(targets, &bump_request, dependent_policy(options.include_dependents))?;
    let mutation_plan = build_release_mutation_plan(
        ctx,
        &plan,
        options.skip_publish,
        options.skip_tag,
        options.pr,
        release_config,
    )?;
    Ok((plan, mutation_plan))
}

/// Execute a release
pub fn run_release_publish(ctx: &WorkspaceContext, args: ReleasePublishArgs) -> RailResult<()> {
    ctx.snapshot()?;
    let json = args.format.is_json();

    let config = ctx.config().map(|c| &c.release);
    let release_config =
        config.ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;
    let skip_publish = registry_publication_skipped(args.publish, release_config)?;

    let workspace_members = ctx.graph().workspace_members();
    let mut warnings = release_config.validate(workspace_members).map_err(RailError::Config)?;
    if !json {
        for warning in &warnings {
            crate::warn!("{}", warning);
        }
    }

    let validator = ReleaseValidator::new(ctx);
    let effective_skip_publish = skip_publish;
    let effective_skip_tag = args.skip_tag;
    let (mut plan, mut expected_mutation_plan) = plan_release_operation(
        ctx,
        release_config,
        ReleaseOperationOptions {
            crate_names: args.crate_names.clone(),
            all: args.all,
            bump: args.bump.clone(),
            skip_publish: effective_skip_publish,
            skip_tag: effective_skip_tag,
            pr: args.pr,
            include_dependents: args.include_dependents,
        },
    )?;
    if plan.crates.is_empty() {
        if json {
            let payload = serde_json::json!({
              "release_plan": plan,
              "warnings": warnings,
            });
            let output = crate::output::machine_json_envelope("release", "apply", "no_changes", 0, payload);
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("{}", plan.format_summary_with_flags(skip_publish, args.skip_tag));
            println!("\nNo release-worthy changes detected.");
        }
        return Ok(());
    }

    let target_crates = plan.canonical_crate_order.clone();
    validator.validate(&target_crates, false)?;

    if let Some(warning) = validator.validate_branch(args.allow_non_default_branch)? {
        if json {
            warnings.push(warning);
        } else {
            crate::warn!("{}", warning);
        }
    }

    // Validate changelog paths
    validator.validate_changelog_paths(&target_crates, release_config)?;

    let mutation_plan = if let Some(path) = args.plan_path.as_ref() {
        let from_file = mutation::read_plan_file(path)?;
        if !from_file.operation_id.starts_with("release-") {
            return Err(RailError::with_help(
                format!("plan '{}' is not a release plan", path.display()),
                "generate a release plan using 'cargo rail release check --json'".to_string(),
            ));
        }
        mutation::validate_pre_apply_with_allowed_paths(ctx, &from_file, std::slice::from_ref(path))?;
        // The saved date is presentation input, not a fresh clock reading. Rebuild
        // every other value from the current selected intent before comparison.
        let github = crate::release::changelog::detect_github_repo(ctx.workspace_root());
        for planned in &mut plan.crates {
            let date = from_file
                .actions
                .iter()
                .filter(|action| action.code == "UPDATE_CHANGELOG")
                .find(|action| {
                    action.payload.get("crate").and_then(serde_json::Value::as_str) == Some(planned.name.as_str())
                })
                .and_then(|action| action.payload.get("presentation"))
                .and_then(|value| value.get("date"))
                .and_then(serde_json::Value::as_str);
            if let Some(date) = date {
                chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                    .map_err(|error| RailError::message(format!("invalid saved release date: {error}")))?;
                planned.presentation = Some(crate::release::presentation::capture(
                    ctx.workspace_root(),
                    release_config,
                    planned,
                    date,
                    github.as_ref(),
                )?);
            }
        }
        expected_mutation_plan = build_release_mutation_plan(
            ctx,
            &plan,
            effective_skip_publish,
            effective_skip_tag,
            args.pr,
            release_config,
        )?;
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
        if !plan.auxiliary_lockfiles.is_empty() {
            println!(
                "  - update {} declared auxiliary Cargo lockfile(s)",
                plan.auxiliary_lockfiles.len()
            );
        }
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

        if !crate::utils::prompt_for_confirmation()? {
            println!("cancelled");
            return Ok(());
        }
    }

    validator.validate_apply_preconditions(&plan, effective_skip_publish, effective_skip_tag, false)?;
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
    let transaction_id = mutation_plan.operation_id.clone();
    publisher.execute(
        &transaction_id,
        &plan,
        skip_publish,
        args.skip_tag,
        args.local,
        args.executor,
        args.prepare,
        args.retain_remote,
        args.pr,
        &planned_paths,
        &allowed_unstaged_paths,
    )?;

    if args.prepare || args.pr || !args.local && release_config.hosted_workflow.is_some() && !args.executor {
        let state = ReleaseState::load(&state_dir(ctx.workspace_root()).join(format!("{transaction_id}.json")))?;
        if json {
            let payload = serde_json::json!({
                "transaction_id": state.transaction_id,
                "intent": state.intent.identity,
                "source": state.release_commit(),
                "phase": state.phase,
                "review": state.review,
            });
            println!(
                "{}",
                serde_json::to_string(&crate::output::machine_json_envelope(
                    "release", "prepare", "success", 0, payload
                ))?
            );
        }
        return Ok(());
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

/// Validate registry publication readiness.
pub fn run_release_publication_check(
    ctx: &WorkspaceContext,
    crate_names: Option<Vec<String>>,
    all: bool,
    extended: bool,
    include_dependents: bool,
    format: TextJsonOutputFormat,
) -> RailResult<()> {
    run_release_publication_check_with_plan_inputs(
        ctx,
        ReleasePublicationCheckArgs {
            crate_names,
            all,
            bump: "auto".to_string(),
            extended,
            skip_tag: false,
            include_dependents,
            format,
        },
    )
}

pub(super) struct ReleasePublicationCheckArgs {
    pub(super) crate_names: Option<Vec<String>>,
    pub(super) all: bool,
    pub(super) bump: String,
    pub(super) extended: bool,
    pub(super) skip_tag: bool,
    pub(super) include_dependents: bool,
    pub(super) format: TextJsonOutputFormat,
}

pub(super) fn run_release_publication_check_with_plan_inputs(
    ctx: &WorkspaceContext,
    args: ReleasePublicationCheckArgs,
) -> RailResult<()> {
    let ReleasePublicationCheckArgs {
        crate_names,
        all,
        bump,
        extended,
        skip_tag,
        include_dependents,
        format,
    } = args;
    ctx.snapshot()?;
    let json = format.is_json();
    let release_config = ctx
        .config()
        .as_ref()
        .map(|config| &config.release)
        .ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;
    let skip_publish = registry_publication_skipped(true, release_config)?;
    debug_assert!(!skip_publish);

    let workspace_members = ctx.graph().workspace_members();
    let warnings = release_config.validate(workspace_members).map_err(RailError::Config)?;
    if !json {
        for warning in &warnings {
            crate::warn!("{}", warning);
        }
    }

    let (plan, mutation_plan) = plan_release_operation(
        ctx,
        release_config,
        ReleaseOperationOptions {
            crate_names,
            all,
            bump,
            skip_publish: false,
            skip_tag,
            pr: false,
            include_dependents,
        },
    )?;
    let has_pending_changes = !mutation_plan.actions.is_empty();
    let target_crates = plan.canonical_crate_order.clone();
    let validator = ReleaseValidator::new(ctx);
    validator.validate(&target_crates, false)?;
    validator.validate_changelog_paths(&target_crates, release_config)?;
    // Match run's local/tag/release-note checks without performing registry
    // lookups. Live publication checks remain opt-in under --extended.
    validator.validate_apply_preconditions(&plan, true, skip_tag, false)?;

    let insights = ReleasePlanner::new(ctx, release_config).release_check_insights(&target_crates)?;
    let commit_diagnostics = insights.commit_diagnostics;
    let commit_failed =
        release_config.unconventional_commits == crate::config::CommitPolicy::Deny && !commit_diagnostics.is_empty();
    let missing_change_files = insights.missing_change_files;
    let shallow_repository = insights.shallow_repository;
    let has_change_file_failures = !missing_change_files.is_empty();
    let has_shallow_failures = shallow_repository;

    let publishable_crates = plan
        .crates
        .iter()
        .filter(|crate_plan| crate_plan.publish)
        .map(|crate_plan| crate_plan.name.clone())
        .collect::<Vec<_>>();
    let skipped_crates = plan
        .crates
        .iter()
        .filter(|crate_plan| !crate_plan.publish)
        .map(|crate_plan| {
            let reason = validator
                .unpublishable_reason(&crate_plan.name)
                .unwrap_or_else(|| "release plan disables publication".to_string());
            (crate_plan.name.clone(), reason)
        })
        .collect::<Vec<_>>();
    for crate_name in &publishable_crates {
        validator.validate_publishable(crate_name)?;
    }

    if !json {
        println!("{}", plan.format_summary_with_flags(false, skip_tag));
        for crate_name in &publishable_crates {
            println!("{}: ready for crates-io publication", crate_name);
        }
        for (crate_name, reason) in &skipped_crates {
            println!("{}: not publishable ({})", crate_name, reason);
        }
        if shallow_repository {
            println!("\nrelease history:");
            println!("  shallow clone: fetch tags: git fetch --unshallow --tags, or set fetch-depth: 0");
        }
        for (name, diagnostics) in &commit_diagnostics {
            for diagnostic in diagnostics {
                println!("{name}: {}", diagnostic.describe());
            }
        }
        if !missing_change_files.is_empty() {
            println!("\nmissing change files:");
            for crate_name in &missing_change_files {
                println!(
                    "  {}: code changes require {} coverage",
                    crate_name, release_config.change_dir
                );
            }
        }
    }

    let (extended_results, has_extended_failures) = if extended {
        run_extended_checks(&validator, &plan, json)?
    } else {
        (Vec::new(), false)
    };

    let validation_failed = has_extended_failures || has_change_file_failures || has_shallow_failures || commit_failed;
    if json {
        let (result, exit_code, status) = if validation_failed {
            ("failed", 2, "failed")
        } else if has_pending_changes {
            ("pending_changes", 1, "pending")
        } else {
            ("no_changes", 0, "passed")
        };
        let mut payload = serde_json::json!({
          "action": "check",
          "check": true,
          "release_plan": plan,
          "mutation_plan": mutation_plan,
          "readiness": publication_check_readiness(&mutation_plan),
          "status": status,
          "crates": publishable_crates,
          "count": publishable_crates.len(),
          "skipped": skipped_crates
              .iter()
              .map(|(name, reason)| serde_json::json!({"crate": name, "reason": reason}))
              .collect::<Vec<_>>(),
          "warnings": warnings,
        });
        if !commit_diagnostics.is_empty() {
            payload["commit_diagnostics"] = serde_json::json!(commit_diagnostics);
        }
        if extended {
            payload["extended"] = serde_json::json!(extended_results);
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
        let output = crate::output::machine_json_envelope("release", "check", result, exit_code, payload);
        println!("{}", serde_json::to_string_pretty(&output)?);
    }

    if validation_failed {
        if json {
            return Err(RailError::ExitWithCode { code: 2 });
        }
        return Err(RailError::message(if has_shallow_failures {
            "release history check failed"
        } else if has_change_file_failures {
            "change file coverage failed"
        } else {
            "extended validation failed"
        }));
    }
    if has_pending_changes {
        if !json {
            println!("\nPublication-ready changes detected. Run the matching release command to apply.");
        }
        return Err(RailError::CheckHasPendingChanges);
    }
    if !json {
        println!("\nNo release-worthy changes detected.");
    }
    Ok(())
}

fn run_extended_checks(
    validator: &ReleaseValidator<'_>,
    plan: &ReleasePlan,
    json: bool,
) -> RailResult<(Vec<serde_json::Value>, bool)> {
    if !json {
        println!("\nrunning extended checks...");
    }
    let results = validator.validate_extended(plan)?;
    let mut rendered = Vec::with_capacity(results.len());
    let mut failed = false;
    for (crate_name, checks) in results {
        let mut crate_checks = Vec::with_capacity(checks.len());
        for check in checks {
            if !json {
                if check.is_skipped() {
                    println!(
                        "  {}: {} - SKIPPED: {}",
                        crate_name,
                        check.check_name,
                        check.details.as_deref().unwrap_or("no evidence")
                    );
                } else if check.passed {
                    println!(
                        "  {}: {} - {}",
                        crate_name,
                        check.check_name,
                        check.details.as_deref().unwrap_or("ok")
                    );
                } else {
                    crate::error!(
                        "  {}: {} - FAILED: {}",
                        crate_name,
                        check.check_name,
                        check.error.as_deref().unwrap_or("unknown error")
                    );
                }
            }
            failed |= !check.passed && !check.is_skipped();
            crate_checks.push(serde_json::json!({
              "check": check.check_name,
              "passed": check.passed,
              "skipped": check.is_skipped(),
              "details": check.details,
              "error": check.error
            }));
        }
        rendered.push(serde_json::json!({
          "crate": crate_name,
          "checks": crate_checks
        }));
    }
    Ok((rendered, failed))
}

fn release_check_readiness(
    has_pending_changes: bool,
    skip_publish: bool,
    skip_tag: bool,
    release_config: &crate::config::ReleaseConfig,
) -> serde_json::Value {
    serde_json::json!({
      "scope": "local",
      "effects_executed": [],
      "effects_excluded_from_check": [
        "workspace_mutation",
        "git_commit",
        "git_tag",
        "git_push",
        "forge_release",
        "registry_publication"
      ],
      "planned_effects": {
        "workspace_mutation": has_pending_changes,
        "git_commit": has_pending_changes,
        "git_tag": has_pending_changes && !skip_tag,
        "git_push": has_pending_changes && release_config.remote_effects != ReleaseRemoteEffects::None,
        "forge_release": has_pending_changes && release_config.remote_effects.creates_forge_release(),
        "registry_publication": has_pending_changes && !skip_publish
      }
    })
}

fn publication_check_readiness(plan: &mutation::MutationPlan) -> serde_json::Value {
    let has_action = |code| plan.actions.iter().any(|action| action.code == code);
    let workspace_mutation = plan.actions.iter().any(|action| !action.expected_mutations.is_empty());
    serde_json::json!({
      "scope": "publication",
      "effects_executed": [],
      "effects_excluded_from_check": [
        "workspace_mutation",
        "git_commit",
        "git_tag",
        "git_push",
        "forge_release",
        "registry_publication"
      ],
      "planned_effects": {
        "workspace_mutation": workspace_mutation,
        "git_commit": has_action("COMMIT_RELEASE"),
        "git_tag": has_action("CREATE_TAG"),
        "git_push": has_action("PUSH_RELEASE_COMMIT") || has_action("PUSH_RELEASE_TAGS"),
        "forge_release": has_action("CREATE_FORGE_RELEASE") || has_action("PUBLISH_FORGE_RELEASE"),
        "registry_publication": has_action("PUBLISH_CRATE")
      }
    })
}

fn dependent_policy(include_dependents: bool) -> DependentPolicy {
    if include_dependents {
        DependentPolicy::IncludeDependents
    } else {
        DependentPolicy::RejectPartialClosure
    }
}

fn registry_publication_skipped(publish: bool, release_config: &crate::config::ReleaseConfig) -> RailResult<bool> {
    if !publish {
        return Ok(true);
    }
    if release_config.remote_effects == ReleaseRemoteEffects::None {
        return Err(RailError::with_help(
            "--publish cannot be combined with release.remote_effects = \"none\"",
            "select an explicit remote effect authority before authorizing irreversible crates.io publication",
        ));
    }
    let registry = release_config.registry_publication.registry().ok_or_else(|| {
        RailError::with_help(
            "--publish requires release.registry_publication = \"crates-io\"",
            "authorize the exact registry in rail.toml as well as at invocation time",
        )
    })?;
    if registry != RELEASE_REGISTRY {
        return Err(RailError::message(format!(
            "release configuration selected unsupported registry '{registry}'"
        )));
    }
    Ok(false)
}

/// Show durable release transactions without loading Cargo metadata.
pub fn run_release_status_standalone(
    workspace_root: &Path,
    state_path: Option<&Path>,
    history: bool,
    format: TextJsonOutputFormat,
) -> RailResult<()> {
    let requested_transaction = state_path
        .filter(|path| !path.exists())
        .and_then(Path::to_str)
        .filter(|value| value.starts_with("release-") && !value.contains(std::path::MAIN_SEPARATOR))
        .map(str::to_string);
    if let Some(path) = state_path
        && !path.exists()
        && requested_transaction.is_none()
    {
        return Err(RailError::with_help(
            format!("release state '{}' does not exist", path.display()),
            "pass an existing journal path or a Rail-Release transaction ID",
        ));
    }
    let paths = if let Some(path) = state_path.filter(|path| path.exists()) {
        vec![validate_state_path(workspace_root, path)?]
    } else {
        crate::release::hosted::refresh(workspace_root, None)?;
        let directory = state_dir(workspace_root);
        if !directory.exists() {
            Vec::new()
        } else {
            let mut paths = std::fs::read_dir(&directory)?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.extension().is_some_and(|extension| extension == "json"))
                .collect::<Vec<_>>();
            paths.sort();
            paths
        }
    };
    let mut reports = paths
        .into_iter()
        .map(|path| match ReleaseState::load(&path) {
            Ok(state) => release_status_report(state, path),
            Err(error) => unreadable_release_status_report(path, error),
        })
        .collect::<Vec<_>>();
    if let Some(requested) = requested_transaction.as_ref() {
        reports.retain(|report| &report.transaction_id == requested);
    }
    let journal_transactions = reports
        .iter()
        .map(|report| report.transaction_id.clone())
        .collect::<HashSet<_>>();
    if state_path.is_none() || requested_transaction.is_some() {
        for transaction in git_release_transactions(workspace_root)? {
            if journal_transactions.contains(&transaction.transaction_id)
                || requested_transaction
                    .as_ref()
                    .is_some_and(|requested| requested != &transaction.transaction_id)
            {
                continue;
            }
            reports.push(history_status_report(workspace_root, transaction));
        }
    }
    if let Some(requested) = requested_transaction
        && !reports.iter().any(|report| report.transaction_id == requested)
    {
        return Err(RailError::with_help(
            format!(
                "release transaction '{}' was not found in journals or Git history",
                requested
            ),
            "copy the exact transaction ID from a Rail-Release commit trailer",
        ));
    }

    reports.sort_by(|left, right| left.transaction_id.cmp(&right.transaction_id));
    if state_path.is_none() && !history {
        reports.retain(|report| report.recoverability != "terminal" || report.ambiguity);
    }

    if format.is_json() {
        let payload = serde_json::json!({ "transactions": reports });
        let output = crate::output::machine_json_envelope("release", "status", "success", 0, payload);
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    if reports.is_empty() {
        println!("No active or actionable release transactions.");
        return Ok(());
    }
    for (index, report) in reports.iter().enumerate() {
        if index > 0 {
            println!();
        }
        println!("{}  {}", report.transaction_id, report.state);
        if !report.packages.is_empty() {
            println!("  Packages: {}", report.packages.join(", "));
        }
        println!("  Exact SHA: {}", report.exact_sha.as_deref().unwrap_or("not prepared"));
        for observation in &report.observations {
            if observation.starts_with("executor:")
                || observation.starts_with("validation:")
                || observation.starts_with("artifacts:")
            {
                println!("  {observation}");
            }
        }
        println!(
            "  Last completed effect: {}",
            report.completed_effect.as_deref().unwrap_or("none")
        );
        println!("  Next effect: {}", report.next_effect.as_deref().unwrap_or("none"));
        println!("  Ambiguous: {}", if report.ambiguity { "yes" } else { "no" });
        println!("  Action: {}", report.safe_operator_command);
        if crate::output::is_verbose() {
            println!("  Recoverability: {}", report.recoverability);
            if let Some(journal) = &report.journal {
                println!("  Journal: {}", journal.display());
            }
            if !report.observations.is_empty() {
                println!("  Observations: {}", report.observations.join(", "));
            }
        }
    }
    Ok(())
}

fn unreadable_release_status_report(path: PathBuf, error: RailError) -> ReleaseStatusReport {
    let transaction_id = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("unreadable-journal")
        .to_string();
    ReleaseStatusReport {
        transaction_id,
        state: "journal:ambiguous".to_string(),
        exact_sha: None,
        packages: Vec::new(),
        completed_effect: None,
        next_effect: None,
        observations: vec![format!("journal:invalid={error}")],
        ambiguity: true,
        recoverability: "unreadable".to_string(),
        safe_operator_command: format!("cargo rail release status {} --format json", path.display()),
        journal: Some(path),
    }
}

fn release_status_report(state: ReleaseState, path: PathBuf) -> ReleaseStatusReport {
    let mut completed = None;
    let mut next = None;
    let mut observations = Vec::new();
    let mut ambiguity = false;
    let mut record = |name: String, status: StepStatus, object: Option<&str>| {
        if status == StepStatus::InProgress {
            ambiguity = true;
        }
        if let Some(object) = object {
            let status = match status {
                StepStatus::Pending => "pending",
                StepStatus::InProgress => "in_progress",
                StepStatus::Complete => "complete",
            };
            observations.push(format!("{}:{}={}", name, status, object));
        }
        if status == StepStatus::Complete {
            completed = Some(name);
        } else if next.is_none() {
            next = Some(name);
        }
    };
    let (status, object) = match &state.preparation {
        Preparation::Pending => (StepStatus::Pending, None),
        Preparation::Writing => (StepStatus::InProgress, None),
        Preparation::Committing { tree } => (StepStatus::InProgress, Some(tree.as_str())),
        Preparation::Complete { commit } => (StepStatus::Complete, Some(commit.as_str())),
    };
    record("workspace preparation".to_string(), status, object);
    if let Some(review) = &state.review {
        record(
            "review branch push".into(),
            review.pushed.status,
            review.pushed.object.as_deref(),
        );
        record(
            "reviewed merge".into(),
            if review.merge.is_some() {
                StepStatus::Complete
            } else {
                StepStatus::Pending
            },
            review.merge.as_ref().map(|merge| merge.commit.as_str()),
        );
    }
    record(
        "remote Git commit push".to_string(),
        state.commit_push.status,
        state.commit_push.object.as_deref(),
    );
    record(
        "remote exact-SHA readiness validation".to_string(),
        state.readiness.status,
        state.readiness.object.as_deref(),
    );
    for crate_state in &state.crates {
        record(
            format!("local Git tag for {}", crate_state.name),
            crate_state.tag.status,
            crate_state.tag.object.as_deref(),
        );
    }
    for crate_state in &state.crates {
        record(
            format!("irreversible registry publication for {}", crate_state.name),
            crate_state.publication.status,
            crate_state.publication.object.as_deref(),
        );
    }
    record(
        "remote Git tag push".to_string(),
        state.tag_push.status,
        state.tag_push.object.as_deref(),
    );
    for crate_state in &state.crates {
        record(
            format!("remote forge draft for {}", crate_state.name),
            crate_state.forge_draft.status,
            crate_state.forge_draft.object.as_deref(),
        );
        record(
            format!("remote forge publication for {}", crate_state.name),
            crate_state.forge_publication.status,
            crate_state.forge_publication.object.as_deref(),
        );
    }
    record(
        "local release restoration".to_string(),
        state.abort.status,
        state.abort.object.as_deref(),
    );

    for package in &state.crates {
        if state.intent.alias_previous.contains_key(&package.name) {
            record(
                format!("release alias for {}", package.name),
                package.alias.status,
                package.alias.object.as_deref(),
            );
        }
    }
    let abort_in_progress = state.abort.status == StepStatus::InProgress;
    let abort_retains_preparation = abort_in_progress && state.abort.object.as_deref() == state.release_commit();
    if state.status == ReleaseStatus::Aborted {
        ambiguity = false;
    }
    let (recoverability, safe_operator_command) = match state.status {
        ReleaseStatus::Active if abort_in_progress => (
            "reconcile_abort".to_string(),
            format!(
                "cargo rail release abort {}{} --yes",
                state.transaction_id,
                if abort_retains_preparation {
                    " --retain-preparation"
                } else {
                    ""
                }
            ),
        ),
        ReleaseStatus::Active => (
            if ambiguity { "reconcile" } else { "resumable" }.to_string(),
            format!("cargo rail release resume {}", state.transaction_id),
        ),
        ReleaseStatus::Complete | ReleaseStatus::Aborted => (
            "terminal".to_string(),
            format!("cargo rail clean --release-journal {}", state.transaction_id),
        ),
    };
    let packages = state
        .intent
        .plan
        .crates
        .iter()
        .map(|package| format!("{} {} → {}", package.name, package.current_version, package.new_version))
        .collect();
    for run in &state.validation {
        observations.push(format!(
            "validation:{} run {} attempt {}",
            run.workflow, run.run_id, run.attempt
        ));
    }
    for artifact in &state.artifacts {
        observations.push(format!(
            "artifacts:{} id {} ({} files)",
            artifact.package,
            artifact.artifact_id,
            artifact.files.len()
        ));
    }
    let exact_sha = state.release_commit().map(str::to_owned);
    if let Some(url) = crate::release::hosted::url(&state) {
        observations.push(format!("executor:{url}"));
    }
    ReleaseStatusReport {
        transaction_id: state.transaction_id,
        state: format!("{}:{:?}", state.phase.as_str(), state.status).to_ascii_lowercase(),
        exact_sha,
        packages,
        completed_effect: completed,
        next_effect: if state.status == ReleaseStatus::Active {
            next
        } else {
            None
        },
        observations,
        ambiguity,
        recoverability,
        safe_operator_command,
        journal: Some(path),
    }
}

fn history_status_report(_workspace_root: &Path, transaction: GitReleaseTransaction) -> ReleaseStatusReport {
    let ambiguity = transaction.ambiguity.is_some();
    ReleaseStatusReport {
        safe_operator_command: format!("cargo rail release record fetch {}", transaction.transaction_id),
        transaction_id: transaction.transaction_id,
        state: "record_unavailable".into(),
        exact_sha: Some(transaction.exact_sha),
        packages: Vec::new(),
        completed_effect: Some("release_commit".into()),
        next_effect: Some("recover_original_record".into()),
        observations: transaction.ambiguity.into_iter().collect(),
        ambiguity,
        recoverability: "missing_record".into(),
        journal: None,
    }
}

fn git_release_transactions(workspace_root: &Path) -> RailResult<Vec<GitReleaseTransaction>> {
    let output = Command::new("git")
        .current_dir(workspace_root)
        .args([
            "log",
            "--fixed-strings",
            "--grep=Rail-Release:",
            "--format=%H%x00%B%x00",
        ])
        .output()
        .map_err(|error| RailError::message(format!("failed to inspect release transaction history: {}", error)))?;
    if !output.status.success() {
        return Err(RailError::message(format!(
            "failed to inspect release transaction history: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let fields = output.stdout.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut transactions = Vec::<GitReleaseTransaction>::new();
    for pair in fields.chunks(2) {
        let [sha, message] = pair else {
            continue;
        };
        let sha = String::from_utf8_lossy(sha).trim().to_string();
        if sha.is_empty() {
            continue;
        }
        let message = String::from_utf8_lossy(message);
        let Some(transaction_id) = trailer_value(&message, "Rail-Release") else {
            continue;
        };
        if !transaction_id.starts_with("release-") {
            continue;
        }
        if !recognized_release_commit(&message) {
            continue;
        }
        let index = transactions
            .iter()
            .position(|transaction| transaction.transaction_id == transaction_id);
        if let Some(index) = index {
            transactions[index].ambiguity = Some("duplicate preparation commits for one transaction".into());
        } else {
            transactions.push(GitReleaseTransaction {
                transaction_id,
                exact_sha: sha,
                ambiguity: (trailer_values(&message, "Rail-Release").len() != 1
                    || trailer_values(&message, "Rail-Release-Intent").len() != 1)
                    .then(|| "duplicate or missing release record identity".into()),
            });
        }
    }
    Ok(transactions)
}

fn recognized_release_commit(message: &str) -> bool {
    trailer_values(message, "Rail-Release-Contract")
        == [crate::release::state::RELEASE_STATE_SCHEMA_VERSION.to_string()]
}

fn trailer_value(message: &str, key: &str) -> Option<String> {
    trailer_values(message, key).into_iter().next()
}

fn trailer_values(message: &str, key: &str) -> Vec<String> {
    let prefix = format!("{}: ", key);
    message
        .lines()
        .filter_map(|line| line.trim().strip_prefix(&prefix).map(str::to_string))
        .collect()
}

/// Resume the original release record, selecting the unique active transaction by default.
pub fn run_release_resume(ctx: &WorkspaceContext, transaction: Option<&str>, executor: bool) -> RailResult<()> {
    let path = crate::release::state::resolve_active(ctx.workspace_root(), transaction)?;
    let release_config = &ctx
        .config()
        .as_ref()
        .ok_or_else(|| RailError::message("no release configuration"))?
        .release;
    ReleasePublisher::new(ctx, release_config).resume(&path, executor)
}

/// Abort an active release before any external side effect has occurred.
pub fn run_release_abort(ctx: &WorkspaceContext, transaction: Option<&str>, yes: bool) -> RailResult<()> {
    run_release_abort_with_options(ctx, transaction, false, yes)
}

pub(crate) fn run_release_abort_with_options(
    ctx: &WorkspaceContext,
    transaction: Option<&str>,
    retain_preparation: bool,
    yes: bool,
) -> RailResult<()> {
    let state = crate::release::state::resolve_active(ctx.workspace_root(), transaction)?;
    enforce_safety_gate("release abort", yes, None, io::stdin().is_terminal())?;
    if !yes && io::stdin().is_terminal() && !crate::utils::prompt_for_confirmation()? {
        println!("cancelled");
        return Ok(());
    }
    let release_config = ctx
        .config()
        .as_ref()
        .map(|config| &config.release)
        .ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;
    let publisher = ReleasePublisher::new(ctx, release_config);
    if retain_preparation {
        publisher.abort_retaining_preparation(&state)
    } else {
        publisher.abort(&state)
    }
}

fn build_release_mutation_plan(
    ctx: &WorkspaceContext,
    plan: &crate::release::planner::ReleasePlan,
    skip_publish: bool,
    skip_tag: bool,
    pr: bool,
    release_config: &crate::config::ReleaseConfig,
) -> RailResult<mutation::MutationPlan> {
    let publish_registry = if skip_publish {
        None
    } else {
        Some(release_config.registry_publication.registry().ok_or_else(|| {
            RailError::message("release mutation plan has no configured registry publication authority")
        })?)
    };
    let mut actions = Vec::with_capacity(
        plan.crates.len() * 7
            + plan.change_files_to_delete.len()
            + plan.change_files_to_update.len()
            + plan.auxiliary_lockfiles.len()
            + 3,
    );

    for crate_plan in &plan.crates {
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
                let package = ctx.cargo().get_package(dependent).ok_or_else(|| {
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
                  "presentation": crate_plan.presentation,
                }))
                .with_mutations(vec![release_mutation(
                    ctx,
                    &crate_plan.changelog_path,
                    MutationEffect::Write,
                )?]),
            );
        }
    }
    if !plan.crates.is_empty() {
        for path in &plan.change_files_to_delete {
            actions.push(
                MutationAction::new("DELETE_CHANGE_FILE", path.display().to_string(), None)
                    .with_payload(serde_json::json!({ "path": path }))
                    .with_mutations(vec![release_mutation(ctx, path, MutationEffect::Delete)?]),
            );
        }
        for update in &plan.change_files_to_update {
            actions.push(
                MutationAction::new("UPDATE_CHANGE_FILE", update.path.display().to_string(), None)
                    .with_payload(serde_json::json!({ "path": update.path, "content": update.content }))
                    .with_mutations(vec![release_mutation(ctx, &update.path, MutationEffect::Write)?]),
            );
        }
        actions.push(
            MutationAction::new("UPDATE_LOCKFILE", "Cargo.lock", None)
                .with_payload(serde_json::json!({ "packages": plan.canonical_crate_order }))
                .with_mutations(vec![release_mutation(
                    ctx,
                    &ctx.workspace_root().join("Cargo.lock"),
                    MutationEffect::Write,
                )?]),
        );
        for auxiliary in &plan.auxiliary_lockfiles {
            actions.push(
                MutationAction::new(
                    "UPDATE_AUXILIARY_LOCKFILE",
                    auxiliary.lockfile_path.display().to_string(),
                    Some(format!("manifest={}", auxiliary.manifest_path.display())),
                )
                .with_payload(serde_json::json!({
                  "manifest": auxiliary.manifest_path,
                  "lockfile": auxiliary.lockfile_path,
                  "before_digest": auxiliary.before_digest,
                  "after_digest": auxiliary.after_digest,
                }))
                .with_mutations(vec![release_mutation(
                    ctx,
                    &ctx.workspace_root().join(&auxiliary.lockfile_path),
                    MutationEffect::Write,
                )?]),
            );
        }
    }

    if plan.crates.is_empty() {
        // Match `release run`: an empty release plan exits before any local or
        // external action is authorized.
    } else {
        if pr {
            actions.push(
                MutationAction::new("AWAIT_REVIEW", "release-pr", None)
                    .with_payload(serde_json::json!({ "crates": plan.canonical_crate_order })),
            );
            actions.push(
                MutationAction::new("PUSH_RELEASE_PR", "origin", None)
                    .with_payload(serde_json::json!({ "remote": "origin" })),
            );
            actions.push(MutationAction::new("OPEN_RELEASE_PR", "origin", None));
        }
        actions.push(
            MutationAction::new("COMMIT_RELEASE", "workspace", None)
                .with_payload(serde_json::json!({ "crates": plan.canonical_crate_order })),
        );
        if release_config.remote_effects.pushes() && !pr {
            actions.push(
                MutationAction::new("PUSH_RELEASE_COMMIT", "origin", None).with_payload(serde_json::json!({
                  "remote": "origin",
                  "branch": ctx.git()?.git().current_branch()?,
                })),
            );
            if !skip_publish || !skip_tag {
                actions.push(
                    MutationAction::new("AWAIT_EXACT_SHA_CHECKS", "release_commit", None)
                        .with_payload(serde_json::json!({ "remote": "origin", "poll": false })),
                );
            }
        }
        if pr && (!skip_publish || !skip_tag) && release_config.remote_effects.pushes() {
            actions.push(MutationAction::new("AWAIT_EXACT_SHA_CHECKS", "reviewed merge", None));
        }
        if let Some(registry) = publish_registry {
            for crate_plan in plan.crates.iter().filter(|crate_plan| crate_plan.publish) {
                actions.push(
                    MutationAction::new("PUBLISH_CRATE", crate_plan.name.clone(), None)
                        .with_payload(serde_json::json!({ "crate": crate_plan.name, "registry": registry })),
                );
            }
        }
        if !skip_tag {
            for crate_plan in &plan.crates {
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
            if release_config.remote_effects.pushes() {
                actions.push(MutationAction::new("PUSH_RELEASE_TAGS", "origin", None).with_payload(
                    serde_json::json!({
                      "remote": "origin",
                      "tags": plan.crates.iter().map(|crate_plan| crate_plan.tag_name.clone()).collect::<Vec<_>>(),
                    }),
                ));
            }
            if release_config.remote_effects.creates_forge_release() {
                for crate_plan in &plan.crates {
                    actions.push(
            MutationAction::new("CREATE_FORGE_RELEASE", crate_plan.tag_name.clone(), None).with_payload(
              serde_json::json!({ "forge": release_forge_detail(release_config.remote_effects), "tag": crate_plan.tag_name, "body": crate_plan.presentation.as_ref().map(|presentation| &presentation.release_notes) }),
            ),
          );
                }
                for crate_plan in &plan.crates {
                    actions.push(
            MutationAction::new("PUBLISH_FORGE_RELEASE", crate_plan.tag_name.clone(), None).with_payload(
              serde_json::json!({ "forge": release_forge_detail(release_config.remote_effects), "tag": crate_plan.tag_name, "body": crate_plan.presentation.as_ref().map(|presentation| &presentation.release_notes) }),
            ),
          );
                }
            }
        }
    }

    for package in &plan.crates {
        if let Some(alias) = release_config.aliases.get(&package.name) {
            actions.push(
                MutationAction::new("PROMOTE_RELEASE_ALIAS", alias, None)
                    .with_payload(serde_json::json!({"package":package.name,"immutable_tag":package.tag_name})),
            );
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
    if !actions.is_empty() && (release_config.remote_effects.pushes() || pr) {
        risks.push(MutationRisk::new(
      "REMOTE_PUSH",
      "medium",
      "the exact release commit is pushed for checks; tags are pushed only after package publication is observable",
    ));
    }

    let trace = vec![MutationTrace::new(
        "RELEASE_PLAN_RESOLVED",
        format!(
            "resolved {} crate(s), {} publish candidate(s), skip_tag={}, publish_registry={}",
            plan.summary.total_crates,
            plan.summary.crates_to_publish,
            skip_tag,
            publish_registry.unwrap_or("none")
        ),
    )];

    mutation::build_plan_with_inputs(
        ctx,
        "release",
        actions,
        release_declared_inputs(ctx, plan)?,
        risks,
        trace,
    )
}

fn release_declared_inputs(
    ctx: &WorkspaceContext,
    plan: &crate::release::planner::ReleasePlan,
) -> RailResult<Vec<MutationInput>> {
    let git = ctx.git()?.git();
    let git_root = &git.worktree_root;
    let mut paths = Vec::new();
    if let Some(config_path) = ctx.config_path() {
        paths.push(config_path.to_path_buf());
    }

    for planned in &plan.crates {
        if let Some(presentation) = &planned.presentation {
            paths.extend(
                presentation
                    .note_inputs
                    .iter()
                    .map(|input| ctx.workspace_root().join(&input.path)),
            );
        }
    }
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .map(|path| {
            let relative = utils::path_relative_to(git_root, &path).map_err(|error| {
                RailError::message(format!(
                    "release input '{}' is outside git worktree '{}': {}",
                    path.display(),
                    git_root.display(),
                    error
                ))
            })?;
            MutationInput::capture(git, git_root, relative)
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
    let relative = utils::path_relative_to(git_root, &absolute).map_err(|error| {
        RailError::message(format!(
            "release path '{}' is outside git worktree '{}': {}",
            absolute.display(),
            git_root.display(),
            error
        ))
    })?;
    let relative = std::path::PathBuf::from(utils::path_to_git_format(&relative));
    Ok(ExpectedMutation::capture(git_root, relative, effect))
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

fn release_forge_detail(remote_effects: ReleaseRemoteEffects) -> &'static str {
    match remote_effects {
        ReleaseRemoteEffects::Auto => "auto",
        ReleaseRemoteEffects::Github => "github",
        ReleaseRemoteEffects::Gitlab => "gitlab",
        ReleaseRemoteEffects::None | ReleaseRemoteEffects::Push => "none",
    }
}

/// Initialize release configuration
pub fn run_release_init(ctx: &WorkspaceContext, crates: Option<Vec<String>>, dry_run: bool) -> RailResult<()> {
    ctx.snapshot()?;
    use crate::config::{ChangelogConfig, CrateReleaseConfig, RailConfig};
    use std::fs;

    let requested_crates = crates;

    let members = ctx.cargo().workspace_members();
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
        surface: crate::config::SurfaceConfig::default(),
        plan: crate::config::PlanConfig::default(),
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
    let config_toml = toml_edit::ser::to_string_pretty(&config)
        .map_err(|e| crate::error::RailError::message(format!("config serialization failed: {}", e)))?;

    if dry_run {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git").current_dir(root).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn preparation_history_reports_duplicate_record_identity_without_execution_authority() {
        let root = tempfile::tempdir().unwrap();
        test_git(root.path(), &["init", "-q", "-b", "main"]);
        test_git(root.path(), &["config", "user.name", "Cargo-Rail Test"]);
        test_git(root.path(), &["config", "user.email", "cargo-rail@example.invalid"]);
        let message = "prepare\n\nRail-Release: release-fixture\nRail-Release-Contract: 10\nRail-Release-Intent: first\nRail-Release-Intent: second";
        test_git(root.path(), &["commit", "--allow-empty", "-qm", message]);
        let transaction = git_release_transactions(root.path()).unwrap().remove(0);
        let status = history_status_report(root.path(), transaction);
        assert!(status.ambiguity);
        assert_eq!(status.recoverability, "missing_record");
        assert_eq!(status.next_effect.as_deref(), Some("recover_original_record"));
    }
}
