use anyhow::{Context, Result};
use std::env;

use crate::core::config::RailConfig;
use crate::core::split::{SplitConfig, Splitter};

/// Run the split command
pub fn run_split(crate_name: Option<String>, all: bool) -> Result<()> {
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

  // Validate all configurations before starting
  for split_config in &crates_to_split {
    split_config.validate()?;
  }

  // Create splitter
  let splitter = Splitter::new(config.workspace.root.clone())?;

  // Split each crate
  for split_config in crates_to_split {
    let crate_paths = split_config.get_paths().into_iter().cloned().collect();

    // Prompt for target repo path if not using a configured remote
    // For MVP, we'll use a local path based on the remote URL
    let remote_name = split_config
      .remote
      .rsplit('/')
      .next()
      .unwrap_or(&split_config.name)
      .trim_end_matches(".git");

    let target_repo_path = current_dir.join("..").join(remote_name);

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
