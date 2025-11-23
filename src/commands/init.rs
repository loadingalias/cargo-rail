//! `cargo rail init` - Initialize cargo-rail configuration
//!
//! Auto-detects workspace structure, toolchain settings, and generates
//! a sensible .config/rail.toml with smart defaults.

use crate::config::{RailConfig, UnifyConfig, WorkspaceConfig};
use crate::error::{RailError, RailResult};
use crate::toml::builder::RailConfigBuilder;
use crate::workspace::WorkspaceContext;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Run the init command to bootstrap rail.toml configuration
///
/// # Arguments
/// * `ctx` - Workspace context (already loaded, contains cargo metadata)
/// * `output_path` - Where to write rail.toml (relative to workspace root)
/// * `force` - Overwrite existing config file
/// * `non_interactive` - Skip all prompts, use defaults
/// * `dry_run` - Print config to stdout instead of writing
pub fn run_init(
  ctx: &WorkspaceContext,
  output_path: &str,
  force: bool,
  non_interactive: bool,
  dry_run: bool,
) -> RailResult<()> {
  let workspace_root = ctx.workspace_root();
  let config_path = workspace_root.join(output_path);

  // 1. Check for existing config
  if let Some(existing) = check_existing_config(workspace_root) {
    if !force {
      return Err(RailError::with_help(
        format!(
          "Configuration already exists at: {}\nUse --force to overwrite or --output to specify a different location",
          existing.display()
        ),
        "Example: cargo rail init --force",
      ));
    }
    if !dry_run {
      println!("⚠️  Overwriting existing config at: {}", existing.display());
    }
  }

  // 2. Detection phase
  let mut unify = default_unify_config();

  // Auto-detect targets from all TOML files in workspace (silent detection)
  let detected_targets = detect_targets(workspace_root);
  unify.validation.targets = detected_targets;

  // 3. Interactive confirmation (unless non-interactive or dry-run)
  if !non_interactive && !dry_run {
    print!("\nGenerate rail.toml with these settings? [Y/n]: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();

    if input == "n" || input == "no" {
      println!("Cancelled.");
      return Ok(());
    }
  }

  // 4. Build config
  let config = build_rail_config(workspace_root.to_path_buf(), unify);

  // 6. Serialize with comments using Builder
  let toml_content = RailConfigBuilder::new()
    .header()
    .workspace(&config.workspace.root)
    .unify(&config.unify)
    .release(&config.release)
    .splits_template()
    .build()?;

  // 5. Write or print
  if dry_run {
    println!("{}", toml_content);
  } else {
    ensure_output_dir(&config_path)?;
    write_config_file(&config_path, &toml_content)?;

    println!("✅ Created {}", config_path.display());
    println!("\nNext steps:");
    println!("  cargo rail unify --dry-run  # Preview dependency unification");
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

fn build_rail_config(_workspace_root: PathBuf, unify: UnifyConfig) -> RailConfig {
  RailConfig {
    workspace: WorkspaceConfig {
      root: PathBuf::from("."),
    },
    unify,
    release: crate::config::ReleaseConfig::default(),
    splits: vec![], // Empty by default - use 'cargo rail split init' to configure
    formatting: crate::config::FormattingConfig::default(),
  }
}

/// Check if config file already exists at any location
fn check_existing_config(workspace_root: &Path) -> Option<PathBuf> {
  crate::config::RailConfig::find_config_path(workspace_root)
}

/// Ensure output directory exists (create if needed)
fn ensure_output_dir(output_path: &Path) -> RailResult<()> {
  if let Some(parent) = output_path.parent()
    && !parent.exists()
  {
    println!("📁 Creating directory: {}", parent.display());
    fs::create_dir_all(parent)
      .map_err(|e| RailError::message(format!("Failed to create directory {}: {}", parent.display(), e)))?;
  }
  Ok(())
}

/// Write config to file with atomic write (write to temp, then rename)
fn write_config_file(config_path: &Path, content: &str) -> RailResult<()> {
  // Use atomic write pattern (write to temp, then rename)
  let temp_path = config_path.with_extension("toml.tmp");

  fs::write(&temp_path, content).map_err(|e| {
    RailError::with_help(
      format!("Failed to write config to {}: {}", temp_path.display(), e),
      "Check file permissions and ensure the directory is writable",
    )
  })?;

  fs::rename(&temp_path, config_path)
    .map_err(|e| RailError::message(format!("Failed to finalize config file: {}", e)))?;

  Ok(())
}

/// Standalone init that doesn't require WorkspaceContext
///
/// This is used when init is called on a directory that may not have
/// a valid Cargo workspace yet (e.g., empty workspace or invalid state).
pub fn run_init_standalone(
  workspace_root: &Path,
  output_path: &str,
  force: bool,
  _non_interactive: bool,
  dry_run: bool,
) -> RailResult<()> {
  let config_path = workspace_root.join(output_path);

  // 1. Check for existing config
  if let Some(existing) = check_existing_config(workspace_root) {
    if !force {
      return Err(RailError::with_help(
        format!(
          "Configuration already exists at: {}\nUse --force to overwrite or --output to specify a different location",
          existing.display()
        ),
        "Example: cargo rail init --force",
      ));
    }
    if !dry_run {
      println!("⚠️  Overwriting existing config at: {}", existing.display());
    }
  }

  // 2. Detection phase - silent target detection
  let detected_targets = detect_targets(workspace_root);

  // 3. Build config
  let config = RailConfig {
    workspace: WorkspaceConfig {
      root: PathBuf::from("."),
    },
    unify: UnifyConfig {
      allow_renamed: false,
      exclude: Vec::new(),
      include: Vec::new(),
      consolidate_transitives: false,
      transitive_host: Default::default(),
      validation: crate::config::UnifyValidationConfig {
        targets: detected_targets,
        max_parallel_jobs: 0,
      },
      output: crate::config::UnifyOutputConfig::default(),
      backup: crate::config::UnifyBackupConfig::default(),
    },
    release: crate::config::ReleaseConfig::default(),
    splits: vec![],
    formatting: crate::config::FormattingConfig::default(),
  };

  // 5. Serialize with comments using Builder
  let config_toml = RailConfigBuilder::new()
    .header()
    .workspace(&config.workspace.root)
    .unify(&config.unify)
    .release(&config.release)
    .splits_template()
    .build()?;

  // 4. Output
  if dry_run {
    println!("{}", config_toml);
  } else {
    // Create parent directory if needed
    if let Some(parent) = config_path.parent() {
      fs::create_dir_all(parent).map_err(|e| {
        RailError::with_help(
          format!("Failed to create directory {}: {}", parent.display(), e),
          "Check file permissions",
        )
      })?;
    }

    write_config_file(&config_path, &config_toml)?;
    println!("✅ Created {}", config_path.display());
    println!("\nNext steps:");
    println!("  cargo rail unify --dry-run  # Preview dependency unification");
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
      workspace: WorkspaceConfig {
        root: PathBuf::from("."),
      },
      unify: UnifyConfig::default(),
      release: ReleaseConfig::default(),
      splits: vec![],
      formatting: FormattingConfig::default(),
    };

    let toml = RailConfigBuilder::new()
      .header()
      .workspace(&config.workspace.root)
      .unify(&config.unify)
      .release(&config.release)
      .splits_template()
      .build()
      .unwrap();

    // Should contain section headers
    assert!(toml.contains("[workspace]"));
    assert!(toml.contains("[unify]"));

    // Should contain helpful comments
    assert!(toml.contains("cargo-rail configuration"));
    assert!(toml.contains("Documentation:"));
  }
}
