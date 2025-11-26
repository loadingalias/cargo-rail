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
  skip_publish: bool,
  skip_tag: bool,
  json: bool,
) -> RailResult<()> {
  if !json {
    eprintln!("🔍 Planning release...\n");
  }

  // Parse bump type
  let bump_type = bump.parse::<BumpType>()?;

  // Validate crate names exist before planning
  if let Some(ref names) = crate_names {
    let workspace_members = ctx.graph.workspace_members();
    for name in names {
      if !workspace_members.contains(name) {
        return Err(RailError::with_help(
          format!("Crate '{}' not found in workspace", name),
          format!("Available crates: {}", workspace_members.join(", ")),
        ));
      }
    }
  }

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
    println!("{}", plan.format_summary_with_flags(skip_publish, skip_tag));

    eprintln!("\n📝 This is a dry-run. To execute this release, run:");
    if let Some(names) = crate_names {
      for name in names {
        eprintln!("   cargo rail release {}", name);
      }
    } else {
      eprintln!("   cargo rail release --all");
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
      "Examples:\n  cargo rail release my-crate\n  cargo rail release --all",
    ));
  };

  // Parse bump type
  let bump_type = bump.parse::<BumpType>()?;

  // Validate
  let validator = ReleaseValidator::new(ctx);
  let target_crates = targets.clone().unwrap_or_else(|| ctx.graph.workspace_members());
  validator.validate(&target_crates, release_config.require_clean)?;

  // Build release plan
  eprintln!("🔍 Building release plan...\n");
  let planner = ReleasePlanner::new(ctx, release_config);
  let plan = planner.plan(targets, &bump_type)?;

  // Display plan with skip flags reflected
  println!("{}", plan.format_summary_with_flags(skip_publish, skip_tag));

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

/// Initialize release configuration for one or more crates
///
/// Detects workspace members and adds release configuration to rail.toml.
/// Can configure specific crates or all workspace members.
pub fn run_release_init(ctx: &WorkspaceContext, crates: Option<&str>, dry_run: bool) -> RailResult<()> {
  use crate::config::{ChangelogConfig, CrateReleaseConfig, RailConfig};
  use std::fs;

  // Parse crate names if provided
  let requested_crates: Option<Vec<String>> = crates.map(|s| {
    s.split(',')
      .map(|name| name.trim().to_string())
      .filter(|name| !name.is_empty())
      .collect()
  });

  // Get workspace members
  let members = ctx.cargo.metadata().workspace_packages();
  let workspace_root = ctx.workspace_root();

  // Filter to requested crates or use all
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
        "No matching crates found for: {}",
        requested.join(", ")
      )));
    } else {
      return Err(crate::error::RailError::message("No workspace members found"));
    }
  }

  // Load existing config or create new one
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

  // Track which crates are new vs already configured
  let mut new_crates = Vec::new();
  let mut existing_crates = Vec::new();

  for pkg in target_crates {
    if config.crates.contains_key(pkg.name.as_str()) {
      // Check if it has release config
      if config.crates[pkg.name.as_str()].release.is_some() {
        existing_crates.push(pkg.name.clone());
        continue;
      }
    }

    new_crates.push(pkg.name.clone());

    // Detect per-crate CHANGELOG file
    let crate_dir = pkg.manifest_path.parent().expect("manifest has parent");
    let changelog_path = crate::utils::detect_crate_changelog(crate_dir);

    // Get or create crate config
    let crate_config = config.crates.entry(pkg.name.to_string()).or_default();

    // Add/update release config
    crate_config.release = Some(CrateReleaseConfig {
      publish: pkg.publish.as_ref().map(|p| !p.is_empty()).unwrap_or(true),
    });

    // Add changelog config if detected
    if let Some(path) = changelog_path {
      crate_config.changelog = Some(ChangelogConfig {
        path: Some(path),
        skip: false,
      });
    }
  }

  if !existing_crates.is_empty() {
    println!(
      "ℹ️  Skipping {} crate(s) with existing release config:",
      existing_crates.len()
    );
    for name in &existing_crates {
      println!("  • {}", name);
    }
    println!();
  }

  if new_crates.is_empty() {
    println!("✅ All requested crates already have release configuration");
    return Ok(());
  }

  println!("Adding release configuration for {} crate(s):", new_crates.len());
  for name in &new_crates {
    println!("  • {}", name);
  }
  println!();

  // Serialize config
  let config_toml = toml_edit::ser::to_string_pretty(&config)
    .map_err(|e| crate::error::RailError::message(format!("Failed to serialize config: {}", e)))?;

  if dry_run {
    println!("🔍 DRY-RUN MODE - Config that would be written:\n");
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
