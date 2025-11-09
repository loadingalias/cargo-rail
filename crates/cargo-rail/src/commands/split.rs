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

    // Determine target repo path
    // If remote is a local file path, use it directly as the target
    // Otherwise, derive the name from the remote URL and create in parent dir
    let target_repo_path = if split_config.remote.starts_with('/')
      || split_config.remote.starts_with("./")
      || split_config.remote.starts_with("../")
    {
      // Remote is a local file path, use it as-is
      std::path::PathBuf::from(&split_config.remote)
    } else {
      // Remote is a URL (git@github.com:... or https://...), extract name
      let remote_name = split_config
        .remote
        .rsplit('/')
        .next()
        .unwrap_or(&split_config.name)
        .trim_end_matches(".git");

      current_dir.join("..").join(remote_name)
    };

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
