//! Generate a sparse cargo-rail configuration.

use crate::error::{RailError, RailResult};
use crate::progress;
use crate::toml::TomlFormatter;
use crate::workspace::WorkspaceContext;
use std::fs;
use std::path::{Path, PathBuf};

/// Generate rail.toml configuration
pub fn run_init(ctx: &WorkspaceContext, output_path: &str, force: bool, dry_run: bool, json: bool) -> RailResult<()> {
  run_init_impl(ctx.workspace_root(), output_path, force, dry_run, json)
}

/// Detect target triples across config files in the workspace
///
/// Uses fuzzy matching against rustc's canonical target list to find targets
/// regardless of where they're defined (rust-toolchain.toml, .cargo/config.toml,
/// Cross.toml, dist-workspace.toml, .github/workflows/*.yml, etc.).
///
/// Returns sorted list of detected targets.
fn detect_targets(workspace_root: &Path) -> RailResult<Vec<String>> {
  crate::targets::detect_targets(workspace_root)
}

fn build_sparse_config(targets: &[String]) -> String {
  let mut content =
    String::from("# cargo-rail configuration\n# Documentation: https://github.com/loadingalias/cargo-rail\n");
  if !targets.is_empty() {
    content.push_str("\n# Additional supported Cargo target-resolution views detected from repository files.\n");
    content.push_str(&format!("targets = {}\n", TomlFormatter::new().array_targets(targets)));
  }
  content
}

/// Check if config file already exists at any location
fn check_existing_config(workspace_root: &Path) -> Option<PathBuf> {
  crate::config::RailConfig::find_config_path(workspace_root)
}

/// Ensure output directory exists
fn ensure_output_dir(output_path: &Path) -> RailResult<()> {
  if let Some(parent) = output_path.parent() {
    fs::create_dir_all(parent)
      .map_err(|e| RailError::message(format!("failed to create {}: {}", parent.display(), e)))?;
  }
  Ok(())
}

/// Write config file atomically
fn write_config_file(config_path: &Path, content: &str) -> RailResult<()> {
  crate::utils::write_file_atomic(config_path, content.as_bytes())
}

/// Check if this is a valid Cargo workspace
fn is_cargo_workspace(workspace_root: &Path) -> bool {
  let cargo_toml = workspace_root.join("Cargo.toml");
  if !cargo_toml.exists() {
    return false;
  }
  // Check if it's a workspace (has [workspace] section or is a virtual workspace)
  if let Ok(content) = fs::read_to_string(&cargo_toml) {
    content.contains("[workspace]")
  } else {
    false
  }
}

/// Shared implementation for both run_init and run_init_standalone
fn run_init_impl(workspace_root: &Path, output_path: &str, force: bool, dry_run: bool, json: bool) -> RailResult<()> {
  let config_path = workspace_root.join(output_path);

  // Warn if not a Cargo workspace (skip warnings in quiet/JSON mode)
  if !is_cargo_workspace(workspace_root) && !dry_run && !json && !crate::output::is_quiet() {
    crate::warn!("no Cargo workspace detected in {}", workspace_root.display());
    eprintln!("         cargo-rail works best with Cargo workspaces\n");
  }

  // Check if target path already exists
  // In --dry-run mode, we just preview, so existence doesn't matter.
  if config_path.exists() && !dry_run {
    if !force {
      return Err(RailError::with_help(
        format!("configuration exists: {}", config_path.display()),
        "use --force to overwrite, or use --dry-run to preview what would be generated",
      ));
    }
    if !json {
      progress!("overwriting: {}", config_path.display());
    }
  } else if let Some(existing) = check_existing_config(workspace_root) {
    // A config exists elsewhere - warn but allow writing to different path
    if !dry_run && !json && !crate::output::is_quiet() {
      crate::note!("existing config found at {}", existing.display());
      eprintln!("      (search order: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml)\n");
    }
  }

  let detected_targets = detect_targets(workspace_root)?;
  let toml_content = build_sparse_config(&detected_targets);

  if dry_run {
    if json {
      let payload = serde_json::json!({
        "command": "init",
        "dry_run": true,
        "config_path": config_path.display().to_string(),
        "targets_detected": detected_targets,
        "content": toml_content
      });
      let output = crate::output::machine_json_envelope("init", "dry_run", "success", 0, payload);
      println!(
        "{}",
        serde_json::to_string_pretty(&output)
          .map_err(|error| RailError::message(format!("failed to serialize init result: {error}")))?
      );
    } else {
      println!("{}", toml_content);
    }
  } else {
    ensure_output_dir(&config_path)?;
    write_config_file(&config_path, &toml_content)?;

    if json {
      let payload = serde_json::json!({
        "command": "init",
        "status": "created",
        "config_path": config_path.display().to_string(),
        "targets_detected": detected_targets
      });
      let output = crate::output::machine_json_envelope("init", "apply", "created", 0, payload);
      println!(
        "{}",
        serde_json::to_string_pretty(&output)
          .map_err(|error| RailError::message(format!("failed to serialize init result: {error}")))?
      );
    } else {
      progress!("created: {}", config_path.display());
      progress!("\nnext: cargo rail unify --check");
    }
  }

  Ok(())
}

/// Standalone init (without WorkspaceContext)
pub fn run_init_standalone(
  workspace_root: &Path,
  output_path: &str,
  force: bool,
  dry_run: bool,
  json: bool,
) -> RailResult<()> {
  run_init_impl(workspace_root, output_path, force, dry_run, json)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn sparse_config_omits_coded_defaults() {
    let toml = build_sparse_config(&[]);
    assert!(toml.contains("cargo-rail configuration"));
    assert!(toml.contains("Documentation:"));
    assert!(!toml.contains("[unify]"));
    assert!(!toml.contains("[release]"));
  }

  #[test]
  fn sparse_config_writes_detected_targets() {
    let toml = build_sparse_config(&["wasm32-unknown-unknown".to_string()]);
    assert!(toml.contains("targets = ["));
    assert!(toml.contains("wasm32-unknown-unknown"));
  }
}
