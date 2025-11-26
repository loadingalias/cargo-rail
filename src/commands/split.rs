use std::io::IsTerminal;

use crate::commands::common::{OutputFormat, SplitSyncConfigBuilder};
use crate::config::RailConfig;
use crate::error::RailResult;
use crate::split::SplitEngine;
use crate::utils;
use crate::workspace::WorkspaceContext;
use rayon::prelude::*;

/// Run the split command
///
/// By default, executes the split operation (with confirmation prompt in interactive mode).
/// Use --check to show the plan without executing.
pub fn run_split(
  ctx: &WorkspaceContext,
  crate_name: Option<String>,
  all: bool,
  remote: Option<String>,
  dry_run: bool,
  format: String,
) -> RailResult<()> {
  let output_format: OutputFormat = format.parse()?;
  let json = output_format.is_json();

  if !json {
    eprintln!("📦 Loaded configuration");
  }

  // Build configurations using the centralized builder
  let builder = SplitSyncConfigBuilder::new(ctx)?
    .with_crate_or_all(crate_name.clone(), all)?
    .with_remote_override(remote)
    .validate()?;

  let all_local = builder.all_local();
  let config_count = builder.count();

  if all_local && !dry_run && !json {
    eprintln!("   Local testing mode\n");
  }

  if all && !json {
    eprintln!("   Splitting all {} configured crates", config_count);
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

    // Clone context for parallel execution (very cheap - just Arc increments)
    let ctx = ctx.clone();
    let results: Vec<RailResult<()>> = configs
      .into_par_iter()
      .map(|config| {
        println!("🔨 Splitting crate '{}'...", config.crate_name);
        let engine = SplitEngine::new(&ctx)?;
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

/// Initialize split configuration for one or more crates
///
/// Detects workspace members and adds split configuration to rail.toml.
/// Can configure specific crates or all workspace members.
pub fn run_split_init(ctx: &WorkspaceContext, crates: Option<&str>, dry_run: bool) -> RailResult<()> {
  use crate::config::RailConfig;
  use std::fs;

  // Parse crate names if provided
  let requested_crates: Option<Vec<String>> = crates.map(|s| {
    s.split(',')
      .map(|name| name.trim().to_string())
      .filter(|name| !name.is_empty())
      .collect()
  });

  // Detect splits based on requested crates or all workspace members
  let splits = detect_workspace_splits(ctx, requested_crates.as_deref())?;

  if splits.is_empty() {
    if let Some(requested) = requested_crates {
      return Err(crate::error::RailError::message(format!(
        "No matching crates found for: {}",
        requested.join(", ")
      )));
    } else {
      return Err(crate::error::RailError::message("No workspace members found"));
    }
  }

  // Load existing config or create new one
  let workspace_root = ctx.workspace_root();
  let existing_config = RailConfig::load(workspace_root).ok();

  let mut config = existing_config.unwrap_or_else(|| RailConfig {
    workspace: crate::config::WorkspaceConfig {
      root: std::path::PathBuf::from("."),
    },
    targets: vec![], // No targets by default
    unify: crate::config::UnifyConfig::default(),
    release: crate::config::ReleaseConfig::default(),
    change_detection: crate::config::ChangeDetectionConfig::default(),
    crates: Default::default(),
    formatting: crate::config::FormattingConfig::default(),
  });

  // Add new splits to crates config (avoid duplicates)
  let existing_names: std::collections::HashSet<_> = config.crates.keys().cloned().collect();
  let new_splits: Vec<_> = splits
    .into_iter()
    .filter(|s| !existing_names.contains(&s.name))
    .collect();

  if new_splits.is_empty() {
    println!("✅ All requested crates are already configured");
    return Ok(());
  }

  println!("Adding {} split configuration(s):", new_splits.len());
  for split in &new_splits {
    println!("  • {}", split.name);
  }
  println!();

  // Convert SplitConfig to unified CrateConfig structure
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

  // Serialize config
  let config_toml = serialize_splits_config(&config)?;

  if dry_run {
    println!("{}", config_toml);
  } else {
    // Find or create config file
    let config_path =
      RailConfig::find_config_path(workspace_root).unwrap_or_else(|| workspace_root.join(".config/rail.toml"));

    // Write config
    if let Some(parent) = config_path.parent() {
      fs::create_dir_all(parent)?;
    }

    fs::write(&config_path, config_toml)?;
    println!("✅ Updated {}", config_path.display());
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

/// Serialize RailConfig to TOML - used for split init
fn serialize_splits_config(config: &RailConfig) -> RailResult<String> {
  toml_edit::ser::to_string_pretty(config)
    .map_err(|e| crate::error::RailError::message(format!("Failed to serialize config: {}", e)))
}
