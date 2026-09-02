//! Plan, execute, and recover durable exact-SHA releases.

use crate::commands::common::{TextJsonOutputFormat, enforce_safety_gate};
use crate::config::ReleaseRemoteEffects;
use crate::error::{RailError, RailResult};
use crate::mutation::{
    self, ExpectedMutation, MutationAction, MutationEffect, MutationInput, MutationObject, MutationRisk, MutationTrace,
};
use crate::release::planner::{DependentPolicy, RELEASE_REGISTRY, ReleasePlanner};
use crate::release::publisher::{
    CheckReadiness, ReleasePublisher, observe_github_exact_sha_readiness, observe_gitlab_exact_sha_readiness,
};
use crate::release::remote::{RemoteRepository, release_repository};
use crate::release::state::{
    ReconstructedRelease, ReleaseState, ReleaseStatus, StepStatus, state_dir, validate_state_path,
};
use crate::release::validator::ReleaseValidator;
use crate::release::version::BumpRequest;
use crate::utils;
use crate::workspace::WorkspaceContext;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(serde::Serialize)]
struct ReleaseStatusReport {
    transaction_id: String,
    state: String,
    exact_sha: Option<String>,
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
    mode: String,
    publish: Option<bool>,
    publish_registry: Option<String>,
    tag: Option<bool>,
    remote: Option<String>,
    remote_repository: Option<RemoteRepository>,
    crates: BTreeMap<String, String>,
    tags: BTreeMap<String, String>,
    crate_publish: BTreeMap<String, bool>,
    commit_targets: BTreeMap<String, String>,
    ambiguity: Option<String>,
}

enum PrepareReconciliation {
    Incomplete,
    Terminal(Box<GitReleaseTransaction>),
    Ambiguous,
}

/// Inputs that select and authorize an already-merged release finalization.
#[derive(Debug)]
pub struct ReleaseFinalizeOptions {
    /// Explicit crate names to finalize when `all` is false.
    pub crate_names: Option<Vec<String>>,
    /// Finalize every crate selected by the release plan.
    pub all: bool,
    /// Positively authorize irreversible crates.io publication.
    pub publish: bool,
    /// Complete the release without creating or pushing release tags.
    pub skip_tag: bool,
    /// Include crates that depend on the explicitly selected crates.
    pub include_dependents: bool,
    /// Confirm the irreversible release effects without an interactive prompt.
    pub yes: bool,
    /// Authorize finalization from a non-default branch.
    pub allow_non_default_branch: bool,
    /// Select human-readable or machine-readable output.
    pub format: TextJsonOutputFormat,
}

/// Plan a release (check mode)
pub fn run_release_plan(
    ctx: &WorkspaceContext,
    crate_names: Option<Vec<String>>,
    bump: String,
    publish: bool,
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
    let skip_publish = registry_publication_skipped(publish, release_config)?;

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

    if json {
        let payload = serde_json::json!({
          "release_plan": plan,
          "mutation_plan": mutation_plan,
          "check": true,
          "readiness": readiness,
        });
        let output = crate::output::machine_json_envelope("release", "check", "pending_changes", 1, payload);
        let json_output = serde_json::to_string_pretty(&output)
            .map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))?;
        println!("{}", json_output);
    } else {
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

        println!("\nChanges detected. Run without --check to apply.");
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
    /// Prepare a release PR branch without tags or publish.
    pub pr: bool,
    /// Wait for exact-SHA remote checks to settle.
    pub wait: bool,
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
    let effective_skip_publish = skip_publish || args.pr;
    let effective_skip_tag = args.skip_tag || args.pr;
    let (plan, expected_mutation_plan) = plan_release_operation(
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
            println!(
                "{}",
                plan.format_summary_with_flags(skip_publish || args.pr, args.skip_tag || args.pr)
            );
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
    if args.pr {
        publisher.execute_pr(&transaction_id, &plan, &planned_paths, &allowed_unstaged_paths)?;
    } else {
        publisher.execute(
            &transaction_id,
            &plan,
            skip_publish,
            args.skip_tag,
            args.wait,
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

    let mut extended_results = Vec::with_capacity(publishable_crates.len());
    let mut has_extended_failures = false;
    if extended {
        if !json {
            println!("\nrunning extended checks...");
        }
        for (crate_name, checks) in validator.validate_extended(&publishable_crates, release_config)? {
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
                has_extended_failures |= !check.passed && !check.is_skipped();
                crate_checks.push(serde_json::json!({
                  "check": check.check_name,
                  "passed": check.passed,
                  "skipped": check.is_skipped(),
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

    let validation_failed = has_extended_failures || has_change_file_failures || has_shallow_failures;
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

/// Finalize a merged release PR through exact-SHA checks, publication, tags, and forge releases.
pub fn run_release_finalize(ctx: &WorkspaceContext, options: ReleaseFinalizeOptions) -> RailResult<()> {
    let ReleaseFinalizeOptions {
        crate_names,
        all,
        publish,
        skip_tag,
        include_dependents,
        yes,
        allow_non_default_branch,
        format,
    } = options;
    ctx.snapshot()?;
    let json = format.is_json();

    let release_config = ctx
        .config()
        .as_ref()
        .map(|config| &config.release)
        .ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;
    let skip_publish = registry_publication_skipped(publish, release_config)?;

    let workspace_members = ctx.graph().workspace_members();
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
    validator.validate(&target_crates, true)?;
    if let Some(warning) = validator.validate_branch(allow_non_default_branch)? {
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
    if !yes && io::stdin().is_terminal() && !json && !crate::utils::prompt_for_confirmation()? {
        println!("cancelled");
        return Ok(());
    }
    let publisher = ReleasePublisher::new(ctx, release_config);
    let generated_transaction_id =
        build_release_mutation_plan(ctx, &plan, skip_publish, skip_tag, false, release_config)?.operation_id;
    let prepared = prepared_release_transaction(ctx, &plan)?;
    if let Some(transaction) = prepared.as_ref() {
        validate_prepared_release_merge(ctx.workspace_root(), transaction)?;
    }
    let transaction_id = prepared
        .map(|transaction| transaction.transaction_id)
        .unwrap_or(generated_transaction_id);
    publisher.execute_finalize(&transaction_id, &plan, skip_publish, skip_tag)?;

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

fn prepared_release_transaction(
    ctx: &WorkspaceContext,
    plan: &crate::release::planner::ReleasePlan,
) -> RailResult<Option<GitReleaseTransaction>> {
    let expected = plan
        .crates
        .iter()
        .map(|crate_plan| (crate_plan.name.clone(), crate_plan.new_version.to_string()))
        .collect::<BTreeMap<_, _>>();
    let mut candidates = Vec::new();
    for transaction in git_release_transactions(ctx.workspace_root())?
        .into_iter()
        .filter(|transaction| {
            transaction.ambiguity.is_none() && transaction.mode == "prepare" && transaction.crates == expected
        })
    {
        let mut observations = Vec::new();
        match reconcile_prepare_transaction(ctx.workspace_root(), &transaction, &mut observations) {
            PrepareReconciliation::Incomplete => candidates.push(transaction),
            PrepareReconciliation::Terminal(_) => {}
            PrepareReconciliation::Ambiguous => {
                return Err(RailError::with_help(
                    format!(
                        "prepared release transaction '{}' has ambiguous or partial external effects: {}",
                        transaction.transaction_id,
                        observations.join(", ")
                    ),
                    "inspect `cargo rail release status --format json` and select an explicit recovery transaction",
                ));
            }
        }
    }
    if candidates.len() > 1 {
        return Err(RailError::with_help(
            "multiple incomplete prepare transactions match the finalize plan",
            "inspect `cargo rail release status --format json` and remove superseded terminal state before finalizing",
        ));
    }
    Ok(candidates.pop())
}

fn validate_prepared_release_merge(workspace_root: &Path, transaction: &GitReleaseTransaction) -> RailResult<()> {
    let git = crate::git::SystemGit::open(workspace_root)?;
    let head = git.head_commit()?;
    let parents = git.run_git_stdout(&["show", "-s", "--format=%P", &head])?;
    let parents = parents.split_whitespace().collect::<Vec<_>>();
    if parents.len() < 2 {
        return Err(RailError::with_help(
            format!(
                "release finalize expected HEAD {} to be the merge commit introducing prepared transaction '{}'",
                head, transaction.transaction_id
            ),
            "merge the generated release PR with a merge commit, check out that exact commit, and retry",
        ));
    }

    let prepared = transaction.exact_sha.as_str();
    let first_parent_contains_prepare = git.run_git_check(&["merge-base", "--is-ancestor", prepared, parents[0]]);
    let merged_parent_contains_prepare = parents[1..]
        .iter()
        .any(|parent| git.run_git_check(&["merge-base", "--is-ancestor", prepared, parent]));
    if first_parent_contains_prepare || !merged_parent_contains_prepare {
        return Err(RailError::with_help(
            format!(
                "HEAD {} is not the merge boundary introducing prepared transaction '{}' at {}",
                head, transaction.transaction_id, prepared
            ),
            "check out the exact merge commit that introduced the generated release PR; do not finalize from a later commit",
        ));
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
            reports.push(reconstructed_status_report(workspace_root, transaction));
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
        println!(
            "  Last completed effect: {}",
            report.completed_effect.as_deref().unwrap_or("none")
        );
        println!("  Next effect: {}", report.next_effect.as_deref().unwrap_or("none"));
        println!("  Ambiguous: {}", if report.ambiguity { "yes" } else { "no" });
        println!("  Action: {}", report.safe_operator_command);
        if crate::output::is_verbose() {
            println!("  Exact SHA: {}", report.exact_sha.as_deref().unwrap_or("not prepared"));
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
    for crate_state in &state.crates {
        record(
            format!("local preparation commit for {}", crate_state.name),
            crate_state.commit.status,
            crate_state.commit.object.as_deref(),
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
            format!("irreversible registry publication for {}", crate_state.name),
            crate_state.publication.status,
            crate_state.publication.object.as_deref(),
        );
    }
    for crate_state in &state.crates {
        record(
            format!("local Git tag for {}", crate_state.name),
            crate_state.tag.status,
            crate_state.tag.object.as_deref(),
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

    let abort_in_progress = state.abort.status == StepStatus::InProgress;
    if state.status == ReleaseStatus::Aborted {
        ambiguity = false;
    }
    let (recoverability, safe_operator_command) = match state.status {
        ReleaseStatus::Active if abort_in_progress => (
            "reconcile_abort".to_string(),
            format!("cargo rail release abort {} --yes", path.display()),
        ),
        ReleaseStatus::Active => (
            if ambiguity { "reconcile" } else { "resumable" }.to_string(),
            format!("cargo rail release resume {}", path.display()),
        ),
        ReleaseStatus::Complete | ReleaseStatus::Aborted => (
            "terminal".to_string(),
            format!("cargo rail clean --release-journal {}", state.transaction_id),
        ),
    };
    ReleaseStatusReport {
        transaction_id: state.transaction_id,
        state: format!("{}:{:?}", state.phase.as_str(), state.status).to_ascii_lowercase(),
        exact_sha: state.release_commit,
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

fn reconstructed_status_report(workspace_root: &Path, transaction: GitReleaseTransaction) -> ReleaseStatusReport {
    if let Some(reason) = transaction.ambiguity.clone() {
        return ReleaseStatusReport {
            transaction_id: transaction.transaction_id,
            state: "reconstructed:ambiguous".to_string(),
            exact_sha: Some(transaction.exact_sha),
            completed_effect: Some("release_commit".to_string()),
            next_effect: None,
            observations: vec![format!("git:ambiguous={reason}")],
            ambiguity: true,
            recoverability: "unreadable".to_string(),
            safe_operator_command: "inspect the conflicting Rail-Release commit trailers".to_string(),
            journal: None,
        };
    }
    let prepare = transaction.mode == "prepare";
    let mut observations = transaction
        .crates
        .iter()
        .map(|(name, version)| format!("git:complete={}@{}", name, version))
        .chain(std::iter::once(format!("git:complete={}", transaction.exact_sha)))
        .collect::<Vec<_>>();
    let prepare_reconciliation =
        prepare.then(|| reconcile_prepare_transaction(workspace_root, &transaction, &mut observations));
    let (terminal, ambiguous, effective_transaction) = match prepare_reconciliation {
        Some(PrepareReconciliation::Terminal(normalized)) => (true, false, *normalized),
        Some(PrepareReconciliation::Incomplete) => (false, false, transaction.clone()),
        Some(PrepareReconciliation::Ambiguous) => (false, true, transaction.clone()),
        None => (
            reconstructed_release_is_terminal(workspace_root, &transaction, &mut observations),
            false,
            transaction.clone(),
        ),
    };
    let safe_operator_command = if terminal {
        "none (transaction is terminal; no release journal exists)".to_string()
    } else if prepare && !ambiguous {
        format!(
            "cargo rail release finalize {} --yes",
            transaction.crates.keys().cloned().collect::<Vec<_>>().join(" ")
        )
    } else if prepare {
        format!("cargo rail release status {} --format json", transaction.transaction_id)
    } else {
        format!("cargo rail release resume {}", transaction.transaction_id)
    };
    ReleaseStatusReport {
        transaction_id: transaction.transaction_id,
        state: if terminal {
            "released:git".to_string()
        } else if prepare {
            "prepared:git".to_string()
        } else {
            "reconstructed:active".to_string()
        },
        exact_sha: Some(effective_transaction.exact_sha),
        completed_effect: Some(if terminal {
            "release".to_string()
        } else {
            "release_commit".to_string()
        }),
        next_effect: if terminal {
            None
        } else {
            Some(if prepare {
                "finalize".to_string()
            } else {
                "reconcile_remote_truth".to_string()
            })
        },
        observations,
        ambiguity: ambiguous || !prepare && !terminal,
        recoverability: if terminal {
            "terminal"
        } else if prepare && !ambiguous {
            "finalizable"
        } else if prepare {
            "reconcile"
        } else {
            "reconstructable"
        }
        .to_string(),
        safe_operator_command,
        journal: None,
    }
}

fn reconcile_prepare_transaction(
    workspace_root: &Path,
    transaction: &GitReleaseTransaction,
    observations: &mut Vec<String>,
) -> PrepareReconciliation {
    if transaction.tags.len() != transaction.crates.len() || transaction.tags.is_empty() {
        observations.push("prepare:ambiguous=incomplete_tag_identity".to_string());
        return PrepareReconciliation::Ambiguous;
    }
    let remote = match transaction.remote.as_deref() {
        Some(remote) => remote,
        None => {
            observations.push("prepare:ambiguous=missing_remote_intent".to_string());
            return PrepareReconciliation::Ambiguous;
        }
    };
    let mut missing = 0usize;
    let mut targets = BTreeSet::new();
    for tag in transaction.tags.values() {
        match prepared_tag_target(workspace_root, remote, tag) {
            Ok(Some(target)) => {
                observations.push(format!("tag:observed={tag}@{target}"));
                targets.insert(target);
            }
            Ok(None) => {
                observations.push(format!("tag:missing={tag}"));
                missing = missing.saturating_add(1);
            }
            Err(()) => {
                observations.push(format!("tag:ambiguous={tag}"));
                return PrepareReconciliation::Ambiguous;
            }
        }
    }
    if missing == transaction.tags.len() {
        return PrepareReconciliation::Incomplete;
    }
    if remote != "none" {
        let Some(expected) = transaction.remote_repository.as_ref() else {
            observations.push("remote_repository:ambiguous=missing_transaction_identity".to_string());
            return PrepareReconciliation::Ambiguous;
        };
        match release_repository(workspace_root) {
            Ok(actual) if &actual == expected => {}
            Ok(_) | Err(_) => {
                observations.push("remote_repository:ambiguous=changed_or_unresolvable".to_string());
                return PrepareReconciliation::Ambiguous;
            }
        }
    }
    if missing != 0 || targets.len() != 1 {
        observations.push("prepare:ambiguous=partial_or_divergent_tags".to_string());
        return PrepareReconciliation::Ambiguous;
    }
    let release_target = targets.into_iter().next().unwrap_or_default();
    if release_target.is_empty()
        || !command_succeeds(
            workspace_root,
            "git",
            &["merge-base", "--is-ancestor", &transaction.exact_sha, &release_target],
        )
        || !command_succeeds(
            workspace_root,
            "git",
            &["merge-base", "--is-ancestor", &release_target, "HEAD"],
        )
    {
        observations.push("prepare:ambiguous=tag_target_not_merged_descendant".to_string());
        return PrepareReconciliation::Ambiguous;
    }
    observations.push(format!("prepare:merged_target={release_target}"));
    let mut normalized = transaction.clone();
    normalized.exact_sha = release_target.clone();
    normalized.tag = Some(true);
    if normalized.publish.is_none() {
        normalized.publish = Some(normalized.crate_publish.values().any(|publish| *publish));
    }
    normalized.commit_targets = normalized
        .crates
        .keys()
        .map(|name| (name.clone(), release_target.clone()))
        .collect();
    if reconstructed_release_is_terminal(workspace_root, &normalized, observations) {
        PrepareReconciliation::Terminal(Box::new(normalized))
    } else {
        observations.push("prepare:ambiguous=external_effects_incomplete_or_unavailable".to_string());
        PrepareReconciliation::Ambiguous
    }
}

fn prepared_tag_target(workspace_root: &Path, remote: &str, tag: &str) -> Result<Option<String>, ()> {
    if remote == "none" {
        let output = Command::new("git")
            .current_dir(workspace_root)
            .args(["rev-parse", "--verify", &format!("refs/tags/{tag}^{{commit}}")])
            .output()
            .map_err(|_| ())?;
        if output.status.success() {
            let target = String::from_utf8(output.stdout).map_err(|_| ())?.trim().to_string();
            return if target.is_empty() { Err(()) } else { Ok(Some(target)) };
        }
        return Ok(None);
    }
    let output = Command::new("git")
        .current_dir(workspace_root)
        .args([
            "ls-remote",
            "origin",
            &format!("refs/tags/{tag}"),
            &format!("refs/tags/{tag}^{{}}"),
        ])
        .output()
        .map_err(|_| ())?;
    if !output.status.success() {
        return Err(());
    }
    let stdout = String::from_utf8(output.stdout).map_err(|_| ())?;
    let peeled = format!("refs/tags/{tag}^{{}}");
    if let Some(target) = stdout.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let target = fields.next()?;
        let reference = fields.next()?;
        (reference == peeled).then(|| target.to_string())
    }) {
        Ok(Some(target))
    } else if stdout.trim().is_empty() {
        Ok(None)
    } else {
        Err(())
    }
}

fn reconstructed_release_is_terminal(
    workspace_root: &Path,
    transaction: &GitReleaseTransaction,
    observations: &mut Vec<String>,
) -> bool {
    let Some(remote) = transaction.remote.as_deref() else {
        return false;
    };
    let remote_repository = if remote == "none" {
        None
    } else {
        let Some(expected) = transaction.remote_repository.as_ref() else {
            observations.push("remote_repository:ambiguous=missing_transaction_identity".to_string());
            return false;
        };
        let Ok(actual) = release_repository(workspace_root) else {
            observations.push("remote_repository:ambiguous=unresolvable".to_string());
            return false;
        };
        if &actual != expected {
            observations.push("remote_repository:ambiguous=changed".to_string());
            return false;
        }
        Some(expected)
    };
    let tags_complete = match transaction.tag {
        Some(true) => {
            if transaction.tags.len() != transaction.crates.len() {
                return false;
            }
            transaction.tags.iter().all(|(name, tag)| {
                let Some(expected) = transaction.commit_targets.get(name) else {
                    return false;
                };
                let observed = if remote == "none" {
                    git_stdout(
                        workspace_root,
                        &["rev-parse", "--verify", &format!("refs/tags/{}^{{commit}}", tag)],
                    )
                } else {
                    git_stdout(
                        workspace_root,
                        &["ls-remote", "origin", &format!("refs/tags/{}^{{}}", tag)],
                    )
                    .and_then(|output| output.split_whitespace().next().map(str::to_string))
                };
                if observed.as_deref() == Some(expected) {
                    observations.push(format!("tag:complete={}", tag));
                    true
                } else {
                    false
                }
            })
        }
        Some(false) => true,
        None => return false,
    };
    if !tags_complete {
        return false;
    }

    let readiness_required = remote != "none" && (transaction.tag == Some(true) || transaction.publish == Some(true));
    if readiness_required {
        let Some(provider) = exact_sha_readiness_provider(remote_repository, remote) else {
            observations.push("readiness:ambiguous=unsupported_provider".to_string());
            return false;
        };
        let readiness = match provider {
            "github" => observe_github_exact_sha_readiness(workspace_root, &transaction.exact_sha),
            "gitlab" => observe_gitlab_exact_sha_readiness(workspace_root, &transaction.exact_sha),
            _ => return false,
        };
        match readiness {
            Ok(CheckReadiness::Green(detail)) => observations.push(format!("readiness:complete={}", detail)),
            Ok(CheckReadiness::Waiting(detail)) => {
                observations.push(format!("readiness:waiting={}", detail));
                return false;
            }
            Ok(CheckReadiness::Failed(detail)) => {
                observations.push(format!("readiness:failed={}", detail));
                return false;
            }
            Err(_) => {
                observations.push("readiness:ambiguous=provider_unavailable".to_string());
                return false;
            }
        }
    }

    let publication_complete = match transaction.publish {
        Some(false) => true,
        Some(true) => transaction.crates.iter().all(|(name, version)| {
            let Some(publish) = transaction.crate_publish.get(name) else {
                return false;
            };
            if !publish {
                return true;
            }
            let spec = format!("{}@{}", name, version);
            let complete = command_succeeds(workspace_root, "cargo", &["info", "--registry", "crates-io", &spec]);
            if complete {
                observations.push(format!("registry:complete={}", spec));
            }
            complete
        }),
        None => false,
    };
    if !publication_complete {
        return false;
    }

    if transaction.tag == Some(false) && remote != "none" {
        let Some(branch) = git_stdout(workspace_root, &["branch", "--show-current"]) else {
            return false;
        };
        let Some(remote_head) = git_stdout(
            workspace_root,
            &["ls-remote", "origin", &format!("refs/heads/{}", branch)],
        )
        .and_then(|output| output.split_whitespace().next().map(str::to_string)) else {
            return false;
        };
        if remote_head != transaction.exact_sha {
            return false;
        }
        observations.push(format!("remote_commit:complete={}", remote_head));
    }
    if transaction.tag == Some(false) {
        return true;
    }

    let forge = match remote {
        "github" | "gitlab" => Some(remote),
        "auto" => remote_repository.and_then(|repository| match repository.host() {
            Some("github.com") => Some("github"),
            Some("gitlab.com") => Some("gitlab"),
            _ => None,
        }),
        "none" | "push" => return true,
        _ => None,
    };
    let Some(forge) = forge else {
        return false;
    };
    transaction.tags.values().all(|tag| {
        let selector = remote_repository.map(RemoteRepository::selector).unwrap_or_default();
        let complete = match forge {
            "github" => command_succeeds(workspace_root, "gh", &["release", "view", tag, "--repo", &selector]),
            "gitlab" => command_succeeds(workspace_root, "glab", &["release", "view", tag, "--repo", &selector]),
            _ => false,
        };
        if complete {
            observations.push(format!("forge:complete={}", tag));
        }
        complete
    })
}

fn exact_sha_readiness_provider(repository: Option<&RemoteRepository>, remote: &str) -> Option<&'static str> {
    match remote {
        "github" => Some("github"),
        "gitlab" => Some("gitlab"),
        "auto" | "push" => repository.and_then(|repository| match repository.host() {
            Some("github.com") => Some("github"),
            Some("gitlab.com") => Some("gitlab"),
            _ => None,
        }),
        _ => None,
    }
}

fn git_stdout(workspace_root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(workspace_root)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn command_succeeds(workspace_root: &Path, program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .current_dir(workspace_root)
        .args(args)
        .output()
        .is_ok_and(|output| output.status.success())
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
        let Some(mode) = trailer_value(&message, "Rail-Release-Mode") else {
            continue;
        };
        if !matches!(mode.as_str(), "prepare" | "run") {
            continue;
        }
        let index = transactions
            .iter()
            .position(|transaction| transaction.transaction_id == transaction_id);
        let index = if let Some(index) = index {
            index
        } else {
            transactions.push(GitReleaseTransaction {
                transaction_id: transaction_id.clone(),
                exact_sha: sha.clone(),
                mode: mode.clone(),
                publish: parse_bool_trailer(&message, "Rail-Release-Publish"),
                publish_registry: trailer_value(&message, "Rail-Release-Publish-Registry"),
                tag: parse_bool_trailer(&message, "Rail-Release-Tag"),
                remote: trailer_value(&message, "Rail-Release-Remote"),
                remote_repository: trailer_value(&message, "Rail-Release-Repository")
                    .and_then(|value| RemoteRepository::from_trailer(&value).ok()),
                crates: BTreeMap::new(),
                tags: BTreeMap::new(),
                crate_publish: BTreeMap::new(),
                commit_targets: BTreeMap::new(),
                ambiguity: None,
            });
            transactions.len() - 1
        };
        let transaction = &mut transactions[index];
        if transaction.mode == "prepare" && mode != "prepare" {
            transaction.mode = mode;
            transaction.exact_sha = sha.clone();
            transaction.publish = parse_bool_trailer(&message, "Rail-Release-Publish");
            transaction.publish_registry = trailer_value(&message, "Rail-Release-Publish-Registry");
            transaction.tag = parse_bool_trailer(&message, "Rail-Release-Tag");
            transaction.remote = trailer_value(&message, "Rail-Release-Remote");
            transaction.remote_repository = trailer_value(&message, "Rail-Release-Repository")
                .and_then(|value| RemoteRepository::from_trailer(&value).ok());
        } else if mode != "prepare"
            && (transaction.publish != parse_bool_trailer(&message, "Rail-Release-Publish")
                || transaction.publish_registry != trailer_value(&message, "Rail-Release-Publish-Registry")
                || transaction.tag != parse_bool_trailer(&message, "Rail-Release-Tag")
                || transaction.remote != trailer_value(&message, "Rail-Release-Remote")
                || transaction.remote_repository
                    != trailer_value(&message, "Rail-Release-Repository")
                        .and_then(|value| RemoteRepository::from_trailer(&value).ok()))
        {
            transaction.ambiguity = Some("conflicting transaction authority".to_string());
        }
        for value in trailer_values(&message, "Rail-Release-Crate") {
            let Some((name, version)) = value.rsplit_once('@') else {
                continue;
            };
            if transaction
                .crates
                .insert(name.to_string(), version.to_string())
                .is_some_and(|previous| previous != version)
            {
                transaction.ambiguity = Some(format!("conflicting crate identity for {name}"));
            }
            if transaction
                .commit_targets
                .insert(name.to_string(), sha.clone())
                .is_some()
            {
                transaction.ambiguity = Some(format!("duplicate release commit for {name}"));
            }
        }
        for value in trailer_values(&message, "Rail-Release-Tag-Name") {
            let Some((name, tag)) = value.split_once('=') else {
                continue;
            };
            if transaction
                .tags
                .insert(name.to_string(), tag.to_string())
                .is_some_and(|previous| previous != tag)
            {
                transaction.ambiguity = Some(format!("conflicting tag identity for {name}"));
            }
        }
        for value in trailer_values(&message, "Rail-Release-Crate-Publish") {
            let Some((name, publish)) = value.split_once('=') else {
                continue;
            };
            if let Ok(publish) = publish.parse::<bool>()
                && transaction
                    .crate_publish
                    .insert(name.to_string(), publish)
                    .is_some_and(|previous| previous != publish)
            {
                transaction.ambiguity = Some(format!("conflicting publication intent for {name}"));
            }
        }
    }
    Ok(transactions)
}

fn recognized_release_commit(message: &str) -> bool {
    let contract = trailer_values(message, "Rail-Release-Contract");
    if contract == ["1"] {
        return true;
    }
    if !contract.is_empty() {
        return false;
    }
    exact_v0_25_release_commit(message)
}

fn exact_v0_25_release_commit(message: &str) -> bool {
    const KEYS: &[&str] = &[
        "Rail-Release",
        "Rail-Release-Mode",
        "Rail-Release-Publish",
        "Rail-Release-Publish-Registry",
        "Rail-Release-Tag",
        "Rail-Release-Remote",
        "Rail-Release-Repository",
        "Rail-Release-Crate",
        "Rail-Release-Tag-Name",
        "Rail-Release-Crate-Publish",
    ];
    if !exact_v0_25_message_shape(message)
        || message
            .lines()
            .filter_map(|line| line.trim().split_once(": "))
            .any(|(key, _)| key.starts_with("Rail-Release") && !KEYS.contains(&key))
    {
        return false;
    }
    let one = |key| trailer_values(message, key).len() == 1;
    if !one("Rail-Release") || !one("Rail-Release-Mode") || !one("Rail-Release-Remote") {
        return false;
    }
    let Some(transaction) = trailer_value(message, "Rail-Release") else {
        return false;
    };
    if !transaction.starts_with("release-")
        || !transaction
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return false;
    }
    let Some(remote) = trailer_value(message, "Rail-Release-Remote") else {
        return false;
    };
    if !matches!(remote.as_str(), "none" | "push" | "auto" | "github" | "gitlab") {
        return false;
    }
    let Some(mode) = trailer_value(message, "Rail-Release-Mode") else {
        return false;
    };
    let repositories = trailer_values(message, "Rail-Release-Repository");
    let repository_is_valid = |value: &String| RemoteRepository::from_trailer(value).is_ok();
    let exact_repository_shape = match mode.as_str() {
        "run" => {
            (remote == "none" && repositories.is_empty())
                || (remote != "none" && repositories.len() == 1 && repository_is_valid(&repositories[0]))
        }
        // v0.25 PR preparation always recorded the exact origin identity,
        // independent of the configured post-merge remote-effects policy.
        "prepare" => repositories.len() == 1 && repository_is_valid(&repositories[0]),
        _ => false,
    };
    if !exact_repository_shape {
        return false;
    }
    match mode.as_str() {
        "run" => exact_v0_25_run_commit(message),
        "prepare" => exact_v0_25_prepare_commit(message),
        _ => false,
    }
}

fn exact_v0_25_message_shape(message: &str) -> bool {
    let message = message.trim_end_matches(['\r', '\n']);
    let mut lines = message.lines();
    let Some(subject) = lines.next() else {
        return false;
    };
    if subject.is_empty() || lines.next() != Some("") {
        return false;
    }
    let trailers = lines.collect::<Vec<_>>();
    !trailers.is_empty()
        && trailers.iter().all(|line| {
            line.trim() == *line
                && line
                    .split_once(": ")
                    .is_some_and(|(key, value)| key.starts_with("Rail-Release") && !value.is_empty())
        })
}

fn exact_v0_25_run_commit(message: &str) -> bool {
    for key in [
        "Rail-Release-Publish",
        "Rail-Release-Publish-Registry",
        "Rail-Release-Tag",
        "Rail-Release-Crate",
        "Rail-Release-Tag-Name",
        "Rail-Release-Crate-Publish",
    ] {
        if trailer_values(message, key).len() != 1 {
            return false;
        }
    }
    let Some(publish) = parse_bool_trailer(message, "Rail-Release-Publish") else {
        return false;
    };
    if parse_bool_trailer(message, "Rail-Release-Tag").is_none() {
        return false;
    }
    let registry = trailer_value(message, "Rail-Release-Publish-Registry");
    if registry.as_deref() != Some(if publish { "crates-io" } else { "none" }) {
        return false;
    }
    let Some((name, version)) = trailer_value(message, "Rail-Release-Crate").and_then(|value| {
        let (name, version) = value.rsplit_once('@')?;
        Some((name.to_string(), version.to_string()))
    }) else {
        return false;
    };
    if name.is_empty() || version.parse::<semver::Version>().is_err() {
        return false;
    }
    let tag_matches = trailer_value(message, "Rail-Release-Tag-Name").and_then(|value| {
        let (crate_name, tag) = value.split_once('=')?;
        Some(crate_name == name && !tag.is_empty())
    }) == Some(true);
    let publish_matches = trailer_value(message, "Rail-Release-Crate-Publish").and_then(|value| {
        let (crate_name, crate_publish) = value.split_once('=')?;
        let crate_publish = crate_publish.parse::<bool>().ok()?;
        Some(crate_name == name && (publish || !crate_publish))
    }) == Some(true);
    message.lines().next() == Some(format!("chore(release): {name} v{version}").as_str())
        && tag_matches
        && publish_matches
}

fn exact_v0_25_prepare_commit(message: &str) -> bool {
    if [
        "Rail-Release-Publish",
        "Rail-Release-Publish-Registry",
        "Rail-Release-Tag",
    ]
    .into_iter()
    .any(|key| !trailer_values(message, key).is_empty())
    {
        return false;
    }
    let subject = message.lines().next().unwrap_or_default();
    let branch_hash = subject.strip_prefix("chore(release): prepare rail/release-");
    if !branch_hash.is_some_and(|hash| {
        hash.len() == 8
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) {
        return false;
    }
    let crates = trailer_values(message, "Rail-Release-Crate");
    let tags = trailer_values(message, "Rail-Release-Tag-Name");
    let publication = trailer_values(message, "Rail-Release-Crate-Publish");
    if crates.is_empty() || crates.len() != tags.len() || crates.len() != publication.len() {
        return false;
    }
    let crate_names = crates
        .iter()
        .filter_map(|value| {
            let (name, version) = value.rsplit_once('@')?;
            (!name.is_empty() && version.parse::<semver::Version>().is_ok()).then_some(name)
        })
        .collect::<BTreeSet<_>>();
    if crate_names.len() != crates.len() {
        return false;
    }
    let tag_names = tags
        .iter()
        .filter_map(|value| {
            let (name, tag) = value.split_once('=')?;
            (!tag.is_empty()).then_some(name)
        })
        .collect::<BTreeSet<_>>();
    let publication_names = publication
        .iter()
        .filter_map(|value| {
            let (name, publish) = value.split_once('=')?;
            matches!(publish, "true" | "false").then_some(name)
        })
        .collect::<BTreeSet<_>>();
    crate_names == tag_names && crate_names == publication_names
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

fn parse_bool_trailer(message: &str, key: &str) -> Option<bool> {
    match trailer_value(message, key)?.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Resume an interrupted release from durable state.
pub fn run_release_resume(ctx: &WorkspaceContext, state: &std::path::Path) -> RailResult<()> {
    let release_config = ctx
        .config()
        .as_ref()
        .map(|config| &config.release)
        .ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;
    let publisher = ReleasePublisher::new(ctx, release_config);
    if state.exists() {
        return publisher.resume(state);
    }
    let transaction_id = state.to_str().filter(|value| {
        value.starts_with("release-")
            && !value.contains(std::path::MAIN_SEPARATOR)
            && value.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    });
    let Some(transaction_id) = transaction_id else {
        return Err(RailError::with_help(
            format!("release state '{}' does not exist", state.display()),
            "pass an existing journal path or a Rail-Release transaction ID",
        ));
    };
    let transaction = git_release_transactions(ctx.workspace_root())?
        .into_iter()
        .find(|transaction| transaction.transaction_id == transaction_id)
        .ok_or_else(|| {
            RailError::with_help(
                format!("release transaction '{}' is not reachable from HEAD", transaction_id),
                "fetch and check out the exact release commit before reconstructing its journal",
            )
        })?;
    if let Some(reason) = transaction.ambiguity.as_deref() {
        return Err(RailError::with_help(
            format!("release transaction '{transaction_id}' is ambiguous: {reason}"),
            "recover the original local journal; conflicting commit trailers are not execution authority",
        ));
    }
    if transaction.mode == "prepare" {
        return Err(RailError::with_help(
            format!("release transaction '{}' is prepared but not finalized", transaction_id),
            "run the safe command printed by 'cargo rail release status'",
        ));
    }
    let publish = transaction.publish.ok_or_else(|| {
        RailError::with_help(
            format!("release transaction '{}' has no publish intent", transaction_id),
            "recover the original local journal; incomplete trailers do not authorize irreversible effects",
        )
    })?;
    if publish {
        let registry = transaction.publish_registry.as_deref().ok_or_else(|| {
            RailError::with_help(
                format!("release transaction '{}' has no registry authority", transaction_id),
                "recover the original local journal; cargo-rail will not guess an irreversible registry target",
            )
        })?;
        if release_config.registry_publication.registry() != Some(registry) {
            return Err(RailError::with_help(
                format!(
                    "transaction authorizes registry '{}', but release.registry_publication resolves to '{}'",
                    registry,
                    release_config.registry_publication.registry().unwrap_or("none")
                ),
                "restore the release configuration from the exact release commit",
            ));
        }
    }
    let tag = transaction.tag.ok_or_else(|| {
        RailError::with_help(
            format!("release transaction '{}' has no tag intent", transaction_id),
            "recover the original local journal; incomplete trailers do not authorize irreversible effects",
        )
    })?;
    let remote = transaction.remote.as_deref().ok_or_else(|| {
        RailError::with_help(
            format!("release transaction '{}' has no remote intent", transaction_id),
            "recover the original local journal; incomplete trailers do not authorize remote effects",
        )
    })?;
    if remote != release_config.remote_effects.as_str() {
        return Err(RailError::with_help(
            format!(
                "transaction authorizes release.remote_effects = '{}', but the checkout resolves '{}'",
                remote,
                release_config.remote_effects.as_str()
            ),
            "restore the release configuration from the exact release commit",
        ));
    }
    let remote_repository = if remote == "none" {
        None
    } else {
        Some(transaction.remote_repository.clone().ok_or_else(|| {
            RailError::with_help(
                format!(
                    "release transaction '{}' predates exact repository identity",
                    transaction_id
                ),
                "recover the original local journal; cargo-rail will not guess an irreversible remote target",
            )
        })?)
    };
    let head = ctx.git()?.git().head_commit()?;
    if head != transaction.exact_sha {
        return Err(RailError::with_help(
            format!(
                "journal reconstruction requires exact release commit {}, but HEAD is {}",
                transaction.exact_sha, head
            ),
            format!(
                "check out the release branch at {} before resuming",
                transaction.exact_sha
            ),
        ));
    }
    let crate_names = transaction.crates.keys().cloned().collect::<Vec<_>>();
    if crate_names.is_empty() {
        return Err(RailError::message(format!(
            "release transaction '{}' records no crate identities",
            transaction_id
        )));
    }
    let plan = ReleasePlanner::new(ctx, release_config)
        .recovery_plan(Some(crate_names), DependentPolicy::RejectPartialClosure)?;
    for crate_plan in &plan.crates {
        let expected = transaction
            .crates
            .get(&crate_plan.name)
            .ok_or_else(|| RailError::message(format!("transaction has no version for crate '{}'", crate_plan.name)))?;
        if expected != &crate_plan.new_version.to_string() {
            return Err(RailError::with_help(
                format!(
                    "transaction records {}@{}, but the checkout resolves version {}",
                    crate_plan.name, expected, crate_plan.new_version
                ),
                "check out the exact release commit and restore its release configuration",
            ));
        }
        let expected_tag = transaction.tags.get(&crate_plan.name).ok_or_else(|| {
            RailError::message(format!(
                "transaction has no tag identity for crate '{}'",
                crate_plan.name
            ))
        })?;
        if expected_tag != &crate_plan.tag_name {
            return Err(RailError::with_help(
                format!(
                    "transaction records tag '{}' for {}, but the checkout resolves '{}'",
                    expected_tag, crate_plan.name, crate_plan.tag_name
                ),
                "restore the release tag format from the exact release commit",
            ));
        }
        let authorized_publish = transaction.crate_publish.get(&crate_plan.name).ok_or_else(|| {
            RailError::message(format!(
                "transaction has no package publication intent for crate '{}'",
                crate_plan.name
            ))
        })?;
        if *authorized_publish != (publish && crate_plan.publish) {
            return Err(RailError::with_help(
                format!("package publication intent changed for '{}'", crate_plan.name),
                "restore Cargo publish metadata and release configuration from the exact release commit",
            ));
        }
    }
    publisher.reconstruct(
        transaction_id,
        &plan,
        !publish,
        !tag,
        ReconstructedRelease {
            release_commit: transaction.exact_sha,
            commit_targets: transaction.commit_targets,
            remote_repository,
        },
    )
}

/// Abort an active release before any external side effect has occurred.
pub fn run_release_abort(ctx: &WorkspaceContext, state: &std::path::Path, yes: bool) -> RailResult<()> {
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
            for update in &plan.change_files_to_update {
                actions.push(
                    MutationAction::new("UPDATE_CHANGE_FILE", update.path.display().to_string(), None)
                        .with_payload(serde_json::json!({ "path": update.path, "content": update.content }))
                        .with_mutations(vec![release_mutation(ctx, &update.path, MutationEffect::Write)?]),
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
        if index + 1 == plan.crates.len() {
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
        }
    }

    if plan.crates.is_empty() {
        // Match `release run`: an empty release plan exits before any local or
        // external action is authorized.
    } else if pr {
        actions.push(
            MutationAction::new("COMMIT_RELEASE_PR", "release-pr", None)
                .with_payload(serde_json::json!({ "crates": plan.canonical_crate_order })),
        );
        actions.push(
            MutationAction::new("PUSH_RELEASE_PR", "origin", None)
                .with_payload(serde_json::json!({ "remote": "origin" })),
        );
        actions.push(MutationAction::new("OPEN_RELEASE_PR", "origin", None));
    } else {
        if release_config.remote_effects.pushes() {
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
              serde_json::json!({ "forge": release_forge_detail(release_config.remote_effects), "tag": crate_plan.tag_name }),
            ),
          );
                }
                for crate_plan in &plan.crates {
                    actions.push(
            MutationAction::new("PUBLISH_FORGE_RELEASE", crate_plan.tag_name.clone(), None).with_payload(
              serde_json::json!({ "forge": release_forge_detail(release_config.remote_effects), "tag": crate_plan.tag_name }),
            ),
          );
                }
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

    mutation::build_plan_with_inputs(ctx, "release", actions, release_declared_inputs(ctx)?, risks, trace)
}

fn release_declared_inputs(ctx: &WorkspaceContext) -> RailResult<Vec<MutationInput>> {
    let git = ctx.git()?.git();
    let git_root = &git.worktree_root;
    let mut paths = Vec::new();
    if let Some(config_path) = crate::config::RailConfig::find_config_path(ctx.workspace_root()) {
        paths.push(config_path);
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

    fn prepared_transaction(exact_sha: String) -> GitReleaseTransaction {
        GitReleaseTransaction {
            transaction_id: "release-test-merge".to_string(),
            exact_sha,
            mode: "prepare".to_string(),
            publish: None,
            publish_registry: None,
            tag: None,
            remote: Some("push".to_string()),
            remote_repository: None,
            crates: BTreeMap::new(),
            tags: BTreeMap::new(),
            crate_publish: BTreeMap::new(),
            commit_targets: BTreeMap::new(),
            ambiguity: None,
        }
    }

    fn prepared_merge_fixture() -> (tempfile::TempDir, GitReleaseTransaction) {
        let root = tempfile::tempdir().unwrap();
        test_git(root.path(), &["init", "-q", "-b", "main"]);
        test_git(root.path(), &["config", "user.name", "Cargo-Rail Test"]);
        test_git(root.path(), &["config", "user.email", "cargo-rail@example.invalid"]);
        test_git(root.path(), &["commit", "--allow-empty", "-qm", "initial"]);
        test_git(root.path(), &["switch", "-qc", "rail/release-test"]);
        test_git(root.path(), &["commit", "--allow-empty", "-qm", "prepare"]);
        let prepared = test_git(root.path(), &["rev-parse", "HEAD"]);
        test_git(root.path(), &["switch", "-q", "main"]);
        test_git(
            root.path(),
            &["merge", "--no-ff", "-qm", "Merge release PR", "rail/release-test"],
        );
        (root, prepared_transaction(prepared))
    }

    const V0_25_RUN: &str = "chore(release): fixture-crate v0.1.1\n\n\
Rail-Release: release-v025-fixture\n\
Rail-Release-Mode: run\n\
Rail-Release-Publish: false\n\
Rail-Release-Publish-Registry: none\n\
Rail-Release-Tag: true\n\
Rail-Release-Remote: none\n\
Rail-Release-Crate: fixture-crate@0.1.1\n\
Rail-Release-Tag-Name: fixture-crate=v0.1.1\n\
Rail-Release-Crate-Publish: fixture-crate=false";

    #[test]
    fn exact_v0_25_run_trailers_are_reconstructable() {
        assert!(recognized_release_commit(V0_25_RUN));
    }

    #[test]
    fn incomplete_or_ambiguous_v0_25_trailers_are_not_authority() {
        for message in [
            V0_25_RUN.replace("Rail-Release-Tag: true\n", ""),
            format!("{V0_25_RUN}\nRail-Release-Tag: false"),
            format!("{V0_25_RUN}\nRail-Release-Future: value"),
            V0_25_RUN.replace(
                "Rail-Release-Publish-Registry: none",
                "Rail-Release-Publish-Registry: crates-io",
            ),
            V0_25_RUN.replace(
                "Rail-Release-Crate-Publish: fixture-crate=false",
                "Rail-Release-Crate-Publish: fixture-crate=true",
            ),
            V0_25_RUN.replace(
                "\n\nRail-Release: release-v025-fixture",
                "\nrelease body\n\nRail-Release: release-v025-fixture",
            ),
        ] {
            assert!(!recognized_release_commit(&message), "{message}");
        }
    }

    #[test]
    fn exact_v0_25_prepare_trailers_are_reconstructable() {
        let message = "chore(release): prepare rail/release-deadbeef\n\n\
Rail-Release: release-v025-prepare\n\
Rail-Release-Mode: prepare\n\
Rail-Release-Remote: github\n\
Rail-Release-Repository: {\"host\":\"github.com\",\"path\":\"example/fixture\"}\n\
        Rail-Release-Crate: fixture-crate@0.1.1\n\
Rail-Release-Tag-Name: fixture-crate=v0.1.1\n\
Rail-Release-Crate-Publish: fixture-crate=true";
        assert!(recognized_release_commit(message));
        assert!(recognized_release_commit(
            &message.replace("Rail-Release-Remote: github", "Rail-Release-Remote: none")
        ));
        assert!(!recognized_release_commit(&message.replace(
            "Rail-Release-Repository: {\"host\":\"github.com\",\"path\":\"example/fixture\"}\n",
            ""
        )));
        assert!(!recognized_release_commit(&message.replacen(
            "rail/release-",
            "release-",
            1
        )));
    }

    #[test]
    fn finalize_accepts_only_the_merge_that_introduces_the_prepare_transaction() {
        let (root, transaction) = prepared_merge_fixture();
        validate_prepared_release_merge(root.path(), &transaction).unwrap();

        test_git(root.path(), &["commit", "--allow-empty", "-qm", "later"]);
        let error = validate_prepared_release_merge(root.path(), &transaction).unwrap_err();
        assert!(error.to_string().contains("merge commit introducing"), "{error}");
    }

    #[test]
    fn finalize_rejects_a_later_unrelated_merge() {
        let (root, transaction) = prepared_merge_fixture();
        test_git(root.path(), &["switch", "-qc", "unrelated"]);
        test_git(root.path(), &["commit", "--allow-empty", "-qm", "unrelated"]);
        test_git(root.path(), &["switch", "-q", "main"]);
        test_git(
            root.path(),
            &["merge", "--no-ff", "-qm", "Merge unrelated PR", "unrelated"],
        );

        let error = validate_prepared_release_merge(root.path(), &transaction).unwrap_err();
        assert!(error.to_string().contains("not the merge boundary"), "{error}");
    }
}
