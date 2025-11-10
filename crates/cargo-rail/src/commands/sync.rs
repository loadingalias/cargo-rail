use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::env;

use crate::cargo::metadata::WorkspaceMetadata;
use crate::cargo::transform::CargoTransform;
use crate::core::config::RailConfig;
use crate::core::conflict::ConflictStrategy;
use crate::core::sync::{SyncConfig, SyncDirection, SyncEngine};

/// Plan for a sync operation
#[derive(Debug, Serialize, Deserialize)]
struct SyncPlan {
  crate_name: String,
  direction: String,
  source: String,
  target: String,
  remote_url: String,
  branch: String,
  strategy: String,
  estimated_commits: Option<usize>,
  operations: Vec<String>,
  protected_branch_handling: Option<String>,
}

/// Run the sync command
pub fn run_sync(
  crate_name: Option<String>,
  all: bool,
  from_remote: bool,
  to_remote: bool,
  strategy_str: String,
  apply: bool,
  json: bool,
) -> Result<()> {
  // Parse conflict strategy
  let strategy = ConflictStrategy::from_str(&strategy_str)?;
  let current_dir = env::current_dir().context("Failed to get current directory")?;

  // Load configuration
  if !RailConfig::exists(&current_dir) {
    anyhow::bail!(
      "No cargo-rail configuration found. Run `cargo rail init` first.\n\
       Expected file: {}/.rail/config.toml",
      current_dir.display()
    );
  }

  let config = RailConfig::load(&current_dir)?;
  println!("📦 Loaded configuration from .rail/config.toml");

  // Determine sync direction
  let direction = match (from_remote, to_remote) {
    (true, true) => anyhow::bail!("Cannot use both --from-remote and --to-remote"),
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
      .ok_or_else(|| anyhow::anyhow!("Crate '{}' not found in configuration", name))?;
    vec![split_config.clone()]
  } else {
    anyhow::bail!("Must specify a crate name or use --all");
  };

  // Collect plans and execute
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

    // Create plan
    let (source, target, dir_str, protected_handling) = match direction {
      SyncDirection::MonoToRemote => (
        config.workspace.root.display().to_string(),
        target_repo_path.display().to_string(),
        "monorepo → remote",
        None,
      ),
      SyncDirection::RemoteToMono => (
        target_repo_path.display().to_string(),
        config.workspace.root.display().to_string(),
        "remote → monorepo",
        Some(format!(
          "Will create PR branch if target is protected ({})",
          config.security.protected_branches.join(", ")
        )),
      ),
      SyncDirection::Both => (
        "both".to_string(),
        "both".to_string(),
        "bidirectional",
        Some(format!(
          "Will create PR branch for remote → mono if needed ({})",
          config.security.protected_branches.join(", ")
        )),
      ),
      SyncDirection::None => continue,
    };

    let plan = SyncPlan {
      crate_name: split_config.name.clone(),
      direction: dir_str.to_string(),
      source,
      target,
      remote_url: split_config.remote.clone(),
      branch: split_config.branch.clone(),
      strategy: strategy_str.clone(),
      estimated_commits: None, // Could calculate in dry-run
      operations: vec![
        "Load git-notes mappings".to_string(),
        "Fetch latest from remote".to_string(),
        "Find commits to sync (filtering duplicates)".to_string(),
        "Apply transforms (Cargo.toml path ↔ version)".to_string(),
        "Detect and resolve conflicts".to_string(),
        "Create commits with Rail-Origin trailers".to_string(),
        "Update git-notes mappings".to_string(),
        "Push to remote".to_string(),
      ],
      protected_branch_handling: protected_handling,
    };

    plans.push((split_config.clone(), crate_paths, target_repo_path, plan, target_exists));
  }

  // Output plans
  if !apply {
    if json {
      // JSON output for CI/automation
      let json_plans: Vec<&SyncPlan> = plans.iter().map(|(_, _, _, plan, _)| plan).collect();
      println!("{}", serde_json::to_string_pretty(&json_plans)?);
    } else {
      // Human-readable plan
      println!("\n🔍 DRY-RUN MODE - No changes will be made");
      println!("   Add --apply to actually perform the sync\n");

      for (_, _, _, plan, target_exists) in &plans {
        println!("📦 Plan for crate: {}", plan.crate_name);
        println!("   Direction: {}", plan.direction);
        println!("   Source: {}", plan.source);
        println!("   Target: {}", plan.target);
        println!("   Remote: {}", plan.remote_url);
        println!("   Branch: {}", plan.branch);
        println!("   Conflict strategy: {}", plan.strategy);
        if !target_exists {
          println!(
            "   ⚠️  Target repo does not exist yet - run `cargo rail split {}` first",
            plan.crate_name
          );
        }
        if let Some(ref handling) = plan.protected_branch_handling {
          println!("   🛡️  {}", handling);
        }
        println!("\n   Operations:");
        for (i, op) in plan.operations.iter().enumerate() {
          println!("     {}. {}", i + 1, op);
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

  for (split_config, crate_paths, target_repo_path, _, _) in plans {
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
    let metadata = WorkspaceMetadata::load(&config.workspace.root)?;
    let transformer = Box::new(CargoTransform::new(metadata));

    // Create sync engine
    let mut engine = SyncEngine::new(
      config.workspace.root.clone(),
      sync_config,
      transformer,
      config.security.clone(),
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
      println!("\n   ⚠️  {} conflicts detected:", result.conflicts.len());
      for conflict in &result.conflicts {
        println!("      - {}: {}", conflict.file_path.display(), conflict.message);
      }
    }
  }

  println!("\n🎉 Sync operation complete!");

  Ok(())
}
