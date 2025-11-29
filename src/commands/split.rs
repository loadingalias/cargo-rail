use std::io::IsTerminal;

use crate::commands::common::{OutputFormat, SplitSyncConfigBuilder};
use crate::config::RailConfig;
use crate::error::RailResult;
use crate::progress;
use crate::split::SplitEngine;
use crate::utils;
use crate::workspace::WorkspaceContext;
use rayon::prelude::*;

/// Run the split command
pub fn run_split(
  ctx: &WorkspaceContext,
  crate_name: Option<String>,
  all: bool,
  remote: Option<String>,
  check: bool,
  format: String,
) -> RailResult<()> {
  let output_format: OutputFormat = format.parse()?;
  let json = output_format.is_json();

  // JSON mode enables structured error output and suppresses progress
  if json {
    crate::output::set_json_mode(true);
  }

  let builder = SplitSyncConfigBuilder::new(ctx)?
    .with_crate_or_all(crate_name.clone(), all)?
    .with_remote_override(remote)
    .validate()?;

  let config_count = builder.count();
  let configs = builder.build_split_configs()?;

  // Check mode: show plan
  if check {
    if json {
      let crates: Vec<_> = configs
        .iter()
        .map(|config| {
          serde_json::json!({
            "crate_name": config.crate_name,
            "mode": format!("{:?}", config.mode),
            "target_repo": config.target_repo_path,
            "branch": config.branch,
            "remote_url": config.remote_url,
          })
        })
        .collect();
      let output = serde_json::json!({
        "command": "split",
        "check": true,
        "crates": crates,
        "count": configs.len()
      });
      println!("{}", serde_json::to_string_pretty(&output)?);
      return Ok(());
    }

    println!("split plan:\n");
    for config in &configs {
      println!("  {}", config.crate_name);
      println!("    mode: {:?}", config.mode);
      println!("    paths:");
      for path in &config.crate_paths {
        println!("      {}", path.display());
      }
      println!("    target: {}", config.target_repo_path.display());
      if let Some(ref remote) = config.remote_url {
        println!("    remote: {}", remote);
      }
      println!("    branch: {}", config.branch);
    }

    println!("\nrun without --check to execute");
    // Exit 1 to signal CI that split is pending
    return Err(crate::error::RailError::CheckHasPendingChanges);
  }

  // Interactive confirmation
  if std::io::stdin().is_terminal() && !json {
    println!("splitting {} crate(s):\n", config_count);
    for config in &configs {
      println!("  {} -> {}", config.crate_name, config.target_repo_path.display());
    }

    if !utils::prompt_for_confirmation("\nproceed? [Enter/Ctrl+C]")? {
      println!("cancelled");
      return Ok(());
    }
  }

  // Execute splits
  if config_count > 1 && all {
    progress!("splitting {} crates...", config_count);
    let ctx = ctx.clone();
    let results: Vec<RailResult<()>> = configs
      .into_par_iter()
      .map(|config| {
        progress!("  {}", config.crate_name);
        let engine = SplitEngine::new(&ctx)?;
        engine.split(&config)
      })
      .collect();

    for result in results {
      result?;
    }
  } else {
    for config in configs {
      progress!("splitting {}...", config.crate_name);
      let engine = SplitEngine::new(ctx)?;
      engine.split(&config)?;
    }
  }

  println!("split complete");
  Ok(())
}

/// Initialize split configuration for crates
pub fn run_split_init(ctx: &WorkspaceContext, crates: Option<Vec<String>>, check: bool) -> RailResult<()> {
  use crate::config::RailConfig;
  use std::fs;

  let requested_crates = crates;

  let splits = detect_workspace_splits(ctx, requested_crates.as_deref())?;

  if splits.is_empty() {
    if let Some(requested) = requested_crates {
      return Err(crate::error::RailError::message(format!(
        "no matching crates: {}",
        requested.join(", ")
      )));
    } else {
      return Err(crate::error::RailError::message("no workspace members found"));
    }
  }

  let workspace_root = ctx.workspace_root();
  let existing_config = RailConfig::load(workspace_root).ok();

  let mut config = existing_config.unwrap_or_else(|| RailConfig {
    targets: vec![],
    unify: crate::config::UnifyConfig::default(),
    release: crate::config::ReleaseConfig::default(),
    change_detection: crate::config::ChangeDetectionConfig::default(),
    crates: Default::default(),
    formatting: crate::config::FormattingConfig::default(),
  });

  let existing_names: std::collections::HashSet<_> = config.crates.keys().cloned().collect();
  let new_splits: Vec<_> = splits
    .into_iter()
    .filter(|s| !existing_names.contains(&s.name))
    .collect();

  if new_splits.is_empty() {
    println!("all crates already configured");
    return Ok(());
  }

  println!("adding {} split config(s):", new_splits.len());
  for split in &new_splits {
    println!("  {}", split.name);
  }

  use crate::config::{ChangelogConfig, CrateConfig, CrateReleaseConfig, CrateSplitConfig};

  for split in new_splits {
    let crate_config = CrateConfig {
      split: Some(CrateSplitConfig {
        remote: split.remote,
        branch: split.branch,
        mode: split.mode,
        workspace_mode: split.workspace_mode,
        paths: split.paths,
        include: split.include,
        exclude: split.exclude,
      }),
      release: Some(CrateReleaseConfig { publish: split.publish }),
      changelog: split.changelog_path.map(|path| ChangelogConfig {
        path: Some(path),
        skip: false,
      }),
      sync: None,
    };

    config.crates.insert(split.name, crate_config);
  }

  let config_toml = serialize_splits_config(&config)?;

  if check {
    println!("{}", config_toml);
  } else {
    let config_path =
      RailConfig::find_config_path(workspace_root).unwrap_or_else(|| workspace_root.join(".config/rail.toml"));

    if let Some(parent) = config_path.parent() {
      fs::create_dir_all(parent)?;
    }

    fs::write(&config_path, config_toml)?;
    println!("updated: {}", config_path.display());
  }

  Ok(())
}

/// Detect workspace members and create split configs
///
/// If `requested_crates` is provided, only creates configs for those crates.
/// Otherwise, creates configs for all workspace members.
fn detect_workspace_splits(
  ctx: &WorkspaceContext,
  requested_crates: Option<&[String]>,
) -> RailResult<Vec<crate::config::SplitConfig>> {
  use crate::config::{CratePath, SplitConfig, SplitMode, WorkspaceMode};

  let workspace_root = ctx.workspace_root();
  let members = ctx.cargo.metadata().workspace_packages();

  let mut splits = Vec::new();

  for pkg in members {
    // Filter by requested crates if specified
    if let Some(requested) = requested_crates
      && !requested.contains(&pkg.name)
    {
      continue;
    }

    // Get relative path from workspace root to crate directory
    let crate_dir = pkg.manifest_path.parent().expect("manifest has parent");
    // Detect per-crate CHANGELOG file
    let changelog_path = crate::utils::detect_crate_changelog(crate_dir);
    let rel_path = match crate_dir.strip_prefix(workspace_root) {
      Ok(p) => p.to_path_buf(),
      Err(_) => continue, // Skip if not under workspace root
    };

    // Generate a reasonable remote URL placeholder (GitHub org/repo pattern)
    let remote = format!("git@github.com:org/{}.git", pkg.name);

    // Check if crate has publish = false in Cargo.toml
    let publish = pkg.publish.as_ref().map(|p| !p.is_empty()).unwrap_or(true);

    splits.push(SplitConfig {
      name: pkg.name.to_string(),
      remote,
      branch: "main".to_string(),
      mode: SplitMode::Single,
      workspace_mode: WorkspaceMode::default(),
      paths: vec![CratePath { path: rel_path.into() }],
      include: vec![],
      exclude: vec![],
      publish,
      changelog_path,
    });
  }

  Ok(splits)
}

/// Serialize RailConfig to TOML
fn serialize_splits_config(config: &RailConfig) -> RailResult<String> {
  toml_edit::ser::to_string_pretty(config)
    .map_err(|e| crate::error::RailError::message(format!("config serialization failed: {}", e)))
}
