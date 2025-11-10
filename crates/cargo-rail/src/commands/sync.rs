use anyhow::{Context, Result};
use std::env;

use crate::cargo::metadata::WorkspaceMetadata;
use crate::cargo::transform::CargoTransform;
use crate::core::config::RailConfig;
use crate::core::conflict::ConflictStrategy;
use crate::core::sync::{SyncConfig, SyncDirection, SyncEngine};

/// Run the sync command
pub fn run_sync(
  crate_name: Option<String>,
  all: bool,
  from_remote: bool,
  to_remote: bool,
  strategy_str: String,
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
  } else if let Some(name) = crate_name {
    let split_config = config
      .splits
      .iter()
      .find(|s| s.name == name)
      .ok_or_else(|| anyhow::anyhow!("Crate '{}' not found in configuration", name))?;
    vec![split_config.clone()]
  } else {
    anyhow::bail!("Must specify a crate name or use --all");
  };

  // Sync each crate
  for split_config in crates_to_sync {
    println!("\n🔄 Syncing crate: {}", split_config.name);

    let crate_paths = split_config.get_paths().into_iter().cloned().collect();

    // Determine target repo path (same logic as split command)
    let target_repo_path = if split_config.remote.starts_with('/')
      || split_config.remote.starts_with("./")
      || split_config.remote.starts_with("../")
    {
      // Remote is a local file path, use it as-is
      std::path::PathBuf::from(&split_config.remote)
    } else {
      // Remote is a URL, extract name
      let remote_name = split_config
        .remote
        .rsplit('/')
        .next()
        .unwrap_or(&split_config.name)
        .trim_end_matches(".git");

      current_dir.join("..").join(remote_name)
    };

    // Check if target repo exists
    if !target_repo_path.exists() {
      println!("   ⚠️  Target repo not found at: {}", target_repo_path.display());
      println!("   Run `cargo rail split {}` first", split_config.name);
      continue;
    }

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

    // Create sync engine with security config and conflict strategy
    let mut engine = SyncEngine::new(
      config.workspace.root.clone(),
      sync_config,
      transformer,
      config.security.clone(),
      strategy,
    )?;

    // Perform sync based on direction
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
