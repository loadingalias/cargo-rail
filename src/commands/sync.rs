use std::env;

use crate::commands::doctor;
use crate::core::config::RailConfig;
use crate::core::conflict::ConflictStrategy;
use crate::core::error::{ConfigError, RailError, RailResult};
use crate::core::plan::{Operation, OperationType, Plan};
use crate::core::sync::{SyncConfig, SyncDirection, SyncEngine, SyncResult};
use crate::ui::progress::{FileProgress, MultiProgress};
use rayon::prelude::*;

/// Run the sync command
pub fn run_sync(
  crate_name: Option<String>,
  all: bool,
  from_remote: bool,
  to_remote: bool,
  strategy_str: String,
  apply: bool,
  json: bool,
) -> RailResult<()> {
  // Parse conflict strategy
  let strategy = ConflictStrategy::from_str(&strategy_str)?;
  let current_dir = env::current_dir()?;

  // Load configuration
  if !RailConfig::exists(&current_dir) {
    return Err(RailError::Config(ConfigError::NotFound {
      workspace_root: current_dir,
    }));
  }

  let config = RailConfig::load(&current_dir)?;
  println!("📦 Loaded configuration from .rail/config.toml");

  // Determine which crates to sync (need this to check if they're all local)
  let crates_to_sync_check: Vec<_> = if all {
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

  // Check if all remotes are local paths (skip SSH checks for local testing)
  let all_local = crates_to_sync_check
    .iter()
    .all(|s| s.remote.starts_with('/') || s.remote.starts_with("./") || s.remote.starts_with("../"));

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
    let target_repo_path = if split_config.remote.starts_with('/')
      || split_config.remote.starts_with("./")
      || split_config.remote.starts_with("../")
    {
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

    // Build unified Plan
    let mut plan = Plan::new(OperationType::Sync, Some(split_config.name.clone()));

    // Add operations based on direction
    let dir_str = match direction {
      SyncDirection::MonoToRemote => {
        plan.add_operation(Operation::Pull {
          remote: split_config.remote.clone(),
          branch: split_config.branch.clone(),
        });

        plan.add_operation(Operation::Transform {
          path: "Cargo.toml".to_string(),
          transform_type: "path_to_version".to_string(),
        });

        plan.add_operation(Operation::CreateCommit {
          message: "Sync from monorepo".to_string(),
          files: crate_paths.iter().map(|p| p.display().to_string()).collect(),
        });

        plan.add_operation(Operation::Push {
          remote: split_config.remote.clone(),
          branch: split_config.branch.clone(),
          force: false,
        });

        "monorepo → remote"
      }
      SyncDirection::RemoteToMono => {
        plan.add_operation(Operation::Pull {
          remote: split_config.remote.clone(),
          branch: split_config.branch.clone(),
        });

        plan.add_operation(Operation::Transform {
          path: "Cargo.toml".to_string(),
          transform_type: "version_to_path".to_string(),
        });

        plan.add_operation(Operation::CreatePrBranch {
          name: format!("rail/sync/{}", split_config.name),
          base: "main".to_string(),
          message: "Sync from remote".to_string(),
        });

        plan.add_operation(Operation::Push {
          remote: "origin".to_string(),
          branch: format!("rail/sync/{}", split_config.name),
          force: false,
        });

        "remote → monorepo"
      }
      SyncDirection::Both => {
        // Both directions
        plan.add_operation(Operation::Pull {
          remote: split_config.remote.clone(),
          branch: split_config.branch.clone(),
        });

        plan.add_operation(Operation::Merge {
          from: split_config.branch.clone(),
          into: "main".to_string(),
          strategy: strategy_str.clone(),
        });

        plan.add_operation(Operation::Push {
          remote: split_config.remote.clone(),
          branch: split_config.branch.clone(),
          force: false,
        });

        "bidirectional"
      }
      SyncDirection::None => continue,
    };

    // Add common operations
    plan.add_operation(Operation::UpdateNotes {
      notes_ref: format!("refs/notes/rail/{}", split_config.name),
      commit: "HEAD".to_string(),
      note_content: "sync mapping".to_string(),
    });

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
        split_config.name, dir_str, strategy_str
      ))
      .add_trailer("Rail-Operation", "sync")
      .add_trailer("Rail-Crate", &split_config.name)
      .add_trailer("Rail-Direction", dir_str)
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

  let plan_count = plans.len();

  // Use parallel processing for multiple crates
  if plan_count > 1 && all {
    println!("🚀 Processing {} crates in parallel...\n", plan_count);

    let multi_progress = MultiProgress::new();
    let bars: Vec<_> = plans
      .iter()
      .map(|(split_config, _, _, _, _, _)| multi_progress.add_bar(1, format!("Syncing {}", split_config.name)))
      .collect();

    // Wrap security config in Arc for cheap sharing across threads (avoid deep clones)
    let security_config = std::sync::Arc::new(config.security.clone());

    let results: Vec<RailResult<SyncResult>> = plans
      .into_par_iter()
      .enumerate()
      .map(|(idx, (split_config, crate_paths, target_repo_path, _, _, _))| {
        let sync_config = SyncConfig {
          crate_name: split_config.name.clone(),
          crate_paths,
          mode: split_config.mode.clone(),
          target_repo_path,
          branch: split_config.branch.clone(),
          remote_url: split_config.remote.clone(),
        };

        // Create sync engine for this thread (security_config Arc clone is cheap)
        let mut engine = SyncEngine::new(
          config.workspace.root.clone(),
          sync_config,
          security_config.clone(),
          strategy,
        )?;

        // Perform sync
        let result = match direction {
          SyncDirection::MonoToRemote => engine.sync_to_remote(),
          SyncDirection::RemoteToMono => engine.sync_from_remote(),
          SyncDirection::Both => engine.sync_bidirectional(),
          SyncDirection::None => unreachable!(),
        };

        multi_progress.inc(&bars[idx]);
        result
      })
      .collect();

    // Report results
    for result in results {
      let sync_result = result?;
      if sync_result.commits_synced > 0 || !sync_result.conflicts.is_empty() {
        println!("   ✅ Synced {} commits", sync_result.commits_synced);

        if !sync_result.conflicts.is_empty() {
          let unresolved_count = sync_result.conflicts.iter().filter(|c| !c.resolved).count();
          let resolved_count = sync_result.conflicts.len() - unresolved_count;

          if resolved_count > 0 {
            println!("\n   ✅ {} conflicts auto-resolved:", resolved_count);
          }

          if unresolved_count > 0 {
            println!("\n   ⚠️  {} conflicts need manual resolution:", unresolved_count);
          }
        }
      }
    }
  } else {
    // Sequential processing for single crate or when not using --all
    let mut crate_progress = if plan_count > 1 {
      Some(FileProgress::new(plan_count, format!("Syncing {} crates", plan_count)))
    } else {
      None
    };

    for (split_config, crate_paths, target_repo_path, _, _, _) in plans {
      if crate_progress.is_none() {
        println!("\n🔄 Syncing crate: {}", split_config.name);
      }

      let sync_config = SyncConfig {
        crate_name: split_config.name.clone(),
        crate_paths,
        mode: split_config.mode.clone(),
        target_repo_path,
        branch: split_config.branch.clone(),
        remote_url: split_config.remote.clone(),
      };

      // Create sync engine (Cargo workspace detected)
      let mut engine = SyncEngine::new(
        config.workspace.root.clone(),
        sync_config,
        std::sync::Arc::new(config.security.clone()),
        strategy,
      )?;

      // Perform sync
      let result = match direction {
        SyncDirection::MonoToRemote => engine.sync_to_remote()?,
        SyncDirection::RemoteToMono => engine.sync_from_remote()?,
        SyncDirection::Both => engine.sync_bidirectional()?,
        SyncDirection::None => unreachable!(),
      };

      println!("   ✅ Synced {} commits", result.commits_synced);

      if !result.conflicts.is_empty() {
        let unresolved_count = result.conflicts.iter().filter(|c| !c.resolved).count();
        let resolved_count = result.conflicts.len() - unresolved_count;

        if resolved_count > 0 {
          println!("\n   ✅ {} conflicts auto-resolved:", resolved_count);
          for conflict in result.conflicts.iter().filter(|c| c.resolved) {
            println!("      - {}: {}", conflict.file_path.display(), conflict.message);
          }
        }

        if unresolved_count > 0 {
          println!("\n   ⚠️  {} conflicts need manual resolution:", unresolved_count);
          for conflict in result.conflicts.iter().filter(|c| !c.resolved) {
            println!("      - {}: {}", conflict.file_path.display(), conflict.message);
          }
          println!("\n   💡 Resolve conflicts manually and run sync again");
        }
      }

      if let Some(ref mut p) = crate_progress {
        p.inc();
      }
    }
  }

  println!("\n🎉 Sync operation complete!");

  Ok(())
}
