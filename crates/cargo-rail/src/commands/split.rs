use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::env;

use crate::core::config::RailConfig;
use crate::core::split::{SplitConfig, Splitter};

/// Plan for a split operation
#[derive(Debug, Serialize, Deserialize)]
struct SplitPlan {
  crate_name: String,
  mode: String,
  source_paths: Vec<String>,
  target_repo: String,
  remote_url: String,
  branch: String,
  estimated_commits: Option<usize>,
  operations: Vec<String>,
}

/// Run the split command
pub fn run_split(crate_name: Option<String>, all: bool, apply: bool, json: bool) -> Result<()> {
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

  // Determine which crates to split
  let crates_to_split: Vec<_> = if all {
    println!("   Splitting all {} configured crates", config.splits.len());
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

  // Validate all configurations before starting
  for split_config in &crates_to_split {
    split_config.validate()?;
  }

  // Create splitter
  let splitter = Splitter::new(config.workspace.root.clone())?;

  // Collect plans and execute
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

    // Create plan
    let plan = SplitPlan {
      crate_name: split_config.name.clone(),
      mode: format!("{:?}", split_config.mode).to_lowercase(),
      source_paths: crate_paths.iter().map(|p| p.display().to_string()).collect(),
      target_repo: target_repo_path.display().to_string(),
      remote_url: split_config.remote.clone(),
      branch: split_config.branch.clone(),
      estimated_commits: None, // Could calculate this in dry-run mode
      operations: vec![
        "Initialize target repository".to_string(),
        "Walk commit history for crate paths".to_string(),
        "Recreate commits in target repo".to_string(),
        "Transform Cargo.toml (workspace → concrete values)".to_string(),
        "Copy auxiliary files (LICENSE, README, etc.)".to_string(),
        "Push to remote".to_string(),
      ],
    };

    plans.push((split_config.clone(), crate_paths, target_repo_path, plan));
  }

  // Output plans
  if !apply {
    if json {
      // JSON output for CI/automation
      let json_plans: Vec<&SplitPlan> = plans.iter().map(|(_, _, _, plan)| plan).collect();
      println!("{}", serde_json::to_string_pretty(&json_plans)?);
    } else {
      // Human-readable plan
      println!("\n🔍 DRY-RUN MODE - No changes will be made");
      println!("   Add --apply to actually perform the split\n");

      for (_, _, _, plan) in &plans {
        println!("📦 Plan for crate: {}", plan.crate_name);
        println!("   Mode: {}", plan.mode);
        println!("   Source paths:");
        for path in &plan.source_paths {
          println!("     • {}", path);
        }
        println!("   Target: {}", plan.target_repo);
        println!("   Remote: {}", plan.remote_url);
        println!("   Branch: {}", plan.branch);
        println!("\n   Operations:");
        for (i, op) in plan.operations.iter().enumerate() {
          println!("     {}. {}", i + 1, op);
        }
        println!();
      }

      println!("✋ To execute this plan, run:");
      if all {
        println!("   cargo rail split --all --apply");
      } else if let Some(ref name) = crate_name {
        println!("   cargo rail split {} --apply", name);
      }
    }

    return Ok(());
  }

  // Apply mode - execute the split
  println!("\n🚀 APPLY MODE - Executing split operations\n");

  for (split_config, crate_paths, target_repo_path, _) in plans {
    let split_cfg = SplitConfig {
      crate_name: split_config.name.clone(),
      crate_paths,
      mode: split_config.mode.clone(),
      target_repo_path,
      branch: split_config.branch.clone(),
      remote_url: Some(split_config.remote.clone()),
    };

    splitter.split(&split_cfg)?;
    println!();
  }

  println!("🎉 Split operation complete!");
  println!("\n📌 Next steps:");
  println!("   1. Review the generated repositories");
  println!("   2. Make sure remote repositories exist on GitHub/GitLab");
  println!("   3. If push failed, you may need to create the remote repo first");

  Ok(())
}
