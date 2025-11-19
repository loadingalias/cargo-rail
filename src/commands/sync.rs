use std::io::IsTerminal;
use std::str::FromStr;
use std::sync::Arc;

use crate::commands::common::SplitSyncConfigBuilder;
use crate::error::RailResult;
use crate::sync::{ConflictStrategy, SyncDirection, SyncEngine};
use crate::utils;
use crate::workspace::WorkspaceContext;
use rayon::prelude::*;

/// Run the sync command
///
/// By default, executes the sync operation (with confirmation prompt in interactive mode).
/// Use --dry-run to show the plan without executing.
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
  dry_run: bool,
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

  println!("📦 Loaded configuration");

  // Build configurations using the centralized builder
  let builder = SplitSyncConfigBuilder::new(ctx)?
    .with_crate_or_all(crate_name.clone(), all)?
    .with_remote_override(remote)
    // Note: sync doesn't validate like split does
    ;

  let all_local = builder.all_local();
  let config_count = builder.count();

  if all_local && !dry_run {
    println!("   Local testing mode\n");
  }

  // Determine sync direction
  let direction = match (from_remote, to_remote) {
    (true, true) => {
      return Err(crate::error::RailError::with_help(
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

  if all {
    println!("   Syncing all {} configured crates", config_count);
  }

  let configs = builder.build_sync_configs()?;

  // Dry-run mode: show what would be done
  if dry_run {
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
      println!("\n🔍 DRY-RUN MODE - Showing plan only\n");

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
          "   cargo rail sync --all {}",
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
          "   cargo rail sync {} {}",
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

      return Ok(());
    }
  }

  // Apply mode - interactive confirmation if TTY
  if std::io::stdin().is_terminal() && !json {
    println!("\n🚀 APPLY MODE - About to execute sync operations\n");

    let dir_display = match direction {
      SyncDirection::MonoToRemote => "→",
      SyncDirection::RemoteToMono => "←",
      SyncDirection::Both => "↔",
      SyncDirection::None => "-",
    };

    for (sync_config, target_exists) in &configs {
      let status = if !target_exists {
        " [⚠️  target missing]"
      } else {
        ""
      };
      println!("📦 {} (mono {} remote){}", sync_config.crate_name, dir_display, status);
    }

    if !utils::prompt_for_confirmation("Press Enter to proceed, or Ctrl+C to cancel")? {
      println!("Operation cancelled.");
      return Ok(());
    }
    println!();
  } else if !json {
    println!("\n🚀 APPLY MODE - Executing sync operations\n");
  }

  // Execute the syncs
  let security_config = Arc::new(config.security.clone());

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
