use std::env;

use crate::commands::doctor;
use crate::core::config::RailConfig;
use crate::core::conflict::ConflictStrategy;
use crate::core::context::WorkspaceContext;
use crate::core::error::{ConfigError, RailError, RailResult};
use crate::core::executor::PlanExecutor;
use crate::core::plan::{Operation, OperationType, Plan};
use crate::core::sync::SyncDirection;
use crate::ui::progress::{FileProgress, MultiProgress};
use crate::utils;
use rayon::prelude::*;

/// Sync command parameters
pub struct SyncParams {
  pub crate_name: Option<String>,
  pub all: bool,
  pub remote: Option<String>,
  pub from_remote: bool,
  pub to_remote: bool,
  pub strategy_str: String,
  pub no_protected_branches: bool,
  pub apply: bool,
  pub json: bool,
}

/// Run the sync command
#[allow(clippy::too_many_arguments)]
pub fn run_sync(
  crate_name: Option<String>,
  all: bool,
  remote: Option<String>,
  from_remote: bool,
  to_remote: bool,
  strategy_str: String,
  no_protected_branches: bool,
  apply: bool,
  json: bool,
) -> RailResult<()> {
  // Convert to struct for internal use
  let params = SyncParams {
    crate_name,
    all,
    remote,
    from_remote,
    to_remote,
    strategy_str,
    no_protected_branches,
    apply,
    json,
  };
  run_sync_impl(params)
}

/// Internal implementation of sync command
fn run_sync_impl(params: SyncParams) -> RailResult<()> {
  let SyncParams {
    crate_name,
    all,
    remote,
    from_remote,
    to_remote,
    strategy_str,
    no_protected_branches,
    apply,
    json,
  } = params;
  // Parse conflict strategy (validate it, then use as string in ExecuteSync operation)
  let _strategy = ConflictStrategy::from_str(&strategy_str)?;
  let current_dir = env::current_dir()?;

  // Load configuration
  if !RailConfig::exists(&current_dir) {
    return Err(RailError::Config(ConfigError::NotFound {
      workspace_root: current_dir,
    }));
  }

  let mut config = RailConfig::load(&current_dir)?;

  // Apply CLI overrides to security config if provided
  if no_protected_branches {
    config.security.protected_branches.clear();
  }

  println!("📦 Loaded configuration from .rail/config.toml");

  // Determine which crates to sync
  let mut crates_to_sync_check: Vec<_> = if all {
    config.splits.clone()
  } else if let Some(ref name) = crate_name {
    let split_config = config
      .splits
      .iter()
      .find(|s| s.name == *name)
      .ok_or_else(|| RailError::Config(ConfigError::CrateNotFound { name: name.clone() }))?;
    vec![split_config.clone()]
  } else {
    return Err(RailError::with_help(
      "Must specify a crate name or use --all",
      "Try: cargo rail sync --all OR cargo rail sync <crate-name>",
    ));
  };

  // Apply remote override if provided (before all_local check)
  if let Some(ref remote_override) = remote {
    for split_config in &mut crates_to_sync_check {
      split_config.remote = remote_override.clone();
    }
  }

  // Check if all remotes are local paths (skip SSH checks for local testing)
  let all_local = crates_to_sync_check.iter().all(|s| utils::is_local_path(&s.remote));

  // Run preflight health checks before proceeding (skip for local-only operations)
  if !json && apply && !all_local {
    println!("🏥 Running preflight health checks...");
    if !doctor::run_preflight_check(false)? {
      return Err(RailError::with_help(
        "Preflight checks failed - environment is not ready",
        "Run 'cargo rail doctor' for detailed diagnostics and fixes",
      ));
    }
    println!("   ✅ All preflight checks passed\n");
  } else if all_local && apply {
    println!("   Skipping preflight checks (local testing mode)\n");
  }

  // Determine sync direction
  let direction = match (from_remote, to_remote) {
    (true, true) => {
      return Err(RailError::with_help(
        "Cannot use both --from-remote and --to-remote",
        "Choose one direction: use --from-remote OR --to-remote (or neither for bidirectional sync)",
      ));
    }
    (true, false) => {
      println!("   Direction: remote → monorepo");
      SyncDirection::RemoteToMono
    }
    (false, true) => {
      println!("   Direction: monorepo → remote");
      SyncDirection::MonoToRemote
    }
    (false, false) => {
      println!("   Direction: bidirectional");
      SyncDirection::Both
    }
  };

  // Use the crates we already determined
  let crates_to_sync = crates_to_sync_check;
  if all {
    println!("   Syncing all {} configured crates", crates_to_sync.len());
  }

  // Validate crates with health checks before starting (skip for local testing)
  if apply && !json && !all_local {
    let mut progress = if crates_to_sync.len() > 1 {
      Some(FileProgress::new(
        crates_to_sync.len(),
        format!("Running pre-flight checks for {} crates", crates_to_sync.len()),
      ))
    } else {
      None
    };

    for split_config in &crates_to_sync {
      if progress.is_none() {
        println!("   🏥 Checking crate '{}'...", split_config.name);
      }
      if !doctor::run_crate_check(&split_config.name, false)? {
        return Err(RailError::with_help(
          format!("Health checks failed for crate '{}'", split_config.name),
          "Run 'cargo rail doctor' for detailed diagnostics",
        ));
      }
      if let Some(ref mut p) = progress {
        p.inc();
      }
    }
  }

  // Build plans using the unified Plan system
  let mut plans = Vec::new();

  for split_config in &crates_to_sync {
    let crate_paths = split_config.get_paths().into_iter().cloned().collect::<Vec<_>>();

    // Determine target repo path
    let target_repo_path = if utils::is_local_path(&split_config.remote) {
      std::path::PathBuf::from(&split_config.remote)
    } else {
      let remote_name = split_config
        .remote
        .rsplit('/')
        .next()
        .unwrap_or(&split_config.name)
        .trim_end_matches(".git");
      current_dir.join("..").join(remote_name)
    };

    // Check if target repo exists
    let target_exists = target_repo_path.exists();
    if !target_exists && apply {
      eprintln!("⚠️  Error: Target repo not found at: {}", target_repo_path.display());
      eprintln!("   Run `cargo rail split {}` first", split_config.name);
      continue;
    }

    // Build unified Plan with ExecuteSync operation
    let mut plan = Plan::new(OperationType::Sync, Some(split_config.name.clone()));

    // Determine direction string for the plan
    let dir_str = match direction {
      SyncDirection::MonoToRemote => "to_remote",
      SyncDirection::RemoteToMono => "from_remote",
      SyncDirection::Both => "bidirectional",
      SyncDirection::None => continue,
    };

    // Add high-level ExecuteSync operation
    plan.add_operation(Operation::ExecuteSync {
      crate_name: split_config.name.clone(),
      crate_paths: crate_paths.iter().map(|p| p.display().to_string()).collect(),
      mode: format!("{:?}", split_config.mode),
      target_repo_path: target_repo_path.display().to_string(),
      branch: split_config.branch.clone(),
      remote_url: split_config.remote.clone(),
      direction: dir_str.to_string(),
      conflict_strategy: strategy_str.clone(),
    });

    let dir_display = match direction {
      SyncDirection::MonoToRemote => "monorepo → remote",
      SyncDirection::RemoteToMono => "remote → monorepo",
      SyncDirection::Both => "bidirectional",
      SyncDirection::None => "none",
    };

    // Add metadata
    let protected_handling = if matches!(direction, SyncDirection::RemoteToMono | SyncDirection::Both) {
      Some(format!(
        "Will create PR branch if target is protected ({})",
        config.security.protected_branches.join(", ")
      ))
    } else {
      None
    };

    plan = plan
      .with_summary(format!(
        "Sync crate '{}' ({}) with conflict strategy: {}",
        split_config.name, dir_display, strategy_str
      ))
      .add_trailer("Rail-Operation", "sync")
      .add_trailer("Rail-Crate", &split_config.name)
      .add_trailer("Rail-Direction", dir_display)
      .add_trailer("Rail-Strategy", &strategy_str);

    plans.push((
      split_config.clone(),
      crate_paths,
      target_repo_path,
      plan,
      target_exists,
      protected_handling,
    ));
  }

  // Output plans
  if !apply {
    if json {
      // JSON output for CI/automation
      let json_plans: Vec<&Plan> = plans.iter().map(|(_, _, _, plan, _, _)| plan).collect();
      for plan in json_plans {
        println!("{}", plan.to_json()?);
      }
    } else {
      // Human-readable plan
      println!("\n🔍 DRY-RUN MODE - No changes will be made");
      println!("   Add --apply to actually perform the sync\n");

      for (split_config, _, target_repo_path, plan, target_exists, protected_handling) in &plans {
        println!("{}", plan.to_human_readable());
        println!("   Target: {}", target_repo_path.display());
        println!("   Remote: {}", split_config.remote);
        println!("   Branch: {}", split_config.branch);
        println!("   Conflict strategy: {}", strategy_str);
        if !target_exists {
          println!(
            "   ⚠️  Target repo does not exist yet - run `cargo rail split {}` first",
            split_config.name
          );
        }
        if let Some(handling) = protected_handling {
          println!("   🛡️  {}", handling);
        }
        println!();
      }

      println!("✋ To execute this plan, run:");
      if all {
        println!(
          "   cargo rail sync --all {} --apply",
          if from_remote {
            "--from-remote"
          } else if to_remote {
            "--to-remote"
          } else {
            ""
          }
        );
      } else if let Some(ref name) = crate_name {
        println!(
          "   cargo rail sync {} {} --apply",
          name,
          if from_remote {
            "--from-remote"
          } else if to_remote {
            "--to-remote"
          } else {
            ""
          }
        );
      }
    }

    return Ok(());
  }

  // Apply mode - execute the sync
  println!("\n🚀 APPLY MODE - Executing sync operations\n");

  // Build workspace context for execution
  let workspace_context = WorkspaceContext::build(&current_dir)?;
  let executor = PlanExecutor::new(&workspace_context);

  let plan_count = plans.len();

  // Use parallel processing for multiple crates
  if plan_count > 1 && all {
    println!("🚀 Processing {} crates in parallel...\n", plan_count);

    let multi_progress = MultiProgress::new();
    let bars: Vec<_> = plans
      .iter()
      .map(|(split_config, _, _, _, _, _)| multi_progress.add_bar(1, format!("Syncing {}", split_config.name)))
      .collect();

    // For parallel execution, we need to build contexts per-thread
    let results: Vec<RailResult<()>> = plans
      .into_par_iter()
      .enumerate()
      .map(|(idx, (_, _, _, plan, _, _))| {
        // Build workspace context for this thread
        let thread_context = WorkspaceContext::build(&current_dir)?;
        let thread_executor = PlanExecutor::new(&thread_context);
        let result = thread_executor.execute(&plan);

        multi_progress.inc(&bars[idx]);
        result
      })
      .collect();

    // Check for errors
    for result in results {
      result?;
    }
  } else {
    // Sequential processing for single crate or when not using --all
    let mut crate_progress = if plan_count > 1 {
      Some(FileProgress::new(plan_count, format!("Syncing {} crates", plan_count)))
    } else {
      None
    };

    for (split_config, _, _, plan, _, _) in plans {
      if crate_progress.is_none() {
        println!("\n🔄 Syncing crate: {}", split_config.name);
      }

      executor.execute(&plan)?;

      if let Some(ref mut p) = crate_progress {
        p.inc();
      }
    }
  }

  println!("\n🎉 Sync operation complete!");

  Ok(())
}
