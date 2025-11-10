use std::env;

use crate::cargo::metadata::WorkspaceMetadata;
use crate::cargo::transform::CargoTransform;
use crate::core::config::RailConfig;
use crate::core::conflict::ConflictStrategy;
use crate::core::error::{ConfigError, RailError, RailResult};
use crate::core::plan::{Operation, OperationType, Plan};
use crate::core::sync::{SyncConfig, SyncDirection, SyncEngine};

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
  let strategy = ConflictStrategy::from_str(&strategy_str).map_err(RailError::Other)?;
  let current_dir = env::current_dir()?;

  // Load configuration
  if !RailConfig::exists(&current_dir) {
    return Err(RailError::Config(ConfigError::NotFound {
      workspace_root: current_dir,
    }));
  }

  let config = RailConfig::load(&current_dir).map_err(RailError::Other)?;
  println!("📦 Loaded configuration from .rail/config.toml");

  // Determine sync direction
  let direction = match (from_remote, to_remote) {
    (true, true) => {
      return Err(RailError::Other(anyhow::anyhow!(
        "Cannot use both --from-remote and --to-remote"
      )));
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

  // Determine which crates to sync
  let crates_to_sync: Vec<_> = if all {
    println!("   Syncing all {} configured crates", config.splits.len());
    config.splits.clone()
  } else if let Some(ref name) = crate_name {
    let split_config = config
      .splits
      .iter()
      .find(|s| s.name == *name)
      .ok_or_else(|| RailError::Config(ConfigError::CrateNotFound { name: name.clone() }))?;
    vec![split_config.clone()]
  } else {
    return Err(RailError::Other(anyhow::anyhow!(
      "Must specify a crate name or use --all"
    )));
  };

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

  for (split_config, crate_paths, target_repo_path, _, _, _) in plans {
    println!("\n🔄 Syncing crate: {}", split_config.name);

    let sync_config = SyncConfig {
      crate_name: split_config.name.clone(),
      crate_paths,
      mode: split_config.mode.clone(),
      target_repo_path,
      branch: split_config.branch.clone(),
      remote_url: split_config.remote.clone(),
    };

    // Create transformer
    let metadata = WorkspaceMetadata::load(&config.workspace.root).map_err(RailError::Other)?;
    let transformer = Box::new(CargoTransform::new(metadata));

    // Create sync engine
    let mut engine = SyncEngine::new(
      config.workspace.root.clone(),
      sync_config,
      transformer,
      config.security.clone(),
      strategy,
    )
    .map_err(RailError::Other)?;

    // Perform sync
    let result = match direction {
      SyncDirection::MonoToRemote => engine.sync_to_remote().map_err(RailError::Other)?,
      SyncDirection::RemoteToMono => engine.sync_from_remote().map_err(RailError::Other)?,
      SyncDirection::Both => engine.sync_bidirectional().map_err(RailError::Other)?,
      SyncDirection::None => unreachable!(),
    };

    println!("   ✅ Synced {} commits", result.commits_synced);

    if !result.conflicts.is_empty() {
      println!("\n   ⚠️  {} conflicts detected:", result.conflicts.len());
      for conflict in &result.conflicts {
        println!("      - {}: {}", conflict.file_path.display(), conflict.message);
      }
    }
  }

  println!("\n🎉 Sync operation complete!");

  Ok(())
}
