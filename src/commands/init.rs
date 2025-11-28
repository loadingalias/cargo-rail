//! `cargo rail init` - Initialize cargo-rail configuration
//!
//! Auto-detects workspace structure, toolchain settings, and generates
//! a sensible .config/rail.toml with smart defaults.

use crate::config::{RailConfig, UnifyConfig};
use crate::error::{RailError, RailResult};
use crate::toml::builder::RailConfigBuilder;
use crate::workspace::WorkspaceContext;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Generate rail.toml configuration
pub fn run_init(
  ctx: &WorkspaceContext,
  output_path: &str,
  force: bool,
  non_interactive: bool,
  check: bool,
) -> RailResult<()> {
  let workspace_root = ctx.workspace_root();
  let config_path = workspace_root.join(output_path);

  // Check if target path already exists
  if config_path.exists() {
    if !force {
      return Err(RailError::with_help(
        format!("configuration exists: {}", config_path.display()),
        "use --force to overwrite",
      ));
    }
    if !check {
      eprintln!("overwriting: {}", config_path.display());
    }
  } else if let Some(existing) = check_existing_config(workspace_root) {
    // A config exists elsewhere - warn but allow writing to different path
    if !check {
      eprintln!("note: existing config at {}", existing.display());
    }
  }

  let unify = default_unify_config();
  let detected_targets = detect_targets(workspace_root);

  if !non_interactive && !check {
    print!("generate rail.toml? [Y/n]: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();

    if input == "n" || input == "no" {
      println!("cancelled");
      return Ok(());
    }
  }

  let config = build_rail_config(detected_targets, unify);

  let toml_content = RailConfigBuilder::new()
    .header()
    .targets(&config.targets)
    .unify(&config.unify)
    .release(&config.release)
    .splits_template()
    .build()?;

  if check {
    println!("{}", toml_content);
  } else {
    ensure_output_dir(&config_path)?;
    write_config_file(&config_path, &toml_content)?;
    println!("created: {}", config_path.display());
    println!("\nnext: cargo rail unify --check");
  }

  Ok(())
}

/// Auto-detect reasonable unify defaults
fn default_unify_config() -> UnifyConfig {
  UnifyConfig::default()
}

/// Detect target triples across all TOML files in workspace
///
/// Uses fuzzy matching against rustc's canonical target list to find targets
/// regardless of where they're defined (rust-toolchain.toml, .cargo/config.toml,
/// Cross.toml, dist-workspace.toml, etc.).
///
/// Returns sorted list of detected targets.
fn detect_targets(workspace_root: &Path) -> Vec<String> {
  // Use the new comprehensive target detection
  crate::targets::detect_targets(workspace_root).unwrap_or_default()
}

fn build_rail_config(targets: Vec<String>, unify: UnifyConfig) -> RailConfig {
  RailConfig {
    targets,
    unify,
    release: crate::config::ReleaseConfig::default(),
    change_detection: crate::config::ChangeDetectionConfig::default(),
    crates: Default::default(),
    formatting: crate::config::FormattingConfig::default(),
  }
}

/// Check if config file already exists at any location
fn check_existing_config(workspace_root: &Path) -> Option<PathBuf> {
  crate::config::RailConfig::find_config_path(workspace_root)
}

/// Ensure output directory exists
fn ensure_output_dir(output_path: &Path) -> RailResult<()> {
  if let Some(parent) = output_path.parent()
    && !parent.exists()
  {
    fs::create_dir_all(parent)
      .map_err(|e| RailError::message(format!("failed to create {}: {}", parent.display(), e)))?;
  }
  Ok(())
}

/// Write config file atomically
fn write_config_file(config_path: &Path, content: &str) -> RailResult<()> {
  let temp_path = config_path.with_extension("toml.tmp");

  fs::write(&temp_path, content).map_err(|e| {
    RailError::with_help(
      format!("failed to write {}: {}", temp_path.display(), e),
      "check file permissions",
    )
  })?;

  fs::rename(&temp_path, config_path).map_err(|e| RailError::message(format!("failed to finalize config: {}", e)))?;

  Ok(())
}

/// Standalone init (without WorkspaceContext)
pub fn run_init_standalone(
  workspace_root: &Path,
  output_path: &str,
  force: bool,
  _non_interactive: bool,
  check: bool,
) -> RailResult<()> {
  let config_path = workspace_root.join(output_path);

  // Check if target path already exists
  if config_path.exists() {
    if !force {
      return Err(RailError::with_help(
        format!("configuration exists: {}", config_path.display()),
        "use --force to overwrite",
      ));
    }
    if !check {
      eprintln!("overwriting: {}", config_path.display());
    }
  } else if let Some(existing) = check_existing_config(workspace_root) {
    // A config exists elsewhere - warn but allow writing to different path
    if !check {
      eprintln!("note: existing config at {}", existing.display());
    }
  }

  let detected_targets = detect_targets(workspace_root);

  let config = RailConfig {
    targets: detected_targets,
    unify: UnifyConfig::default(),
    release: crate::config::ReleaseConfig::default(),
    change_detection: crate::config::ChangeDetectionConfig::default(),
    crates: Default::default(),
    formatting: crate::config::FormattingConfig::default(),
  };

  let config_toml = RailConfigBuilder::new()
    .header()
    .targets(&config.targets)
    .unify(&config.unify)
    .release(&config.release)
    .splits_template()
    .build()?;

  if check {
    println!("{}", config_toml);
  } else {
    if let Some(parent) = config_path.parent() {
      fs::create_dir_all(parent).map_err(|e| {
        RailError::with_help(
          format!("failed to create {}: {}", parent.display(), e),
          "check file permissions",
        )
      })?;
    }

    write_config_file(&config_path, &config_toml)?;
    println!("created: {}", config_path.display());
    println!("\nnext: cargo rail unify --check");
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::{FormattingConfig, ReleaseConfig};

  #[test]
  fn test_serialize_config_with_builder() {
    let config = RailConfig {
      targets: vec![],
      unify: UnifyConfig::default(),
      release: ReleaseConfig::default(),
      change_detection: crate::config::ChangeDetectionConfig::default(),
      crates: Default::default(),
      formatting: FormattingConfig::default(),
    };

    let toml = RailConfigBuilder::new()
      .header()
      .targets(&config.targets)
      .unify(&config.unify)
      .release(&config.release)
      .splits_template()
      .build()
      .unwrap();

    // Should contain section headers
    assert!(toml.contains("[unify]"));

    // Should contain helpful comments
    assert!(toml.contains("cargo-rail configuration"));
    assert!(toml.contains("Documentation:"));
  }
}
