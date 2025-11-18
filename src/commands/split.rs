use std::io::{self, Write};

use crate::error::{ConfigError, RailError, RailResult};
use crate::split::{SplitConfig, SplitEngine};
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

/// Run the split command
pub fn run_split(
  ctx: &WorkspaceContext,
  crate_name: Option<String>,
  all: bool,
  remote: Option<String>,
  apply: bool,
  json: bool,
) -> RailResult<()> {
  let config = ctx.require_config()?.as_ref();
  println!("📦 Loaded configuration from .rail/config.toml");

  // Determine which crates to split
  let mut crates_to_split_check: Vec<_> = if all {
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
      "Try: cargo rail split --all OR cargo rail split <crate-name>",
    ));
  };

  // Apply remote override if provided
  if let Some(ref remote_override) = remote {
    for split_config in &mut crates_to_split_check {
      split_config.remote = remote_override.clone();
    }
  }

  // Check if all remotes are local paths (skip SSH checks for local testing)
  let all_local = crates_to_split_check.iter().all(|s| utils::is_local_path(&s.remote));

  if all_local && apply {
    println!("   Local testing mode\n");
  }

  let crates_to_split = crates_to_split_check;
  if all {
    println!("   Splitting all {} configured crates", crates_to_split.len());
  }

  // Validate all configurations
  for split_config in &crates_to_split {
    split_config.validate()?;
  }

  // Build SplitConfig for each crate
  let mut configs = Vec::new();

  for split_config in &crates_to_split {
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

    let config = SplitConfig {
      crate_name: split_config.name.clone(),
      crate_paths,
      mode: split_config.mode.clone(),
      target_repo_path,
      branch: split_config.branch.clone(),
      remote_url: Some(split_config.remote.clone()),
    };

    configs.push(config);
  }

  // Dry-run mode: show what would be done
  if !apply {
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
      println!("\n🔍 DRY-RUN MODE - No changes will be made");
      println!("   Add --apply to actually perform the split\n");

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
        println!("   cargo rail split --all --apply");
      } else if let Some(ref name) = crate_name {
        println!("   cargo rail split {} --apply", name);
      }
      println!();

      // Interactive confirmation
      if prompt_for_confirmation("Press Enter to apply this plan, or Ctrl+C to cancel")? {
        println!("\n🚀 APPLY MODE - Executing split operations\n");
        // Fall through to apply mode
      } else {
        return Ok(());
      }
    }
  } else {
    println!("\n🚀 APPLY MODE - Executing split operations\n");
  }

  // Apply mode - execute the splits
  let config_count = configs.len();

  // Use parallel processing for multiple crates
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
