use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::cargo::metadata::WorkspaceMetadata;
use crate::core::config::{CratePath, RailConfig, SplitConfig, SplitMode};
use crate::core::error::{RailError, RailResult, ResultExt};

/// Run the init command to set up cargo-rail configuration
pub fn run_init(all: bool) -> RailResult<()> {
  // Find Cargo workspace root
  let current_dir = env::current_dir()?;
  let workspace_root = find_workspace_root(&current_dir)?;

  println!("📦 Found Cargo workspace at: {}", workspace_root.display());

  // Check if config already exists
  if RailConfig::exists(&workspace_root) {
    print!("⚠️  Configuration already exists. Overwrite? [y/N]: ");
    io::stdout().flush()?;
    let mut response = String::new();
    io::stdin().read_line(&mut response)?;
    if !response.trim().eq_ignore_ascii_case("y") {
      println!("Aborted.");
      return Ok(());
    }
  }

  // Load Cargo workspace metadata
  println!("🔍 Discovering workspace crates...");
  let metadata = WorkspaceMetadata::load(&workspace_root)?;
  let packages = metadata.list_crates();

  if packages.is_empty() {
    return Err(RailError::Validation(
      crate::core::error::ValidationError::WorkspaceInvalid {
        reason: "No crates found in workspace".to_string(),
      },
    ));
  }

  println!("\n📋 Found {} crates:", packages.len());
  for (idx, pkg) in packages.iter().enumerate() {
    let pkg_path = pkg
      .manifest_path
      .parent()
      .and_then(|p| p.strip_prefix(&workspace_root).ok())
      .map(|p| p.as_str())
      .unwrap_or_else(|| pkg.manifest_path.as_str());
    println!("  {}. {} v{} ({})", idx + 1, pkg.name, pkg.version, pkg_path);
  }

  // Select packages to track
  let selected_indices = if all {
    println!("\n✨ Using --all flag, selecting all packages");
    (0..packages.len()).collect()
  } else {
    print!("\nEnter package numbers to track (comma-separated, e.g., 1,3,5): ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    parse_selection(&input, packages.len())?
  };

  if selected_indices.is_empty() {
    println!("No packages selected. Exiting.");
    return Ok(());
  }

  // Create config with selected crates
  let mut config = RailConfig::new(workspace_root.clone());

  println!("\n🔧 Scaffolding configuration for selected crates...");
  for idx in selected_indices {
    let pkg = &packages[idx];

    // Get crate path relative to workspace root
    let package_path = pkg
      .manifest_path
      .parent()
      .ok_or_else(|| RailError::message(format!("Invalid manifest path for crate '{}'", pkg.name)))?
      .strip_prefix(&workspace_root)
      .with_context(|| format!("Crate '{}' is not within workspace root", pkg.name))?
      .to_path_buf()
      .into(); // Convert Utf8PathBuf to PathBuf

    // Standard Cargo include patterns
    let include_patterns = vec![
      "src/**".to_string(),
      "tests/**".to_string(),
      "examples/**".to_string(),
      "benches/**".to_string(),
      "Cargo.toml".to_string(),
    ];

    config.splits.push(SplitConfig {
      name: pkg.name.to_string(),
      remote: String::new(), // Empty - user will fill this in
      branch: "main".to_string(),
      mode: SplitMode::Single,
      paths: vec![CratePath { path: package_path }],
      include: include_patterns,
      exclude: vec![],
    });

    println!("  ✅ {}", pkg.name);
  }

  // Save configuration
  println!("\n💾 Saving configuration...");
  config.save(&workspace_root)?;

  println!("\n✅ Successfully initialized cargo-rail!");
  println!("   Configuration saved to: {}/rail.toml", workspace_root.display());
  println!("\n🚀 Next steps:");
  println!("   1. Edit rail.toml and fill in the remote URLs for each crate");
  println!("      Example: remote = \"git@github.com:user/crate-name.git\"");
  println!("   2. Create empty target repositories on GitHub/GitLab");
  println!("   3. Run: cargo rail split <crate-name>");

  Ok(())
}

/// Find the Cargo workspace root
fn find_workspace_root(start: &Path) -> RailResult<PathBuf> {
  let mut current = start.to_path_buf();

  loop {
    let cargo_toml = current.join("Cargo.toml");

    // Check if this is a Cargo workspace
    if cargo_toml.exists()
      && let Ok(content) = std::fs::read_to_string(&cargo_toml)
      && content.contains("[workspace]")
    {
      return Ok(current);
    }

    // Try parent directory
    if let Some(parent) = current.parent() {
      current = parent.to_path_buf();
    } else {
      return Err(RailError::with_help(
        "Could not find Cargo workspace root",
        "cargo-rail requires a Cargo.toml file with a [workspace] section. Run this command from within a Rust workspace.",
      ));
    }
  }
}

/// Parse user selection input like "1,3,5" or "1-3,5"
fn parse_selection(input: &str, max: usize) -> RailResult<Vec<usize>> {
  let mut indices = Vec::new();
  let input = input.trim();

  if input.is_empty() {
    return Ok(indices);
  }

  for part in input.split(',') {
    let part = part.trim();
    if part.contains('-') {
      // Range like "1-3"
      let parts: Vec<&str> = part.split('-').collect();
      if parts.len() != 2 {
        return Err(RailError::message(format!("Invalid range format: {}", part)));
      }
      let start: usize = parts[0]
        .trim()
        .parse()
        .with_context(|| format!("Invalid number: {}", parts[0]))?;
      let end: usize = parts[1]
        .trim()
        .parse()
        .with_context(|| format!("Invalid number: {}", parts[1]))?;

      if start == 0 || end == 0 || start > max || end > max {
        return Err(RailError::message(format!("Range out of bounds: {}", part)));
      }
      if start > end {
        return Err(RailError::message(format!("Invalid range (start > end): {}", part)));
      }

      for i in start..=end {
        indices.push(i - 1); // Convert to 0-indexed
      }
    } else {
      // Single number
      let num: usize = part.parse().with_context(|| format!("Invalid number: {}", part))?;
      if num == 0 || num > max {
        return Err(RailError::message(format!("Number out of range: {}", num)));
      }
      indices.push(num - 1); // Convert to 0-indexed
    }
  }

  // Remove duplicates and sort
  indices.sort_unstable();
  indices.dedup();

  Ok(indices)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_parse_selection() {
    assert_eq!(parse_selection("1,3,5", 10).unwrap(), vec![0, 2, 4]);
    assert_eq!(parse_selection("1-3", 10).unwrap(), vec![0, 1, 2]);
    assert_eq!(parse_selection("1-3,5,7-8", 10).unwrap(), vec![0, 1, 2, 4, 6, 7]);
    assert_eq!(parse_selection("3,1,2", 10).unwrap(), vec![0, 1, 2]); // sorted and deduped
    assert!(parse_selection("0", 10).is_err()); // 0 is invalid
    assert!(parse_selection("11", 10).is_err()); // out of range
    assert!(parse_selection("5-3", 10).is_err()); // invalid range
  }
}
