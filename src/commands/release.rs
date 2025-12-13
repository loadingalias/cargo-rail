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
  format: OutputFormat,
) -> RailResult<()> {
  let json = format.is_json();

  // JSON mode enables structured error output and suppresses progress
  if json {
    crate::output::set_json_mode(true);
  }

  let bump_type = bump.parse::<BumpType>()?;

  let workspace_members = ctx.graph.workspace_members();

  if let Some(ref names) = crate_names {
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

  // Validate release config (Issue #14: tag_format, Issue #20: skip_changelog_for)
  let warnings = release_config.validate(workspace_members).map_err(RailError::Config)?;

  // Print warnings
  for warning in &warnings {
    eprintln!("warning: {}", warning);
  }

  // Issue #15: Run require_clean check in --check mode too (early feedback)
  let validator = ReleaseValidator::new(ctx);
  let target_crates = crate_names.clone().unwrap_or_else(|| workspace_members.to_vec());

  if release_config.require_clean {
    validator.validate(&target_crates, true)?;
  }

  // Validate changelog paths (catches path traversal issues early)
  validator.validate_changelog_paths(&target_crates, release_config)?;

  let planner = ReleasePlanner::new(ctx, release_config);
  let plan = planner.plan(crate_names.clone(), &bump_type)?;

  if json {
    let json_output = serde_json::to_string_pretty(&plan)
      .map_err(|e| RailError::message(format!("JSON serialization failed: {}", e)))?;
    println!("{}", json_output);
  } else {
    // Issue #16: Show publish_delay in the plan output
    println!("{}", plan.format_summary_with_flags(skip_publish, skip_tag));

    // Show additional config info
    if !skip_publish && plan.summary.crates_to_publish > 1 {
      println!("Publish delay: {}s between crates", release_config.publish_delay);
    }
    if release_config.create_github_release && !skip_tag {
      println!("GitHub releases: enabled");
    }
    if release_config.sign_tags && !skip_tag {
      println!("Tag signing: enabled");
    }

    println!("\nChanges detected. Run without --check to apply.");
  }

  // Exit code 1 in --check mode indicates changes are pending (consistent across text/json)
  Err(RailError::CheckHasPendingChanges)
}

/// Execute a release
pub fn run_release_publish(
  ctx: &WorkspaceContext,
  crate_names: Option<Vec<String>>,
  all: bool,
  bump: String,
  skip_publish: bool,
  skip_tag: bool,
  yes: bool,
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
  let target_crates = targets
    .clone()
    .unwrap_or_else(|| ctx.graph.workspace_members().to_vec());
  validator.validate(&target_crates, release_config.require_clean)?;

  // Validate branch state (detached HEAD = error, non-default branch = error unless --yes)
  if let Some(warning) = validator.validate_branch(yes)? {
    eprintln!("{}", warning);
  }

  // Validate changelog paths
  validator.validate_changelog_paths(&target_crates, release_config)?;

  let planner = ReleasePlanner::new(ctx, release_config);
  let plan = planner.plan(targets, &bump_type)?;

  println!("{}", plan.format_summary_with_flags(skip_publish, skip_tag));

  // Skip confirmation if --yes flag is set
  if !yes && io::stdin().is_terminal() {
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
  extended: bool,
  format: OutputFormat,
) -> RailResult<()> {
  let json = format.is_json();

  // JSON mode enables structured error output and suppresses progress
  if json {
    crate::output::set_json_mode(true);
  }

  let config = ctx.config.as_ref().map(|c| &c.release);
  let release_config =
    config.ok_or_else(|| RailError::with_help("no release configuration", "run 'cargo rail init' first"))?;

  let target_crates = if all {
    ctx.graph.workspace_members().to_vec()
  } else if let Some(names) = crate_names {
    names
  } else {
    return Err(RailError::with_help(
      "must specify crate name(s) or --all",
      "cargo rail release check my-crate\ncargo rail release check --all",
    ));
  };

  let validator = ReleaseValidator::new(ctx);
  validator.validate(&target_crates, release_config.require_clean)?;

  // Validate changelog paths
  validator.validate_changelog_paths(&target_crates, release_config)?;

  let mut results = Vec::new();
  for crate_name in &target_crates {
    validator.validate_publishable(crate_name)?;
    results.push(crate_name.clone());
    if !json {
      println!("{}: ready", crate_name);
    }
  }

  // Extended validation: cargo publish --dry-run and MSRV check
  let mut extended_results = Vec::new();
  let mut has_extended_failures = false;

  if extended {
    if !json {
      println!("\nrunning extended checks...");
    }

    let ext_results = validator.validate_extended(&target_crates);

    for (crate_name, checks) in ext_results {
      let mut crate_checks = Vec::new();

      for check in checks {
        if check.passed {
          if !json {
            println!(
              "  {}: {} - {}",
              crate_name,
              check.check_name,
              check.details.as_deref().unwrap_or("ok")
            );
          }
        } else {
          has_extended_failures = true;
          if !json {
            eprintln!(
              "  {}: {} - FAILED: {}",
              crate_name,
              check.check_name,
              check.error.as_deref().unwrap_or("unknown error")
            );
          }
        }

        crate_checks.push(serde_json::json!({
          "check": check.check_name,
          "passed": check.passed,
          "details": check.details,
          "error": check.error
        }));
      }

      extended_results.push(serde_json::json!({
        "crate": crate_name,
        "checks": crate_checks
      }));
    }
  }

  if json {
    let mut output = serde_json::json!({
      "command": "check",
      "status": if has_extended_failures { "failed" } else { "passed" },
      "crates": results,
      "count": results.len()
    });

    if extended {
      output["extended"] = serde_json::json!(extended_results);
    }

    println!(
      "{}",
      serde_json::to_string_pretty(&output).map_err(|e| RailError::message(format!("JSON error: {}", e)))?
    );
  } else if has_extended_failures {
    return Err(RailError::message("extended validation failed"));
  } else {
    println!("\nall checks passed");
  }

  if has_extended_failures && json {
    return Err(RailError::CheckHasPendingChanges);
  }

  Ok(())
}

/// Initialize release configuration
pub fn run_release_init(ctx: &WorkspaceContext, crates: Option<Vec<String>>, check: bool) -> RailResult<()> {
  use crate::config::{ChangelogConfig, CrateReleaseConfig, RailConfig};
  use std::fs;

  let requested_crates = crates;

  let members = ctx.cargo.workspace_members();
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
    targets: vec![],
    unify: crate::config::UnifyConfig::default(),
    release: crate::config::ReleaseConfig::default(),
    change_detection: crate::config::ChangeDetectionConfig::default(),
    crates: Default::default(),
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
