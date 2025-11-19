//! `cargo rail release` - Release automation commands

use crate::error::{RailError, RailResult};
use crate::release::planner::ReleasePlanner;
use crate::release::publisher::ReleasePublisher;
use crate::release::validator::ReleaseValidator;
use crate::release::version::BumpType;
use crate::workspace::WorkspaceContext;
use std::io::{self, IsTerminal};

/// Run release plan command (dry-run by default)
///
/// Shows what would be released without making any changes.
pub fn run_release_plan(
  ctx: &WorkspaceContext,
  crate_names: Option<Vec<String>>,
  bump: String,
  json: bool,
) -> RailResult<()> {
  println!("🔍 Planning release...\n");

  // Parse bump type
  let bump_type = bump.parse::<BumpType>()?;

  // Get release config
  let config = ctx.config.as_ref().map(|c| &c.release);
  let release_config = config.ok_or_else(|| {
    RailError::with_help(
      "No release configuration found",
      "Run 'cargo rail init' to create configuration with release settings",
    )
  })?;

  // Build release plan
  let planner = ReleasePlanner::new(ctx, release_config);
  let plan = planner.plan(crate_names.clone(), &bump_type)?;

  // Display plan
  if json {
    let json_output = serde_json::to_string_pretty(&plan)
      .map_err(|e| RailError::message(format!("Failed to serialize plan: {}", e)))?;
    println!("{}", json_output);
  } else {
    println!("{}", plan.format_summary());

    println!("📝 This is a dry-run. To execute this release, run:");
    if let Some(names) = crate_names {
      for name in names {
        println!("   cargo rail release publish {} --execute", name);
      }
    } else {
      println!("   cargo rail release publish --all --execute");
    }
  }

  Ok(())
}

/// Run release publish command
///
/// Executes the actual release: version bumps, changelog updates, tagging, and publishing.
pub fn run_release_publish(
  ctx: &WorkspaceContext,
  crate_names: Option<Vec<String>>,
  all: bool,
  bump: String,
  execute: bool,
  skip_publish: bool,
  skip_tag: bool,
) -> RailResult<()> {
  // Get release config
  let config = ctx.config.as_ref().map(|c| &c.release);
  let release_config = config.ok_or_else(|| {
    RailError::with_help(
      "No release configuration found",
      "Run 'cargo rail init' to create configuration with release settings",
    )
  })?;

  // Determine target crates
  let targets = if all {
    None
  } else if let Some(names) = crate_names {
    Some(names)
  } else {
    return Err(RailError::with_help(
      "Must specify crate name(s) or --all",
      "Examples:\n  cargo rail release publish my-crate\n  cargo rail release publish --all",
    ));
  };

  // Parse bump type
  let bump_type = bump.parse::<BumpType>()?;

  // Validate
  let validator = ReleaseValidator::new(ctx);
  let target_crates = targets.clone().unwrap_or_else(|| ctx.graph.workspace_members());
  validator.validate(&target_crates, release_config.require_clean)?;

  // Build release plan
  println!("🔍 Building release plan...\n");
  let planner = ReleasePlanner::new(ctx, release_config);
  let plan = planner.plan(targets, &bump_type)?;

  // Display plan
  println!("{}", plan.format_summary());

  // Dry-run check
  if !execute {
    println!("\n🔍 DRY-RUN MODE - No changes will be made\n");
    println!("To execute this release, add --execute flag:");
    let target_str = if all {
      "--all".to_string()
    } else {
      target_crates.join(" ")
    };
    println!("   cargo rail release publish {} --execute", target_str);
    return Ok(());
  }

  // Interactive confirmation (if terminal)
  if io::stdin().is_terminal() {
    println!("\n⚠️  WARNING: This will:");
    println!("  • Modify Cargo.toml files (version bumps)");
    println!("  • Update CHANGELOG.md files");
    println!("  • Create git commits");
    if !skip_tag {
      println!("  • Create {} git tag(s)", plan.crates.len());
    }
    if !skip_publish {
      println!("  • Publish to crates.io (IRREVERSIBLE!)");
    }
    println!();

    if !crate::utils::prompt_for_confirmation("Press Enter to proceed, or Ctrl+C to cancel")? {
      println!("Operation cancelled.");
      return Ok(());
    }
  }

  // Execute release
  let publisher = ReleasePublisher::new(ctx, release_config);
  publisher.execute(&plan, skip_publish, skip_tag)?;

  Ok(())
}

/// Run release check command (CI validation)
///
/// Validates that crates are ready for release without making changes.
pub fn run_release_check(ctx: &WorkspaceContext, crate_names: Option<Vec<String>>, all: bool) -> RailResult<()> {
  println!("🔍 Checking release readiness...\n");

  // Get release config
  let config = ctx.config.as_ref().map(|c| &c.release);
  let release_config = config.ok_or_else(|| {
    RailError::with_help(
      "No release configuration found",
      "Run 'cargo rail init' to create configuration with release settings",
    )
  })?;

  // Determine target crates
  let target_crates = if all {
    ctx.graph.workspace_members()
  } else if let Some(names) = crate_names {
    names
  } else {
    return Err(RailError::with_help(
      "Must specify crate name(s) or --all",
      "Examples:\n  cargo rail release check my-crate\n  cargo rail release check --all",
    ));
  };

  // Validate
  let validator = ReleaseValidator::new(ctx);
  validator.validate(&target_crates, release_config.require_clean)?;

  // Check publishability
  for crate_name in &target_crates {
    validator.validate_publishable(crate_name)?;
    println!("✅ {} is ready for release", crate_name);
  }

  println!("\n✅ All release checks passed!");
  Ok(())
}
