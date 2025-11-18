use std::io::{self, Write};
use std::sync::Arc;

use crate::error::{ConfigError, RailError, RailResult};
use crate::sync::{ConflictStrategy, SyncConfig, SyncDirection, SyncEngine};
use crate::utils;
use crate::workspace::WorkspaceContext;
use rayon::prelude::*;

/// Prompt user for confirmation
fn prompt_for_confirmation(message: &str) -> RailResult<bool> {
  print!("\n{}: ", message);
  io::stdout().flush()?;

  let mut input = String::new();
  io::stdin().read_line(&mut input)?;

  // If user just presses Enter (empty line), that's a confirmation
  Ok(input.trim().is_empty())
}

/// Run the sync command
#[allow(clippy::too_many_arguments)]
pub fn run_sync(
  ctx: &WorkspaceContext,
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
  // Parse conflict strategy
  let conflict_strategy = ConflictStrategy::from_str(&strategy_str)?;

  // Load configuration
  let mut config = ctx.require_config()?.as_ref().clone();

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

  // Apply remote override if provided
  if let Some(ref remote_override) = remote {
    for split_config in &mut crates_to_sync_check {
      split_config.remote = remote_override.clone();
    }
  }

  // Check if all remotes are local paths (skip SSH checks for local testing)
  let all_local = crates_to_sync_check.iter().all(|s| utils::is_local_path(&s.remote));

  if all_local && apply {
    println!("   Local testing mode\n");
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

  let crates_to_sync = crates_to_sync_check;
  if all {
    println!("   Syncing all {} configured crates", crates_to_sync.len());
  }

  // Build SyncConfig for each crate
  let mut configs = Vec::new();

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
      ctx.workspace_root().join("..").join(remote_name)
    };

    // Check if target repo exists
    let target_exists = target_repo_path.exists();

    let sync_config = SyncConfig {
      crate_name: split_config.name.clone(),
      crate_paths,
      mode: split_config.mode.clone(),
      target_repo_path: target_repo_path.clone(),
      branch: split_config.branch.clone(),
      remote_url: split_config.remote.clone(),
    };

    configs.push((sync_config, target_exists));
  }

  // Dry-run mode: show what would be done
  if !apply {
    if json {
      // JSON output for CI/automation
      for (sync_config, target_exists) in &configs {
        let dir_str = match direction {
          SyncDirection::MonoToRemote => "to_remote",
          SyncDirection::RemoteToMono => "from_remote",
          SyncDirection::Both => "bidirectional",
          SyncDirection::None => "none",
        };

        println!(
          "{}",
          serde_json::to_string_pretty(&serde_json::json!({
            "crate_name": sync_config.crate_name,
            "mode": format!("{:?}", sync_config.mode),
            "target_repo": sync_config.target_repo_path,
            "branch": sync_config.branch,
            "remote_url": sync_config.remote_url,
            "direction": dir_str,
            "conflict_strategy": strategy_str,
            "target_exists": target_exists,
          }))?
        );
      }
      return Ok(());
    } else {
      // Human-readable plan
      println!("\n🔍 DRY-RUN MODE - No changes will be made");
      println!("   Add --apply to actually perform the sync\n");

      let dir_display = match direction {
        SyncDirection::MonoToRemote => "monorepo → remote",
        SyncDirection::RemoteToMono => "remote → monorepo",
        SyncDirection::Both => "bidirectional",
        SyncDirection::None => "none",
      };

      for (sync_config, target_exists) in &configs {
        println!("📦 Crate: {}", sync_config.crate_name);
        println!("   Mode: {:?}", sync_config.mode);
        println!("   Direction: {}", dir_display);
        println!("   Source paths:");
        for path in &sync_config.crate_paths {
          println!("     • {}", path.display());
        }
        println!("   Target: {}", sync_config.target_repo_path.display());
        println!("   Remote: {}", sync_config.remote_url);
        println!("   Branch: {}", sync_config.branch);
        println!("   Conflict strategy: {}", strategy_str);
        if !target_exists {
          println!(
            "   ⚠️  Target repo does not exist yet - run `cargo rail split {}` first",
            sync_config.crate_name
          );
        }
        if matches!(direction, SyncDirection::RemoteToMono | SyncDirection::Both)
          && !config.security.protected_branches.is_empty()
        {
          println!(
            "   🛡️  Will create PR branch if target is protected ({})",
            config.security.protected_branches.join(", ")
          );
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
      println!();

      // Interactive confirmation
      if prompt_for_confirmation("Press Enter to apply this plan, or Ctrl+C to cancel")? {
        println!("\n🚀 APPLY MODE - Executing sync operations\n");
        // Fall through to apply mode
      } else {
        return Ok(());
      }
    }
  } else {
    println!("\n🚀 APPLY MODE - Executing sync operations\n");
  }

  // Apply mode - execute the syncs
  let config_count = configs.len();
  let security_config = Arc::new(config.security.clone());

  // Use parallel processing for multiple crates
  if config_count > 1 && all {
    println!("🚀 Processing {} crates in parallel...\n", config_count);

    // For parallel execution, we need to build contexts per-thread
    let workspace_root = ctx.workspace_root().to_path_buf();
    let results: Vec<RailResult<()>> = configs
      .into_par_iter()
      .map(|(sync_config, target_exists)| {
        if !target_exists {
          eprintln!(
            "⚠️  Error: Target repo not found at: {}",
            sync_config.target_repo_path.display()
          );
          eprintln!("   Run `cargo rail split {}` first", sync_config.crate_name);
          return Ok(());
        }

        println!("🔄 Syncing crate '{}'...", sync_config.crate_name);

        // Build workspace context for this thread
        let thread_context = WorkspaceContext::build(&workspace_root)?;
        let mut engine = SyncEngine::new(&thread_context, sync_config, security_config.clone(), conflict_strategy)?;

        // Execute sync based on direction
        let _result = match direction {
          SyncDirection::MonoToRemote => engine.sync_to_remote()?,
          SyncDirection::RemoteToMono => engine.sync_from_remote()?,
          SyncDirection::Both => engine.sync_bidirectional()?,
          SyncDirection::None => return Ok(()),
        };

        Ok(())
      })
      .collect();

    // Check for errors
    for result in results {
      result?;
    }
  } else {
    // Sequential processing for single crate or when not using --all
    for (sync_config, target_exists) in configs {
      if !target_exists {
        eprintln!(
          "⚠️  Error: Target repo not found at: {}",
          sync_config.target_repo_path.display()
        );
        eprintln!("   Run `cargo rail split {}` first", sync_config.crate_name);
        continue;
      }

      println!("🔄 Syncing crate '{}'...", sync_config.crate_name);
      let mut engine = SyncEngine::new(ctx, sync_config, security_config.clone(), conflict_strategy)?;

      // Execute sync based on direction
      let _result = match direction {
        SyncDirection::MonoToRemote => engine.sync_to_remote()?,
        SyncDirection::RemoteToMono => engine.sync_from_remote()?,
        SyncDirection::Both => engine.sync_bidirectional()?,
        SyncDirection::None => continue,
      };

      println!();
    }
  }

  println!("🎉 Sync operation complete!");
  Ok(())
}
