use std::env;

use crate::core::config::RailConfig;
use crate::core::error::{ConfigError, RailError, RailResult};
use crate::core::plan::{Operation, OperationType, Plan};
use crate::core::split::{SplitConfig, Splitter};

/// Run the split command
pub fn run_split(crate_name: Option<String>, all: bool, apply: bool, json: bool) -> RailResult<()> {
  let current_dir = env::current_dir()?;

  // Load configuration
  if !RailConfig::exists(&current_dir) {
    return Err(RailError::Config(ConfigError::NotFound {
      workspace_root: current_dir,
    }));
  }

  let config = RailConfig::load(&current_dir).map_err(RailError::Other)?;
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
      .ok_or_else(|| RailError::Config(ConfigError::CrateNotFound { name: name.clone() }))?;
    vec![split_config.clone()]
  } else {
    return Err(RailError::Other(anyhow::anyhow!(
      "Must specify a crate name or use --all"
    )));
  };

  // Validate all configurations before starting
  for split_config in &crates_to_split {
    split_config.validate().map_err(RailError::Other)?;
  }

  // Create splitter
  let splitter = Splitter::new(config.workspace.root.clone()).map_err(RailError::Other)?;

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

    splitter.split(&split_cfg).map_err(RailError::Other)?;
    println!();
  }

  println!("🎉 Split operation complete!");
  println!("\n📌 Next steps:");
  println!("   1. Review the generated repositories");
  println!("   2. Make sure remote repositories exist on GitHub/GitLab");
  println!("   3. If push failed, you may need to create the remote repo first");

  Ok(())
}
