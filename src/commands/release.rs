//! `cargo rail release` - Release automation

use crate::commands::common::OutputFormat;
use crate::error::{RailError, RailResult};
use crate::release::planner::ReleasePlanner;
use crate::release::publisher::ReleasePublisher;
use crate::release::validator::ReleaseValidator;
use crate::release::version::BumpType;
use crate::workspace::WorkspaceContext;
use std::io::{self, IsTerminal};

/// Plan a release (check mode)
pub fn run_release_plan(
  ctx: &WorkspaceContext,
  crate_names: Option<Vec<String>>,
  bump: String,
  skip_publish: bool,
  skip_tag: bool,
  format: String,
) -> RailResult<()> {
  let output_format: OutputFormat = format.parse()?;
  let json = output_format.is_json();

  let bump_type = bump.parse::<BumpType>()?;

  if let Some(ref names) = crate_names {
    let workspace_members = ctx.graph.workspace_members();
    for name in names {
      if !workspace_members.contains(name) {
        return Err(RailError::with_help(
          format!("crate '{}' not found", name),
          format!("available: {}", workspace_members.join(", ")),
        ));
      }
    }
  }

  let config = ctx.config.as_ref().map(|c| &c.release);
  let release_config =
    config.ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;

  let planner = ReleasePlanner::new(ctx, release_config);
  let plan = planner.plan(crate_names.clone(), &bump_type)?;

  if json {
    let json_output = serde_json::to_string_pretty(&plan)
      .map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))?;
    println!("{}", json_output);
  } else {
    println!("{}", plan.format_summary_with_flags(skip_publish, skip_tag));
    println!("\nrun without --check to execute");
  }

  Ok(())
}

/// Execute a release
pub fn run_release_publish(
  ctx: &WorkspaceContext,
  crate_names: Option<Vec<String>>,
  all: bool,
  bump: String,
  skip_publish: bool,
  skip_tag: bool,
) -> RailResult<()> {
  let config = ctx.config.as_ref().map(|c| &c.release);
  let release_config =
    config.ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;

  let targets = if all {
    None
  } else if let Some(names) = crate_names {
    Some(names)
  } else {
    return Err(RailError::with_help(
      "must specify crate name(s) or --all",
      "cargo rail release my-crate\ncargo rail release --all",
    ));
  };

  let bump_type = bump.parse::<BumpType>()?;

  let validator = ReleaseValidator::new(ctx);
  let target_crates = targets.clone().unwrap_or_else(|| ctx.graph.workspace_members());
  validator.validate(&target_crates, release_config.require_clean)?;

  let planner = ReleasePlanner::new(ctx, release_config);
  let plan = planner.plan(targets, &bump_type)?;

  println!("{}", plan.format_summary_with_flags(skip_publish, skip_tag));

  if io::stdin().is_terminal() {
    println!("\nthis will:");
    println!("  - modify Cargo.toml (version bumps)");
    println!("  - update changelogs");
    println!("  - create git commits");
    if !skip_tag {
      println!("  - create {} tag(s)", plan.crates.len());
    }
    if !skip_publish {
      println!("  - publish to crates.io (irreversible)");
    }

    if !crate::utils::prompt_for_confirmation("\nproceed? [Enter/Ctrl+C]")? {
      println!("cancelled");
      return Ok(());
    }
  }

  let publisher = ReleasePublisher::new(ctx, release_config);
  publisher.execute(&plan, skip_publish, skip_tag)?;

  Ok(())
}

/// Validate release readiness
pub fn run_release_check(
  ctx: &WorkspaceContext,
  crate_names: Option<Vec<String>>,
  all: bool,
  format: String,
) -> RailResult<()> {
  let output_format: OutputFormat = format.parse()?;
  let json = output_format.is_json();

  let config = ctx.config.as_ref().map(|c| &c.release);
  let release_config =
    config.ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;

  let target_crates = if all {
    ctx.graph.workspace_members()
  } else if let Some(names) = crate_names {
    names
  } else {
    return Err(RailError::with_help(
      "must specify crate name(s) or --all",
      "cargo rail check my-crate\ncargo rail check --all",
    ));
  };

  let validator = ReleaseValidator::new(ctx);
  validator.validate(&target_crates, release_config.require_clean)?;

  let mut results = Vec::new();
  for crate_name in &target_crates {
    validator.validate_publishable(crate_name)?;
    results.push(crate_name.clone());
    if !json {
      println!("{}: ready", crate_name);
    }
  }

  if json {
    let output = serde_json::json!({
      "status": "passed",
      "crates": results,
      "count": results.len()
    });
    println!(
      "{}",
      serde_json::to_string_pretty(&output).map_err(|e| RailError::message(format!("JSON error: {}", e)))?
    );
  } else {
    println!("\nall checks passed");
  }

  Ok(())
}

/// Initialize release configuration
pub fn run_release_init(ctx: &WorkspaceContext, crates: Option<&str>, check: bool) -> RailResult<()> {
  use crate::config::{ChangelogConfig, CrateReleaseConfig, RailConfig};
  use std::fs;

  let requested_crates: Option<Vec<String>> = crates.map(|s| {
    s.split(',')
      .map(|name| name.trim().to_string())
      .filter(|name| !name.is_empty())
      .collect()
  });

  let members = ctx.cargo.metadata().workspace_packages();
  let workspace_root = ctx.workspace_root();

  let target_crates: Vec<_> = members
    .iter()
    .filter(|pkg| {
      requested_crates
        .as_ref()
        .map(|requested| requested.contains(&pkg.name))
        .unwrap_or(true)
    })
    .collect();

  if target_crates.is_empty() {
    if let Some(requested) = requested_crates {
      return Err(crate::error::RailError::message(format!(
        "no matching crates: {}",
        requested.join(", ")
      )));
    } else {
      return Err(crate::error::RailError::message("no workspace members found"));
    }
  }

  let existing_config = RailConfig::load(workspace_root).ok();

  let mut config = existing_config.unwrap_or_else(|| RailConfig {
    workspace: crate::config::WorkspaceConfig {
      root: std::path::PathBuf::from("."),
    },
    targets: vec![],
    unify: crate::config::UnifyConfig::default(),
    release: crate::config::ReleaseConfig::default(),
    change_detection: crate::config::ChangeDetectionConfig::default(),
    crates: Default::default(),
    formatting: crate::config::FormattingConfig::default(),
  });

  let mut new_crates = Vec::new();
  let mut existing_crates = Vec::new();

  for pkg in target_crates {
    if config.crates.contains_key(pkg.name.as_str()) && config.crates[pkg.name.as_str()].release.is_some() {
      existing_crates.push(pkg.name.clone());
      continue;
    }

    new_crates.push(pkg.name.clone());

    let crate_dir = pkg.manifest_path.parent().expect("manifest has parent");
    let changelog_path = crate::utils::detect_crate_changelog(crate_dir);

    let crate_config = config.crates.entry(pkg.name.to_string()).or_default();

    crate_config.release = Some(CrateReleaseConfig {
      publish: pkg.publish.as_ref().map(|p| !p.is_empty()).unwrap_or(true),
    });

    if let Some(path) = changelog_path {
      crate_config.changelog = Some(ChangelogConfig {
        path: Some(path),
        skip: false,
      });
    }
  }

  if !existing_crates.is_empty() {
    println!("skipping {} with existing config:", existing_crates.len());
    for name in &existing_crates {
      println!("  {}", name);
    }
  }

  if new_crates.is_empty() {
    println!("all crates already configured");
    return Ok(());
  }

  println!("adding release config for {} crate(s):", new_crates.len());
  for name in &new_crates {
    println!("  {}", name);
  }

  let config_toml = toml_edit::ser::to_string_pretty(&config)
    .map_err(|e| crate::error::RailError::message(format!("config serialization failed: {}", e)))?;

  if check {
    println!("\n{}", config_toml);
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
