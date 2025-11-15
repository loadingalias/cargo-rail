use std::env;
use std::io::{self, Write};

use crate::commands::doctor;
use crate::core::config::RailConfig;
use crate::core::error::{ConfigError, RailError, RailResult};
use crate::core::plan::{Operation, OperationType, Plan};
use crate::core::split::{SplitConfig, Splitter};
use crate::ui::progress::{FileProgress, MultiProgress};
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
  crate_name: Option<String>,
  all: bool,
  remote: Option<String>,
  apply: bool,
  json: bool,
) -> RailResult<()> {
  let current_dir = env::current_dir()?;

  // Load configuration
  if !RailConfig::exists(&current_dir) {
    return Err(RailError::Config(ConfigError::NotFound {
      workspace_root: current_dir,
    }));
  }

  let config = RailConfig::load(&current_dir)?;
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

  // Apply remote override if provided (before all_local check)
  if let Some(ref remote_override) = remote {
    for split_config in &mut crates_to_split_check {
      split_config.remote = remote_override.clone();
    }
  }

  // Check if all remotes are local paths (skip SSH checks for local testing)
  let all_local = crates_to_split_check
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

  // Use the crates we already determined
  let crates_to_split = crates_to_split_check;
  if all {
    println!("   Splitting all {} configured crates", crates_to_split.len());
  }

  // Validate all configurations and run health checks before starting
  let mut progress = if apply && !json && !all_local && crates_to_split.len() > 1 {
    Some(FileProgress::new(
      crates_to_split.len(),
      format!("Running pre-flight checks for {} crates", crates_to_split.len()),
    ))
  } else {
    None
  };

  for split_config in &crates_to_split {
    split_config.validate()?;

    // Run crate-specific health checks (skip for local testing)
    if apply && !json && !all_local {
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

  // Create splitter with security config
  let splitter = Splitter::new(config.workspace.root.clone(), config.security.clone())?;

  // Build plans using the unified Plan system
  let mut plans = Vec::new();

  for split_config in &crates_to_split {
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

    // Build unified Plan
    let mut plan = Plan::new(OperationType::Split, Some(split_config.name.clone()));

    // Add operations
    plan.add_operation(Operation::InitRepo {
      path: target_repo_path.display().to_string(),
    });

    plan.add_operation(Operation::CreateCommit {
      message: "Walking commit history for crate paths".to_string(),
      files: crate_paths.iter().map(|p| p.display().to_string()).collect(),
    });

    plan.add_operation(Operation::Transform {
      path: target_repo_path.join("Cargo.toml").display().to_string(),
      transform_type: "workspace_to_concrete".to_string(),
    });

    plan.add_operation(Operation::Copy {
      from: "auxiliary_files".to_string(),
      to: target_repo_path.display().to_string(),
    });

    plan.add_operation(Operation::Push {
      remote: split_config.remote.clone(),
      branch: split_config.branch.clone(),
      force: false,
    });

    // Add metadata
    plan = plan
      .with_summary(format!(
        "Split crate '{}' to {} (mode: {:?})",
        split_config.name, split_config.remote, split_config.mode
      ))
      .mark_destructive()
      .add_trailer("Rail-Operation", "split")
      .add_trailer("Rail-Crate", &split_config.name);

    plans.push((split_config.clone(), crate_paths, target_repo_path, plan));
  }

  // Output plans
  if !apply {
    if json {
      // JSON output for CI/automation
      let json_plans: Vec<&Plan> = plans.iter().map(|(_, _, _, plan)| plan).collect();
      for plan in json_plans {
        println!("{}", plan.to_json()?);
      }
      return Ok(());
    } else {
      // Human-readable plan
      println!("\n🔍 DRY-RUN MODE - No changes will be made");
      println!("   Add --apply to actually perform the split\n");

      for (split_config, crate_paths, target_repo_path, plan) in &plans {
        println!("{}", plan.to_human_readable());
        println!("   Mode: {:?}", split_config.mode);
        println!("   Source paths:");
        for path in crate_paths {
          println!("     • {}", path.display());
        }
        println!("   Target: {}", target_repo_path.display());
        println!("   Remote: {}", split_config.remote);
        println!("   Branch: {}", split_config.branch);
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
        // User confirmed, continue to apply logic below
        println!("\n🚀 APPLY MODE - Executing split operations\n");
      } else {
        return Ok(());
      }
    }
  }

  // Apply mode - execute the split (message already printed above or from --apply flag)
  if apply {
    println!("\n🚀 APPLY MODE - Executing split operations\n");
  }

  let plan_count = plans.len();

  // Use parallel processing for multiple crates
  if plan_count > 1 && all {
    println!("🚀 Processing {} crates in parallel...\n", plan_count);

    let multi_progress = MultiProgress::new();
    let bars: Vec<_> = plans
      .iter()
      .map(|(split_config, _, _, _)| multi_progress.add_bar(1, format!("Splitting {}", split_config.name)))
      .collect();

    let results: Vec<RailResult<()>> = plans
      .into_par_iter()
      .enumerate()
      .map(|(idx, (split_config, crate_paths, target_repo_path, _))| {
        let split_cfg = SplitConfig {
          crate_name: split_config.name.clone(),
          crate_paths,
          mode: split_config.mode.clone(),
          target_repo_path,
          branch: split_config.branch.clone(),
          remote_url: Some(split_config.remote.clone()),
        };

        // Create a new splitter for this thread
        let thread_splitter = Splitter::new(config.workspace.root.clone(), config.security.clone())?;
        let result = thread_splitter.split(&split_cfg);

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
      Some(FileProgress::new(
        plan_count,
        format!("Splitting {} crates", plan_count),
      ))
    } else {
      None
    };

    for (split_config, crate_paths, target_repo_path, _) in plans {
      if crate_progress.is_none() {
        println!("🔨 Splitting crate '{}'...", split_config.name);
      }

      let split_cfg = SplitConfig {
        crate_name: split_config.name.clone(),
        crate_paths,
        mode: split_config.mode.clone(),
        target_repo_path,
        branch: split_config.branch.clone(),
        remote_url: Some(split_config.remote.clone()),
      };

      splitter.split(&split_cfg)?;

      if let Some(ref mut p) = crate_progress {
        p.inc();
      }
      println!();
    }
  }

  println!("🎉 Split operation complete!");
  println!("\n📌 Next steps:");
  println!("   1. Review the generated repositories");
  println!("   2. Make sure remote repositories exist on GitHub/GitLab");
  println!("   3. If push failed, you may need to create the remote repo first");

  Ok(())
}
