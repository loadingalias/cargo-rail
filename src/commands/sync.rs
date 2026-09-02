//! Synchronize a monorepo with configured split repositories.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use crate::commands::common::{
    SplitMappingSnapshot, SplitSyncConfigBuilder, TextJsonOutputFormat, enforce_safety_gate, origin_authority_json,
    prepared_effect_projection, publication_authority_json, split_mapping_snapshot,
};
use crate::error::{GitError, RailError, RailResult};
use crate::git::SystemGit;
use crate::mutation::git_effect::{GitEffectAudit, GitEffectStore, ordered_mapping_effect_indices};
use crate::mutation::{self, MutationAction, MutationRisk, MutationTrace};
use crate::progress;
use crate::sync::{ConflictStrategy, SyncDirection, SyncEngine, SyncResult};
use crate::utils;
use crate::workspace::WorkspaceContext;
use rayon::prelude::*;

/// Per-crate sync result for structured output
struct CrateSyncResult {
    crate_name: String,
    result: SyncResult,
    origin_migrations: usize,
    skipped: bool,
}

/// Arguments for the sync command
#[derive(Debug)]
pub struct SyncArgs {
    /// Crate name to sync (mutually exclusive with `all`)
    pub crate_name: Option<String>,
    /// Sync all configured crates
    pub all: bool,
    /// Override remote repository URL
    pub remote: Option<String>,
    /// Sync from remote to monorepo only
    pub from_remote: bool,
    /// Sync from monorepo to remote only
    pub to_remote: bool,
    /// Conflict resolution strategy
    pub strategy: ConflictStrategy,
    /// Check for pending changes without executing
    pub check: bool,
    /// Apply from a previously generated mutation plan file
    pub plan_path: Option<PathBuf>,
    /// Resume from a durable manual-conflict receipt.
    pub resume: Option<PathBuf>,
    /// Allow running on dirty worktree (uncommitted changes)
    pub allow_dirty: bool,
    /// Skip confirmation prompts (for CI/automation)
    pub yes: bool,
    /// Output format
    pub format: TextJsonOutputFormat,
}

/// Run the sync command
pub fn run_sync(ctx: &WorkspaceContext, args: SyncArgs) -> RailResult<()> {
    ctx.snapshot()?;
    let json = args.format.is_json();

    // JSON mode enables structured error output and suppresses progress

    if let Some(receipt) = args.resume.as_deref() {
        return run_sync_resume(ctx, receipt, json);
    }

    // Dirty worktree check (unless --allow-dirty or --check mode)
    if !args.check && !args.allow_dirty {
        let files = ctx
            .changed_source_paths()?
            .into_iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();
        if !files.is_empty() {
            return Err(RailError::Git(GitError::DirtyWorktree { files }));
        }
    }

    let builder = SplitSyncConfigBuilder::new(ctx)?
        .with_crate_or_all(args.crate_name.clone(), args.all)?
        .with_remote_override(args.remote)
        .validate()?;

    let config_count = builder.count();

    if config_count == 0 && args.all {
        if json {
            let output = crate::output::machine_json_envelope(
                "sync",
                "selection",
                "clean",
                0,
                serde_json::json!({ "crates": [], "count": 0 }),
            );
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("No sync operations are configured.");
        }
        return Ok(());
    }

    let direction = match (args.from_remote, args.to_remote) {
        (true, true) => {
            return Err(crate::error::RailError::with_help(
                "cannot use both --from-remote and --to-remote",
                "choose one direction or neither for bidirectional",
            ));
        }
        (true, false) => SyncDirection::RemoteToMono,
        (false, true) => SyncDirection::MonoToRemote,
        (false, false) => SyncDirection::Both,
    };

    let configs = builder.build_sync_configs()?;
    if config_count > 1 && args.all && matches!(&direction, SyncDirection::RemoteToMono | SyncDirection::Both) {
        return Err(RailError::with_help(
            "sync --all cannot combine multiple operations that mutate the monorepo",
            "run each remote-to-monorepo or bidirectional sync separately so every action is planned against its exact source HEAD",
        ));
    }
    for (config, target_exists) in &configs {
        if *target_exists {
            crate::commands::common::validate_existing_target_before_remote_refresh(
                &config.target_repo_path,
                Some(&config.remote_url),
            )?;
        }
    }
    if !args.check {
        enforce_safety_gate(
            "sync apply",
            args.yes,
            args.plan_path.as_deref(),
            std::io::stdin().is_terminal() && !json,
        )?;
        if !args.yes && std::io::stdin().is_terminal() && !json {
            let dir_sym = match direction {
                SyncDirection::MonoToRemote => "->",
                SyncDirection::RemoteToMono => "<-",
                SyncDirection::Both => "<->",
                SyncDirection::None => "-",
            };
            println!(
                "syncing {} crate(s) ({}):\n",
                config_count,
                match direction {
                    SyncDirection::MonoToRemote => "mono -> remote",
                    SyncDirection::RemoteToMono => "remote -> mono",
                    SyncDirection::Both => "bidirectional",
                    SyncDirection::None => "none",
                }
            );
            for (sync_config, target_exists) in &configs {
                let status = if !target_exists { " (missing)" } else { "" };
                println!("  {} {}{}", sync_config.crate_name, dir_sym, status);
            }
            if !utils::prompt_for_confirmation()? {
                println!("cancelled");
                return Ok(());
            }
        }
        for (config, target_exists) in &configs {
            if *target_exists {
                crate::commands::common::validate_existing_target_before_remote_refresh(
                    &config.target_repo_path,
                    Some(&config.remote_url),
                )?;
            }
        }
    }
    let mapping_snapshots = capture_sync_mapping_snapshots(ctx, &configs, &direction)?;
    let selected_repositories = configs
        .iter()
        .map(|(config, _)| (config.crate_name.clone(), config.target_repo_path.display().to_string()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let snapshots = collect_sync_snapshots(ctx, &configs, &mapping_snapshots, &direction, args.strategy)?;
    let expected_mutation_plan = build_sync_mutation_plan(
        ctx,
        &configs,
        &mapping_snapshots,
        &direction,
        args.strategy,
        args.allow_dirty,
    )?;
    let pre_heads = collect_sync_heads(ctx.workspace_root(), &configs);

    // Check mode
    if args.check {
        let pending_commits = configs
            .iter()
            .map(|(config, target_exists)| {
                if !target_exists {
                    return Ok(1);
                }
                let mut engine = SyncEngine::new(ctx, config.clone(), args.strategy)?;
                engine.bind_origin_migration(mapping_snapshots[&config.crate_name].origin_migration.clone())?;
                engine.bind_publication(mapping_snapshots[&config.crate_name].publication.clone())?;
                engine.pending_commit_count(&direction)
            })
            .collect::<RailResult<Vec<_>>>()?;
        revalidate_sync_mapping_snapshots(ctx, &configs, &direction, &mapping_snapshots)?;
        let pending_origin_migrations = configs
            .iter()
            .map(|(config, _)| mapping_snapshots[&config.crate_name].origin_migration.count())
            .collect::<Vec<_>>();
        let pending_publications = configs
            .iter()
            .map(|(config, _)| {
                let mapping = &mapping_snapshots[&config.crate_name];
                let publishes_target = matches!(direction, SyncDirection::MonoToRemote | SyncDirection::Both)
                    || mapping.origin_migration.count() > 0
                    || mapping
                        .publication
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.count() > 0);
                if publishes_target {
                    mapping.publication.as_ref().map_or(0, |snapshot| snapshot.count())
                } else {
                    0
                }
            })
            .collect::<Vec<_>>();
        let has_pending = pending_commits.iter().any(|count| *count > 0)
            || pending_origin_migrations.iter().any(|count| *count > 0)
            || pending_publications.iter().any(|count| *count > 0)
            || mapping_snapshots
                .values()
                .any(|snapshot| !snapshot.prepared_effects.is_empty());
        let result = if has_pending { "pending_changes" } else { "clean" };
        let exit_code = i32::from(has_pending);
        if json {
            let dir_str = direction_name(&direction);

            let crates: Vec<_> = configs
                .iter()
                .enumerate()
                .map(
                    |(index, (sync_config, target_exists))| {
                        let pending_commits = pending_commits[index];
                        let pending_origin_migrations = pending_origin_migrations[index];
                        let pending_publication = pending_publications[index];
                        let migration = &mapping_snapshots[&sync_config.crate_name].origin_migration;
                        serde_json::json!({
                          "crate_name": sync_config.crate_name,
                          "mode": split_mode_name(&sync_config.mode),
                          "target_repo": sync_config.target_repo_path,
                          "branch": sync_config.branch,
                          "remote_url": sync_config.remote_url,
                          "target_exists": target_exists,
                          "pending": pending_commits > 0 || pending_origin_migrations > 0 || pending_publication > 0 || !mapping_snapshots[&sync_config.crate_name].prepared_effects.is_empty(),
                          "pending_commits": pending_commits,
                          "pending_origin_migrations": pending_origin_migrations,
                          "pending_publication_commits": pending_publication,
                          "origin_migration_digest": migration.migration_digest(),
                          "origin_authority": origin_authority_json(migration),
                          "publication_authority": publication_authority_json(mapping_snapshots[&sync_config.crate_name].publication.as_ref()),
                          "prepared_effects": mapping_snapshots[&sync_config.crate_name].prepared_effects,
                        })
                    },
                )
                .collect();

            let payload = serde_json::json!({
              "command": "sync",
              "check": true,
              "direction": dir_str,
              "strategy": strategy_name(args.strategy),
              "crates": crates,
              "count": configs.len(),
              "planning": {
                "source_head": ctx.git()?.git().head_commit().unwrap_or_else(|_| "unknown".to_string()),
                "targets": snapshots,
                "conflict_candidates": compute_conflict_candidates(&configs),
              },
              "mutation_plan": expected_mutation_plan,
            });
            let output = crate::output::machine_json_envelope("sync", "check", result, exit_code, payload);
            println!("{}", serde_json::to_string_pretty(&output)?);
            return if has_pending {
                Err(crate::error::RailError::CheckHasPendingChanges)
            } else {
                Ok(())
            };
        }

        for (index, (sync_config, target_exists)) in configs.iter().enumerate() {
            println!(
                "{}: {}; repository {} ({} pending commit(s), {} pending origin migration(s), {} pending publication commit(s))",
                sync_config.crate_name,
                direction_display(&direction),
                sync_config.target_repo_path.display(),
                pending_commits[index],
                pending_origin_migrations[index],
                pending_publications[index],
            );
            if !target_exists {
                println!("  Warning: target repository is missing; run split first.");
            }
            if crate::output::is_verbose() {
                println!("  Remote: {}", sync_config.remote_url);
                println!("  Conflict strategy: {}", strategy_name(args.strategy));
            }
        }

        if has_pending {
            println!(
                "Next: cargo rail sync {}",
                if args.all {
                    "--all"
                } else {
                    configs[0].0.crate_name.as_str()
                }
            );
            return Err(crate::error::RailError::CheckHasPendingChanges);
        }
        println!("Sync targets are current.");
        return Ok(());
    }

    let mutation_plan = if let Some(path) = args.plan_path.as_ref() {
        let from_file = mutation::read_plan_file(path)?;
        if !from_file.operation_id.starts_with("sync-") {
            return Err(RailError::with_help(
                format!("plan '{}' is not a sync plan", path.display()),
                "generate a sync plan using 'cargo rail sync --check -f json'".to_string(),
            ));
        }
        if validate_sync_requested_operation(&from_file, &expected_mutation_plan)? {
            mutation::validate_pre_apply_with_allowed_paths(ctx, &expected_mutation_plan, std::slice::from_ref(path))?;
            expected_mutation_plan
        } else {
            mutation::validate_pre_apply_with_allowed_paths(ctx, &from_file, std::slice::from_ref(path))?;
            from_file
        }
    } else {
        mutation::validate_pre_apply(ctx, &expected_mutation_plan)?;
        expected_mutation_plan
    };
    revalidate_sync_mapping_snapshots(ctx, &configs, &direction, &mapping_snapshots)?;

    let plan_receipt = mutation::write_receipt(
        ctx.workspace_root(),
        "sync",
        "plan",
        "planned",
        mutation_plan.clone(),
        vec![MutationTrace::new(
            "SYNC_PLAN_CREATED",
            format!("planned sync for {} crate(s)", config_count),
        )],
    )?;
    if crate::output::is_verbose() {
        progress!("plan receipt: {}", plan_receipt.display());
    }

    // Execute syncs and collect per-crate results
    let configs_for_exec = configs.clone();
    let crate_results: Vec<CrateSyncResult> =
        if config_count > 1 && args.all && matches!(direction, SyncDirection::MonoToRemote) {
            progress!("syncing {} crates...", config_count);

            let strategy = args.strategy;
            let results: Vec<RailResult<CrateSyncResult>> = configs_for_exec
                .into_par_iter()
                .map(|(sync_config, target_exists)| {
                    let crate_name = sync_config.crate_name.clone();

                    if !target_exists {
                        if crate::output::is_verbose() {
                            progress!("  {} skipped (run split first)", crate_name);
                        }
                        return Ok(CrateSyncResult {
                            crate_name,
                            result: SyncResult::default(),
                            origin_migrations: 0,
                            skipped: true,
                        });
                    }

                    if crate::output::is_verbose() {
                        progress!("  {}", crate_name);
                    }
                    let origin_migrations = mapping_snapshots[&crate_name].origin_migration.count();
                    let mut engine = SyncEngine::new(ctx, sync_config, strategy)?;
                    engine.bind_origin_migration(mapping_snapshots[&crate_name].origin_migration.clone())?;
                    engine.bind_publication(mapping_snapshots[&crate_name].publication.clone())?;

                    let result = match direction {
                        SyncDirection::MonoToRemote => engine.sync_to_remote()?,
                        SyncDirection::RemoteToMono => engine.sync_from_remote()?,
                        SyncDirection::Both => engine.sync_bidirectional()?,
                        SyncDirection::None => SyncResult::default(),
                    };

                    Ok(CrateSyncResult {
                        crate_name,
                        result,
                        origin_migrations,
                        skipped: false,
                    })
                })
                .collect();

            results.into_iter().collect::<RailResult<Vec<_>>>()?
        } else {
            let mut results = Vec::new();
            for (sync_config, target_exists) in configs_for_exec {
                let crate_name = sync_config.crate_name.clone();

                if !target_exists {
                    if crate::output::is_verbose() {
                        progress!("{} skipped (run split first)", crate_name);
                    }
                    results.push(CrateSyncResult {
                        crate_name,
                        result: SyncResult::default(),
                        origin_migrations: 0,
                        skipped: true,
                    });
                    continue;
                }

                if crate::output::is_verbose() {
                    progress!("syncing {}...", crate_name);
                }
                let origin_migrations = mapping_snapshots[&crate_name].origin_migration.count();
                let mut engine = SyncEngine::new(ctx, sync_config, args.strategy)?;
                engine.bind_origin_migration(mapping_snapshots[&crate_name].origin_migration.clone())?;
                engine.bind_publication(mapping_snapshots[&crate_name].publication.clone())?;

                let result = match direction {
                    SyncDirection::MonoToRemote => engine.sync_to_remote()?,
                    SyncDirection::RemoteToMono => engine.sync_from_remote()?,
                    SyncDirection::Both => engine.sync_bidirectional()?,
                    SyncDirection::None => SyncResult::default(),
                };
                let conflicted = result.status == crate::sync::SyncStatus::Conflicted;

                results.push(CrateSyncResult {
                    crate_name,
                    result,
                    origin_migrations,
                    skipped: false,
                });
                if conflicted {
                    break;
                }
            }
            results
        };

    // Print summary
    print_sync_summary(&crate_results, json, Some(&direction), &selected_repositories)?;
    let post_heads = collect_sync_heads(ctx.workspace_root(), &configs);
    let audit_path =
        write_sync_audit_artifact(ctx.workspace_root(), &configs, &crate_results, &pre_heads, &post_heads)?;
    if crate::output::is_verbose() {
        progress!("sync audit: {}", audit_path.display());
    }
    if crate_results
        .iter()
        .any(|result| result.result.status == crate::sync::SyncStatus::Conflicted)
    {
        return Err(RailError::ExitWithCode { code: 1 });
    }
    let effect_acknowledgements = collect_sync_effect_audits(ctx.workspace_root(), &configs)?;
    let mut apply_trace = vec![
        MutationTrace::new("SYNC_APPLY_STARTED", "started sync apply"),
        MutationTrace::new("SYNC_APPLY_COMPLETED", "completed sync apply"),
    ];
    for (_, audit) in &effect_acknowledgements {
        apply_trace.push(MutationTrace::new(
            "SYNC_GIT_EFFECT_COMPLETED",
            serde_json::to_string(audit).map_err(|error| {
                RailError::message(format!("failed to encode sync Git-effect audit binding: {error}"))
            })?,
        ));
    }
    let apply_receipt = mutation::write_receipt(
        ctx.workspace_root(),
        "sync",
        "apply",
        "applied",
        mutation_plan,
        apply_trace,
    )?;
    acknowledge_sync_effects(&effect_acknowledgements)?;
    if crate::output::is_verbose() {
        progress!("apply receipt: {}", apply_receipt.display());
    }
    if !json {
        println!("Receipt: {}", apply_receipt.display());
    }

    Ok(())
}

fn run_sync_resume(ctx: &WorkspaceContext, receipt: &Path, json: bool) -> RailResult<()> {
    let crate_name = crate::sync::engine::conflict_receipt_crate(ctx.workspace_root(), receipt)?;
    let configs = SplitSyncConfigBuilder::new(ctx)?
        .with_crate_or_all(Some(crate_name.clone()), false)?
        .validate()?
        .build_sync_configs()?;
    let (config, target_exists) = configs
        .into_iter()
        .next()
        .ok_or_else(|| RailError::message(format!("no sync configuration for '{}'", crate_name)))?;
    if !target_exists {
        return Err(RailError::with_help(
            format!("split target for '{}' no longer exists", crate_name),
            "restore the split worktree before resuming",
        ));
    }

    let selected_repositories =
        std::collections::BTreeMap::from([(crate_name.clone(), config.target_repo_path.display().to_string())]);
    let target_repo_path = config.target_repo_path.clone();
    let audit_plan = mutation::build_plan(
        ctx,
        "sync-resume",
        vec![MutationAction::new(
            "SYNC_RESUME",
            crate_name.clone(),
            Some(format!("receipt={}", receipt.display())),
        )],
        Vec::new(),
        vec![MutationTrace::new(
            "SYNC_RESUME_AUDIT_PLANNED",
            "captured authorized conflict-resume state",
        )],
    )?;
    let mut engine = SyncEngine::new(ctx, config, ConflictStrategy::Manual)?;
    let result = engine.resume_from_receipt(receipt)?;
    let results = vec![CrateSyncResult {
        crate_name: crate_name.clone(),
        result,
        origin_migrations: 0,
        skipped: false,
    }];
    print_sync_summary(&results, json, None, &selected_repositories)?;
    if results
        .iter()
        .any(|result| result.result.status == crate::sync::SyncStatus::Conflicted)
    {
        return Err(RailError::ExitWithCode { code: 1 });
    }
    let effect_acknowledgements =
        collect_sync_effect_audits_for_crate(ctx.workspace_root(), &crate_name, &target_repo_path)?;
    let mut trace = vec![MutationTrace::new(
        "SYNC_RESUME_COMPLETED",
        format!("completed conflict resume from {}", receipt.display()),
    )];
    for (_, audit) in &effect_acknowledgements {
        trace.push(MutationTrace::new(
            "SYNC_GIT_EFFECT_COMPLETED",
            serde_json::to_string(audit).map_err(|error| {
                RailError::message(format!(
                    "failed to encode resumed sync Git-effect audit binding: {error}"
                ))
            })?,
        ));
    }
    let apply_receipt = mutation::write_receipt(
        ctx.workspace_root(),
        "sync-resume",
        "apply",
        "applied",
        audit_plan,
        trace,
    )?;
    acknowledge_sync_effects(&effect_acknowledgements)?;
    if crate::output::is_verbose() {
        progress!("apply receipt: {}", apply_receipt.display());
    }
    Ok(())
}

fn collect_sync_effect_audits(
    workspace_root: &Path,
    configs: &[(crate::sync::SyncConfig, bool)],
) -> RailResult<Vec<(PathBuf, GitEffectAudit)>> {
    let mut acknowledgements = Vec::new();
    for (config, target_exists) in configs {
        if !target_exists {
            continue;
        }
        acknowledgements.extend(collect_sync_effect_audits_for_crate(
            workspace_root,
            &config.crate_name,
            &config.target_repo_path,
        )?);
    }
    acknowledgements.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.effect_id.cmp(&right.1.effect_id))
    });
    acknowledgements.dedup_by(|left, right| left.0 == right.0 && left.1.effect_id == right.1.effect_id);
    Ok(acknowledgements)
}

fn collect_sync_effect_audits_for_crate(
    workspace_root: &Path,
    crate_name: &str,
    target_repo_path: &Path,
) -> RailResult<Vec<(PathBuf, GitEffectAudit)>> {
    let target = SystemGit::open(target_repo_path)?;
    let to_remote_prefix = format!("sync-to-remote-{crate_name}-");
    let publication_prefix = format!("sync-publication-{crate_name}-");
    let mut acknowledgements = GitEffectStore::completed_audits_read_only(
        &target,
        crate_name,
        &[
            "origin-migration-",
            to_remote_prefix.as_str(),
            publication_prefix.as_str(),
        ],
    )?
    .into_iter()
    .map(|audit| (target_repo_path.to_path_buf(), audit))
    .collect::<Vec<_>>();

    let source = SystemGit::open(workspace_root)?;
    let from_remote_prefix = format!("sync-from-remote-{crate_name}-");
    acknowledgements.extend(
        GitEffectStore::completed_audits_read_only(&source, crate_name, &[from_remote_prefix.as_str()])?
            .into_iter()
            .map(|audit| (workspace_root.to_path_buf(), audit)),
    );
    Ok(acknowledgements)
}

fn acknowledge_sync_effects(acknowledgements: &[(PathBuf, GitEffectAudit)]) -> RailResult<()> {
    for (repository, audit) in acknowledgements {
        GitEffectStore::acknowledge_completed(&SystemGit::open(repository)?, &audit.effect_id, &audit.payload_digest)?;
    }
    Ok(())
}

/// Print sync results summary
fn print_sync_summary(
    results: &[CrateSyncResult],
    json: bool,
    direction: Option<&SyncDirection>,
    repositories: &std::collections::BTreeMap<String, String>,
) -> RailResult<()> {
    if json {
        let crates: Vec<_> = results
      .iter()
      .map(|r| {
        let conflicts: Vec<_> = r
          .result
          .conflicts
          .iter()
          .map(|c| c.file_path.display().to_string())
          .collect();

        serde_json::json!({
          "crate": r.crate_name,
          "target_repository": repositories.get(&r.crate_name),
          "commits_synced": r.result.commits_synced,
          "origin_migrations": r.origin_migrations,
          "conflicts": conflicts,
          "status": if r.result.status == crate::sync::SyncStatus::Conflicted { "conflicted" } else { "complete" },
          "conflict_receipt": r.result.conflict_receipt,
          "skipped": r.skipped
        })
      })
      .collect();

        let total_commits: usize = results.iter().map(|r| r.result.commits_synced).sum();
        let total_origin_migrations: usize = results.iter().map(|r| r.origin_migrations).sum();
        let total_conflicts: usize = results.iter().map(|r| r.result.conflicts.len()).sum();

        let conflicted = results
            .iter()
            .any(|result| result.result.status == crate::sync::SyncStatus::Conflicted);
        let payload = serde_json::json!({
          "command": "sync",
          "direction": direction.map(direction_name).unwrap_or("resume"),
          "crates": crates,
          "summary": {
            "total_commits": total_commits,
            "total_origin_migrations": total_origin_migrations,
            "total_conflicts": total_conflicts,
            "crates_synced": results.iter().filter(|r| !r.skipped).count(),
            "crates_skipped": results.iter().filter(|r| r.skipped).count()
          }
        });
        let output = crate::output::machine_json_envelope(
            "sync",
            "apply",
            if conflicted { "conflicted" } else { "success" },
            if conflicted { 1 } else { 0 },
            payload,
        );
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    // Text output
    let active_results: Vec<_> = results.iter().filter(|r| !r.skipped).collect();
    let total_commits: usize = active_results.iter().map(|r| r.result.commits_synced).sum();
    let total_origin_migrations: usize = active_results.iter().map(|r| r.origin_migrations).sum();
    let total_conflicts: usize = active_results.iter().map(|r| r.result.conflicts.len()).sum();
    let conflicted = active_results
        .iter()
        .any(|result| result.result.status == crate::sync::SyncStatus::Conflicted);

    for r in &active_results {
        let commit_word = if r.result.commits_synced == 1 {
            "commit"
        } else {
            "commits"
        };
        let repository = repositories.get(&r.crate_name).map(String::as_str).unwrap_or("unknown");
        let direction = direction.map(direction_display).unwrap_or("resumed conflict");
        if r.result.conflicts.is_empty() {
            println!(
                "{}: {}; repository {} ({} {}, {} origin migration(s))",
                r.crate_name, direction, repository, r.result.commits_synced, commit_word, r.origin_migrations,
            );
        } else {
            let conflict_word = if r.result.conflicts.len() == 1 {
                "conflict"
            } else {
                "conflicts"
            };
            println!(
                "{}: {}; repository {} ({} {}, {} origin migration(s), {} {})",
                r.crate_name,
                direction,
                repository,
                r.result.commits_synced,
                commit_word,
                r.origin_migrations,
                r.result.conflicts.len(),
                conflict_word
            );
            for conflict in &r.result.conflicts {
                if crate::output::is_verbose() {
                    println!("  Conflict: {}", conflict.file_path.display());
                }
            }
        }
    }

    // Summary line
    let commit_word = if total_commits == 1 { "commit" } else { "commits" };
    if conflicted {
        println!(
            "Sync conflicted: {} unresolved path(s); no conflicted commit was created.",
            total_conflicts
        );
        for result in &active_results {
            if let Some(receipt) = &result.result.conflict_receipt {
                println!("Conflict receipt: {}", receipt.display());
                println!("Next: cargo rail sync --resume {}", receipt.display());
            }
        }
    } else if total_conflicts > 0 {
        let conflict_word = if total_conflicts == 1 { "conflict" } else { "conflicts" };
        println!(
            "Sync complete: {} {}, {} origin migration(s), {} {}.",
            total_commits, commit_word, total_origin_migrations, total_conflicts, conflict_word
        );
    } else {
        println!(
            "Sync complete: {} {}, {} origin migration(s).",
            total_commits, commit_word, total_origin_migrations
        );
    }

    Ok(())
}

fn split_mode_name(mode: &crate::config::SplitMode) -> &'static str {
    match mode {
        crate::config::SplitMode::Single => "single",
        crate::config::SplitMode::Combined => "combined",
    }
}

fn direction_name(direction: &SyncDirection) -> &'static str {
    match direction {
        SyncDirection::MonoToRemote => "local_to_remote",
        SyncDirection::RemoteToMono => "remote_to_local",
        SyncDirection::Both => "bidirectional",
        SyncDirection::None => "none",
    }
}

fn direction_display(direction: &SyncDirection) -> &'static str {
    match direction {
        SyncDirection::MonoToRemote => "local source -> remote target",
        SyncDirection::RemoteToMono => "remote source -> local target",
        SyncDirection::Both => "local and remote bidirectional",
        SyncDirection::None => "no direction",
    }
}

fn strategy_name(strategy: ConflictStrategy) -> &'static str {
    match strategy {
        ConflictStrategy::Ours => "ours",
        ConflictStrategy::Theirs => "theirs",
        ConflictStrategy::Manual => "manual",
        ConflictStrategy::Union => "union",
    }
}

fn build_sync_mutation_plan(
    ctx: &WorkspaceContext,
    configs: &[(crate::sync::SyncConfig, bool)],
    mapping_snapshots: &std::collections::BTreeMap<String, SplitMappingSnapshot>,
    direction: &SyncDirection,
    strategy: ConflictStrategy,
    allow_dirty: bool,
) -> RailResult<mutation::MutationPlan> {
    let direction_name = match direction {
        SyncDirection::MonoToRemote => "mono_to_remote",
        SyncDirection::RemoteToMono => "remote_to_mono",
        SyncDirection::Both => "bidirectional",
        SyncDirection::None => "none",
    };

    let mut sorted = configs.iter().collect::<Vec<_>>();
    sorted.sort_by(|a, b| a.0.crate_name.cmp(&b.0.crate_name));

    let actions = sorted
        .into_iter()
        .map(|(config, target_exists)| {
            let mapping = &mapping_snapshots[&config.crate_name];
            let source_head = mapping.origin_migration.source_head();
            let target_head = mapping.origin_migration.target_head().unwrap_or("none");
            let publication_count = mapping.publication.as_ref().map_or(0, |snapshot| snapshot.count());
            let publication_digest = mapping
                .publication
                .as_ref()
                .map_or_else(|| "none".to_string(), |snapshot| snapshot.digest());
            let mut payload = serde_json::json!({
                "direction": direction_name,
                "strategy": strategy_name(strategy),
                "target_exists": target_exists,
                "source_head": source_head,
                "target_head": target_head,
                "mapping_count": mapping.mapping_count,
                "origin_migration_count": mapping.origin_migration.count(),
                "origin_migration_digest": mapping.origin_migration.migration_digest(),
                "origin_authority": origin_authority_json(&mapping.origin_migration),
                "publication_count": publication_count,
                "publication_authority": publication_authority_json(mapping.publication.as_ref()),
            });
            if !mapping.prepared_effects.is_empty() {
                payload["prepared_effects"] = serde_json::Value::Array(mapping.prepared_effects.clone());
            }
            Ok(MutationAction::new(
                "SYNC_CRATE",
                config.crate_name.clone(),
                Some(format!(
                    "direction={}, strategy={}, target_exists={}, source_head={}, target_head={}, mapping_count={}, origin_migration_count={}, origin_migration_digest={}, publication_count={}, publication_digest={}",
                    direction_name,
                    strategy_name(strategy),
                    target_exists,
                    source_head,
                    target_head,
                    mapping.mapping_count,
                    mapping.origin_migration.count(),
                    mapping.origin_migration.migration_digest(),
                    publication_count,
                    publication_digest,
                )),
            )
            .with_payload(payload))
        })
        .collect::<RailResult<Vec<_>>>()?;

    let mut risks = Vec::new();
    if allow_dirty {
        risks.push(MutationRisk::new(
            "ALLOW_DIRTY_WORKTREE",
            "medium",
            "sync is allowed on a dirty worktree",
        ));
    }
    if matches!(direction, SyncDirection::Both) {
        risks.push(MutationRisk::new(
            "BIDIRECTIONAL_SYNC",
            "medium",
            "bidirectional sync can create larger conflict surfaces",
        ));
    }

    let trace = vec![MutationTrace::new(
        "SYNC_CONFIGS_RESOLVED",
        format!("resolved {} sync config(s)", configs.len()),
    )];

    mutation::build_plan(ctx, "sync", actions, risks, trace)
}

fn collect_sync_snapshots(
    _ctx: &WorkspaceContext,
    configs: &[(crate::sync::SyncConfig, bool)],
    mapping_snapshots: &std::collections::BTreeMap<String, SplitMappingSnapshot>,
    direction: &SyncDirection,
    strategy: ConflictStrategy,
) -> RailResult<Vec<serde_json::Value>> {
    let direction_name = match direction {
        SyncDirection::MonoToRemote => "mono_to_remote",
        SyncDirection::RemoteToMono => "remote_to_mono",
        SyncDirection::Both => "bidirectional",
        SyncDirection::None => "none",
    };

    configs
        .iter()
        .map(|(config, target_exists)| {
            let mapping = &mapping_snapshots[&config.crate_name];
            Ok(serde_json::json!({
              "crate_name": config.crate_name,
              "direction": direction_name,
              "strategy": strategy_name(strategy),
              "source_head": mapping.origin_migration.source_head(),
              "target_head": mapping.origin_migration.target_head(),
              "target_exists": target_exists,
              "ownership": {
                "snapshot_id": config.ownership.snapshot_id,
                "members": config.ownership.members,
                "dependency_closure": config.ownership.dependency_closure,
                "release_boundaries": config.ownership.release_boundaries.iter().map(|boundary| serde_json::json!({
                  "name": boundary.name,
                  "members": boundary.members,
                })).collect::<Vec<_>>(),
              },
              "mapping_snapshot": {
                "mapping_count": mapping.mapping_count,
                "pending_origin_migrations": mapping.origin_migration.count(),
                "origin_migration_digest": mapping.origin_migration.migration_digest(),
                "origin_authority": origin_authority_json(&mapping.origin_migration),
                "publication_authority": publication_authority_json(mapping.publication.as_ref()),
                "prepared_effects": mapping.prepared_effects,
              },
            }))
        })
        .collect()
}

fn capture_sync_mapping_snapshots(
    ctx: &WorkspaceContext,
    configs: &[(crate::sync::SyncConfig, bool)],
    direction: &SyncDirection,
) -> RailResult<std::collections::BTreeMap<String, SplitMappingSnapshot>> {
    let direction = direction.authority_name();
    configs
        .iter()
        .map(|(config, _)| {
            let mut snapshot = split_mapping_snapshot(
                ctx.workspace_root(),
                &config.crate_name,
                &config.ownership.snapshot_id,
                &config.target_repo_path,
                &config.branch,
                direction,
                "sync",
                Some(&config.remote_url),
            )?;
            let source = ctx.git()?.git();
            let prefix = format!("sync-from-remote-{}-", config.crate_name);
            let source_effects = GitEffectStore::discover_unacknowledged_read_only(source)?
                .into_iter()
                .filter(|journal| journal.operation_id().starts_with(&prefix))
                .collect::<Vec<_>>();
            let source_order = ordered_mapping_effect_indices(&source_effects)?;
            let ordered_source_effects = source_order
                .iter()
                .map(|index| &source_effects[*index])
                .collect::<Vec<_>>();
            let source_repository = crate::git::mappings::repository_identity(ctx.workspace_root())?;
            for (index, journal) in ordered_source_effects.iter().enumerate() {
                let mapping = journal
                    .mapping()
                    .ok_or_else(|| RailError::message("prepared source sync effect has no mapping authority"))?;
                if mapping.owner() != config.crate_name
                    || mapping.ownership_snapshot() != config.ownership.snapshot_id
                    || journal.publication().is_some()
                    || journal.repository().logical_repository != source_repository
                    || (index + 1 < ordered_source_effects.len() && !journal.is_terminal())
                {
                    return Err(RailError::message(
                        "prepared source sync effect changed outside its exact authority",
                    ));
                }
            }
            for pair in ordered_source_effects.windows(2) {
                let previous = pair[0].repository();
                let next = pair[1].repository();
                if previous.common_dir_identity != next.common_dir_identity
                    || previous.worktree_identity != next.worktree_identity
                    || previous.logical_repository != next.logical_repository
                    || previous.object_format != next.object_format
                    || previous.ref_name != next.ref_name
                    || previous.symbolic_head != next.symbolic_head
                    || next.expected_oid.as_deref() != Some(previous.result_oid.as_str())
                {
                    return Err(RailError::message(
                        "prepared source sync effects have a broken repository transition chain",
                    ));
                }
            }
            if let Some(terminal) = ordered_source_effects.last()
                && !terminal.permits_owned_path_recovery_state(source)?
            {
                return Err(RailError::message(
                    "terminal prepared source sync effect changed outside its exact authority",
                ));
            }
            if let (Some(first), Some(terminal)) = (ordered_source_effects.first(), ordered_source_effects.last()) {
                let mapping = first.mapping().expect("ordered source mapping effect");
                let expected_source_head =
                    first.repository().expected_oid.as_deref().ok_or_else(|| {
                        RailError::message("prepared source sync effect has no predecessor source HEAD")
                    })?;
                let selected_target_head = snapshot
                    .origin_migration
                    .target_selected_head()
                    .or_else(|| snapshot.origin_migration.target_head())
                    .ok_or_else(|| RailError::message("prepared source sync effect lost target HEAD authority"))?;
                let (store, authority) = crate::git::mappings::MappingStore::capture_prepared_source_authority_at(
                    ctx.workspace_root(),
                    &config.target_repo_path,
                    &crate::git::mappings::OriginContext::new(
                        crate::git::mappings::repository_identity(ctx.workspace_root())?,
                        &config.crate_name,
                        &config.ownership.snapshot_id,
                    )?,
                    &crate::git::mappings::repository_identity(&config.target_repo_path)?,
                    config.path_capabilities.target_root(),
                    &config.branch,
                    direction,
                    &first.repository().ref_name,
                    expected_source_head,
                    &terminal.repository().result_oid,
                    selected_target_head,
                )?;
                if authority.digest() != mapping.pre_authority() {
                    return Err(RailError::message("prepared source sync mapping pre-authority changed"));
                }
                snapshot.mapping_count = store.count();
                snapshot.origin_migration = authority;
                if let Some(target_pre_authority) = snapshot.prepared_effects.iter().find_map(|effect| {
                    effect
                        .pointer("/mapping/pre_authority")
                        .and_then(serde_json::Value::as_str)
                }) && terminal
                    .mapping()
                    .is_none_or(|mapping| mapping.post_authority() != target_pre_authority)
                {
                    return Err(RailError::message(
                        "prepared source and target sync effects have a broken mapping transition chain",
                    ));
                }
                let mut prepared_effects = ordered_source_effects
                    .iter()
                    .map(|journal| prepared_effect_projection(journal))
                    .collect::<Vec<_>>();
                prepared_effects.append(&mut snapshot.prepared_effects);
                snapshot.prepared_effects = prepared_effects;
            }
            Ok((config.crate_name.clone(), snapshot))
        })
        .collect()
}

fn validate_sync_requested_operation(
    approved: &mutation::MutationPlan,
    expected: &mutation::MutationPlan,
) -> RailResult<bool> {
    let original_error = match mutation::validate_requested_operation(approved, expected) {
        Ok(()) => return Ok(false),
        Err(error) => error,
    };
    let mut pre_effect = expected.clone();
    let mut projected = false;
    for action in &mut pre_effect.actions {
        if let Some(payload) = action.payload.as_object_mut()
            && payload.remove("prepared_effects").is_some()
        {
            projected = true;
        }
    }
    if projected && mutation::validate_requested_operation(approved, &pre_effect).is_ok() {
        return Ok(true);
    }
    Err(original_error)
}

fn revalidate_sync_mapping_snapshots(
    ctx: &WorkspaceContext,
    configs: &[(crate::sync::SyncConfig, bool)],
    direction: &SyncDirection,
    expected: &std::collections::BTreeMap<String, SplitMappingSnapshot>,
) -> RailResult<()> {
    let actual = capture_sync_mapping_snapshots(ctx, configs, direction)?;
    if &actual == expected {
        return Ok(());
    }
    Err(RailError::with_help(
        "sync origin evidence changed after the operation was planned",
        "retry after the ordinary histories and refs/notes/rail mapping refs stop changing",
    ))
}

fn compute_conflict_candidates(configs: &[(crate::sync::SyncConfig, bool)]) -> Vec<serde_json::Value> {
    configs
        .iter()
        .map(|(config, _)| {
            let mut paths = config
                .crate_paths
                .iter()
                .chain(&config.asset_paths)
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            paths.sort();
            paths.dedup();
            serde_json::json!({
              "crate_name": config.crate_name,
              "candidate_paths": paths,
            })
        })
        .collect()
}

fn collect_sync_heads(
    workspace_root: &Path,
    configs: &[(crate::sync::SyncConfig, bool)],
) -> std::collections::BTreeMap<String, (Option<String>, Option<String>)> {
    let mono_head = SystemGit::open(workspace_root).and_then(|git| git.head_commit()).ok();
    let mut out = std::collections::BTreeMap::new();
    for (config, _) in configs {
        let target_head = SystemGit::open(&config.target_repo_path)
            .and_then(|git| git.head_commit())
            .ok();
        out.insert(config.crate_name.clone(), (mono_head.clone(), target_head));
    }
    out
}

fn write_sync_audit_artifact(
    workspace_root: &Path,
    configs: &[(crate::sync::SyncConfig, bool)],
    results: &[CrateSyncResult],
    pre_heads: &std::collections::BTreeMap<String, (Option<String>, Option<String>)>,
    post_heads: &std::collections::BTreeMap<String, (Option<String>, Option<String>)>,
) -> RailResult<PathBuf> {
    let dir = crate::workspace::cargo_rail_state_root(workspace_root).join("receipts");
    std::fs::create_dir_all(&dir)?;
    let nonce = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let path = dir.join(format!("sync-audit-{}.json", nonce));

    let by_crate: Vec<_> = configs
        .iter()
        .map(|(config, _)| {
            let result = results.iter().find(|r| r.crate_name == config.crate_name);
            let pre = pre_heads.get(&config.crate_name).cloned().unwrap_or((None, None));
            let post = post_heads.get(&config.crate_name).cloned().unwrap_or((None, None));
            let conflicts: Vec<String> = result
                .map(|r| {
                    r.result
                        .conflicts
                        .iter()
                        .map(|c| c.file_path.display().to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            serde_json::json!({
              "crate_name": config.crate_name,
              "source_refs": {
                "mono_pre": pre.0,
                "mono_post": post.0,
                "target_pre": pre.1,
                "target_post": post.1,
              },
              "selected_commits": {
                "count": result.map(|r| r.result.commits_synced).unwrap_or(0),
              },
              "produced_commits": {
                "count": result.map(|r| r.result.commits_synced).unwrap_or(0),
              },
              "conflict_outcomes": conflicts,
            })
        })
        .collect();

    let json = serde_json::json!({
      "artifact": "sync_audit",
      "version": 1,
      "generated_at_utc": chrono::Utc::now().to_rfc3339(),
      "crates": by_crate,
    });
    let rendered = serde_json::to_vec_pretty(&json)
        .map_err(|e| RailError::message(format!("failed to serialize sync audit: {}", e)))?;
    std::fs::write(&path, rendered)
        .map_err(|e| RailError::message(format!("failed to write sync audit '{}': {}", path.display(), e)))?;
    Ok(path)
}
