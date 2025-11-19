use std::io::IsTerminal;

use crate::commands::common::SplitSyncConfigBuilder;
use crate::error::RailResult;
use crate::split::SplitEngine;
use crate::utils;
use crate::workspace::WorkspaceContext;
use rayon::prelude::*;

/// Run the split command
///
/// By default, executes the split operation (with confirmation prompt in interactive mode).
/// Use --dry-run to show the plan without executing.
pub fn run_split(
  ctx: &WorkspaceContext,
  crate_name: Option<String>,
  all: bool,
  remote: Option<String>,
  dry_run: bool,
  json: bool,
) -> RailResult<()> {
  println!("📦 Loaded configuration");

  // Build configurations using the centralized builder
  let builder = SplitSyncConfigBuilder::new(ctx)?
    .with_crate_or_all(crate_name.clone(), all)?
    .with_remote_override(remote)
    .validate()?;

  let all_local = builder.all_local();
  let config_count = builder.count();

  if all_local && !dry_run {
    println!("   Local testing mode\n");
  }

  if all {
    println!("   Splitting all {} configured crates", config_count);
  }

  let configs = builder.build_split_configs()?;

  // Dry-run mode: show what would be done
  if dry_run {
    if json {
      // JSON output for CI/automation
      for config in &configs {
        println!(
          "{}",
          serde_json::to_string_pretty(&serde_json::json!({
            "crate_name": config.crate_name,
            "mode": format!("{:?}", config.mode),
            "target_repo": config.target_repo_path,
            "branch": config.branch,
            "remote_url": config.remote_url,
          }))?
        );
      }
      return Ok(());
    } else {
      // Human-readable plan
      println!("\n🔍 DRY-RUN MODE - Showing plan only\n");

      for config in &configs {
        println!("📦 Crate: {}", config.crate_name);
        println!("   Mode: {:?}", config.mode);
        println!("   Source paths:");
        for path in &config.crate_paths {
          println!("     • {}", path.display());
        }
        println!("   Target: {}", config.target_repo_path.display());
        if let Some(ref remote) = config.remote_url {
          println!("   Remote: {}", remote);
        }
        println!("   Branch: {}", config.branch);
        println!();
      }

      println!("✋ To execute this plan, run:");
      if all {
        println!("   cargo rail split --all");
      } else if let Some(ref name) = crate_name {
        println!("   cargo rail split {}", name);
      }
      println!();

      return Ok(());
    }
  }

  // Apply mode - interactive confirmation if TTY
  if std::io::stdin().is_terminal() && !json {
    println!("\n🚀 APPLY MODE - About to execute split operations\n");

    for config in &configs {
      println!(
        "📦 {}: {} → {}",
        config.crate_name,
        config
          .crate_paths
          .iter()
          .map(|p| p.display().to_string())
          .collect::<Vec<_>>()
          .join(", "),
        config.target_repo_path.display()
      );
    }

    if !utils::prompt_for_confirmation("Press Enter to proceed, or Ctrl+C to cancel")? {
      println!("Operation cancelled.");
      return Ok(());
    }
    println!();
  } else if !json {
    println!("\n🚀 APPLY MODE - Executing split operations\n");
  }

  // Execute the splits
  if config_count > 1 && all {
    println!("🚀 Processing {} crates in parallel...\n", config_count);

    // For parallel execution, we need to build contexts per-thread
    let workspace_root = ctx.workspace_root().to_path_buf();
    let results: Vec<RailResult<()>> = configs
      .into_par_iter()
      .map(|config| {
        println!("🔨 Splitting crate '{}'...", config.crate_name);
        // Build workspace context for this thread
        let thread_context = WorkspaceContext::build(&workspace_root)?;
        let engine = SplitEngine::new(&thread_context)?;
        engine.split(&config)
      })
      .collect();

    // Check for errors
    for result in results {
      result?;
    }
  } else {
    // Sequential processing for single crate or when not using --all
    for config in configs {
      println!("🔨 Splitting crate '{}'...", config.crate_name);
      let engine = SplitEngine::new(ctx)?;
      engine.split(&config)?;
      println!();
    }
  }

  println!("✅ Split operation completed successfully");
  Ok(())
}
