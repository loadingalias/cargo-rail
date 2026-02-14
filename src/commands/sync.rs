//! `cargo rail sync` - Bidirectional sync between monorepo and split repositories.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use crate::commands::common::{OutputFormat, SplitSyncConfigBuilder};
use crate::error::{GitError, RailError, RailResult};
use crate::git::SystemGit;
use crate::git::mappings::MappingStore;
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
  skipped: bool,
}

/// Arguments for the sync command
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
  /// Dry-run mode: preview changes without executing
  pub check: bool,
  /// Apply from a previously generated mutation plan file
  pub plan_path: Option<PathBuf>,
  /// Allow running on dirty worktree (uncommitted changes)
  pub allow_dirty: bool,
  /// Skip confirmation prompts (for CI/automation)
  pub yes: bool,
  /// Output format
  pub format: OutputFormat,
}

/// Run the sync command
pub fn run_sync(ctx: &WorkspaceContext, args: SyncArgs) -> RailResult<()> {
  let json = args.format.is_json();

  // JSON mode enables structured error output and suppresses progress
  if json {
    crate::output::set_json_mode(true);
  }

  // Dirty worktree check (unless --allow-dirty or --check mode)
  if !args.check && !args.allow_dirty && ctx.git.git().is_dirty()? {
    let files = ctx.git.git().dirty_files()?;
    return Err(RailError::Git(GitError::DirtyWorktree { files }));
  }

  let builder = SplitSyncConfigBuilder::new(ctx)?
    .with_crate_or_all(args.crate_name.clone(), args.all)?
    .with_remote_override(args.remote);

  let config_count = builder.count();

  if config_count == 0 && args.all {
    return Err(crate::error::RailError::with_help(
      "no crates configured for sync",
      "run 'cargo rail split init' first",
    ));
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
  let snapshots = collect_sync_snapshots(ctx, &configs, &direction, args.strategy);
  let expected_mutation_plan = build_sync_mutation_plan(ctx, &configs, &direction, args.strategy, args.allow_dirty)?;
  let pre_heads = collect_sync_heads(ctx.workspace_root(), &configs);

  // Check mode
  if args.check {
    if json {
      let dir_str = match direction {
        SyncDirection::MonoToRemote => "to_remote",
        SyncDirection::RemoteToMono => "from_remote",
        SyncDirection::Both => "bidirectional",
        SyncDirection::None => "none",
      };

      let crates: Vec<_> = configs
        .iter()
        .map(|(sync_config, target_exists)| {
          serde_json::json!({
            "crate_name": sync_config.crate_name,
            "mode": format!("{:?}", sync_config.mode),
            "target_repo": sync_config.target_repo_path,
            "branch": sync_config.branch,
            "remote_url": sync_config.remote_url,
            "target_exists": target_exists,
          })
        })
        .collect();

      let output = serde_json::json!({
        "command": "sync",
        "check": true,
        "direction": dir_str,
        "strategy": format!("{:?}", args.strategy).to_lowercase(),
        "crates": crates,
        "count": configs.len(),
        "planning": {
          "source_head": ctx.git.git().head_commit().unwrap_or_else(|_| "unknown".to_string()),
          "targets": snapshots,
          "conflict_candidates": compute_conflict_candidates(&configs),
        },
        "mutation_plan": expected_mutation_plan,
      });
      println!("{}", serde_json::to_string_pretty(&output)?);
      return Ok(());
    }

    let dir_display = match direction {
      SyncDirection::MonoToRemote => "mono -> remote",
      SyncDirection::RemoteToMono => "remote -> mono",
      SyncDirection::Both => "bidirectional",
      SyncDirection::None => "none",
    };

    println!("sync plan:\n");
    for (sync_config, target_exists) in &configs {
      println!("  {}", sync_config.crate_name);
      println!("    direction: {}", dir_display);
      println!("    target: {}", sync_config.target_repo_path.display());
      println!("    remote: {}", sync_config.remote_url);
      println!("    strategy: {}", format!("{:?}", args.strategy).to_lowercase());
      if !target_exists {
        println!("    warning: target repo missing (run split first)");
      }
    }

    println!("\nChanges detected. Run without --check to apply.");
    return Err(crate::error::RailError::CheckHasPendingChanges);
  }

  // Interactive confirmation (unless --yes)
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

    if !utils::prompt_for_confirmation("\nproceed? [Enter/Ctrl+C]")? {
      println!("cancelled");
      return Ok(());
    }
  }

  let mutation_plan = if let Some(path) = args.plan_path.as_ref() {
    let from_file = mutation::read_plan_file(path)?;
    if !from_file.operation_id.starts_with("sync-") {
      return Err(RailError::with_help(
        format!("plan '{}' is not a sync plan", path.display()),
        "generate a sync plan using 'cargo rail sync --check -f json'".to_string(),
      ));
    }
    mutation::validate_pre_apply(ctx, &from_file)?;
    if from_file.inputs_fingerprint != expected_mutation_plan.inputs_fingerprint {
      return Err(RailError::with_help(
        "provided sync plan does not match current requested operation",
        "regenerate the sync plan and rerun with --plan",
      ));
    }
    from_file
  } else {
    mutation::validate_pre_apply(ctx, &expected_mutation_plan)?;
    expected_mutation_plan
  };

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
  progress!("receipt: {}", plan_receipt.display());

  // Execute syncs and collect per-crate results
  let configs_for_exec = configs.clone();
  let crate_results: Vec<CrateSyncResult> = if config_count > 1 && args.all {
    progress!("syncing {} crates...", config_count);

    let strategy = args.strategy;
    let results: Vec<RailResult<CrateSyncResult>> = configs_for_exec
      .into_par_iter()
      .map(|(sync_config, target_exists)| {
        let crate_name = sync_config.crate_name.clone();

        if !target_exists {
          progress!("  {} skipped (run split first)", crate_name);
          return Ok(CrateSyncResult {
            crate_name,
            result: SyncResult::default(),
            skipped: true,
          });
        }

        progress!("  {}", crate_name);
        let mut engine = SyncEngine::new(ctx, sync_config, strategy)?;

        let result = match direction {
          SyncDirection::MonoToRemote => engine.sync_to_remote()?,
          SyncDirection::RemoteToMono => engine.sync_from_remote()?,
          SyncDirection::Both => engine.sync_bidirectional()?,
          SyncDirection::None => SyncResult::default(),
        };

        Ok(CrateSyncResult {
          crate_name,
          result,
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
        progress!("{} skipped (run split first)", crate_name);
        results.push(CrateSyncResult {
          crate_name,
          result: SyncResult::default(),
          skipped: true,
        });
        continue;
      }

      progress!("syncing {}...", crate_name);
      let mut engine = SyncEngine::new(ctx, sync_config, args.strategy)?;

      let result = match direction {
        SyncDirection::MonoToRemote => engine.sync_to_remote()?,
        SyncDirection::RemoteToMono => engine.sync_from_remote()?,
        SyncDirection::Both => engine.sync_bidirectional()?,
        SyncDirection::None => SyncResult::default(),
      };

      results.push(CrateSyncResult {
        crate_name,
        result,
        skipped: false,
      });
    }
    results
  };

  // Print summary
  print_sync_summary(&crate_results, json)?;
  let post_heads = collect_sync_heads(ctx.workspace_root(), &configs);
  let audit_path = write_sync_audit_artifact(ctx.workspace_root(), &configs, &crate_results, &pre_heads, &post_heads)?;
  progress!("sync audit: {}", audit_path.display());
  let apply_receipt = mutation::write_receipt(
    ctx.workspace_root(),
    "sync",
    "apply",
    "applied",
    mutation_plan,
    vec![
      MutationTrace::new("SYNC_APPLY_STARTED", "started sync apply"),
      MutationTrace::new("SYNC_APPLY_COMPLETED", "completed sync apply"),
    ],
  )?;
  progress!("receipt: {}", apply_receipt.display());

  Ok(())
}

/// Print sync results summary
fn print_sync_summary(results: &[CrateSyncResult], json: bool) -> RailResult<()> {
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
          "commits_synced": r.result.commits_synced,
          "conflicts": conflicts,
          "skipped": r.skipped
        })
      })
      .collect();

    let total_commits: usize = results.iter().map(|r| r.result.commits_synced).sum();
    let total_conflicts: usize = results.iter().map(|r| r.result.conflicts.len()).sum();

    let output = serde_json::json!({
      "command": "sync",
      "crates": crates,
      "summary": {
        "total_commits": total_commits,
        "total_conflicts": total_conflicts,
        "crates_synced": results.iter().filter(|r| !r.skipped).count(),
        "crates_skipped": results.iter().filter(|r| r.skipped).count()
      }
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    return Ok(());
  }

  // Text output
  let active_results: Vec<_> = results.iter().filter(|r| !r.skipped).collect();
  let total_commits: usize = active_results.iter().map(|r| r.result.commits_synced).sum();
  let total_conflicts: usize = active_results.iter().map(|r| r.result.conflicts.len()).sum();

  // Per-crate details (only if multiple crates or conflicts)
  if active_results.len() > 1 || total_conflicts > 0 {
    println!();
    for r in &active_results {
      let commit_word = if r.result.commits_synced == 1 {
        "commit"
      } else {
        "commits"
      };
      if r.result.conflicts.is_empty() {
        println!("  {}: {} {}", r.crate_name, r.result.commits_synced, commit_word);
      } else {
        let conflict_word = if r.result.conflicts.len() == 1 {
          "conflict"
        } else {
          "conflicts"
        };
        println!(
          "  {}: {} {}, {} {}",
          r.crate_name,
          r.result.commits_synced,
          commit_word,
          r.result.conflicts.len(),
          conflict_word
        );
        for conflict in &r.result.conflicts {
          println!("    {}", conflict.file_path.display());
        }
      }
    }
    println!();
  }

  // Summary line
  let commit_word = if total_commits == 1 { "commit" } else { "commits" };
  if total_conflicts > 0 {
    let conflict_word = if total_conflicts == 1 { "conflict" } else { "conflicts" };
    println!(
      "sync complete: {} {}, {} {}",
      total_commits, commit_word, total_conflicts, conflict_word
    );
  } else {
    println!("sync complete: {} {}", total_commits, commit_word);
  }

  Ok(())
}

fn build_sync_mutation_plan(
  ctx: &WorkspaceContext,
  configs: &[(crate::sync::SyncConfig, bool)],
  direction: &SyncDirection,
  strategy: ConflictStrategy,
  allow_dirty: bool,
) -> RailResult<mutation::MutationPlan> {
  let source_head = ctx.git.git().head_commit().unwrap_or_else(|_| "unknown".to_string());
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
      let target_head = SystemGit::open(&config.target_repo_path)
        .and_then(|git| git.head_commit())
        .unwrap_or_else(|_| "none".to_string());
      let mapping_count = mapping_count_for(ctx.workspace_root(), &config.crate_name, &config.target_repo_path);
      MutationAction::new(
        "SYNC_CRATE",
        config.crate_name.clone(),
        Some(format!(
          "direction={}, strategy={}, target_exists={}, source_head={}, target_head={}, mapping_count={}",
          direction_name,
          format!("{:?}", strategy).to_lowercase(),
          target_exists,
          source_head,
          target_head,
          mapping_count
        )),
      )
    })
    .collect();

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
  ctx: &WorkspaceContext,
  configs: &[(crate::sync::SyncConfig, bool)],
  direction: &SyncDirection,
  strategy: ConflictStrategy,
) -> Vec<serde_json::Value> {
  let source_head = ctx.git.git().head_commit().unwrap_or_else(|_| "unknown".to_string());
  let direction_name = match direction {
    SyncDirection::MonoToRemote => "mono_to_remote",
    SyncDirection::RemoteToMono => "remote_to_mono",
    SyncDirection::Both => "bidirectional",
    SyncDirection::None => "none",
  };

  configs
    .iter()
    .map(|(config, target_exists)| {
      let target_head = SystemGit::open(&config.target_repo_path)
        .and_then(|git| git.head_commit())
        .ok();
      let mapping_count = mapping_count_for(ctx.workspace_root(), &config.crate_name, &config.target_repo_path);
      serde_json::json!({
        "crate_name": config.crate_name,
        "direction": direction_name,
        "strategy": format!("{:?}", strategy).to_lowercase(),
        "source_head": source_head,
        "target_head": target_head,
        "target_exists": target_exists,
        "mapping_snapshot": {
          "mapping_count": mapping_count,
        },
      })
    })
    .collect()
}

fn compute_conflict_candidates(configs: &[(crate::sync::SyncConfig, bool)]) -> Vec<serde_json::Value> {
  configs
    .iter()
    .map(|(config, _)| {
      let paths: Vec<String> = config.crate_paths.iter().map(|p| p.display().to_string()).collect();
      serde_json::json!({
        "crate_name": config.crate_name,
        "candidate_paths": paths,
      })
    })
    .collect()
}

fn mapping_count_for(workspace_root: &Path, crate_name: &str, target_repo_path: &Path) -> usize {
  let mut store = MappingStore::new(crate_name.to_string());
  let _ = store.load(workspace_root);
  let _ = store.load(target_repo_path);
  store.count()
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
  let dir = workspace_root.join("target").join("cargo-rail").join("receipts");
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
