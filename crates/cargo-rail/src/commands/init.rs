use anyhow::Context;
use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::cargo::metadata::WorkspaceMetadata;
use crate::core::config::{CratePath, RailConfig, SplitConfig, SplitMode};
use crate::core::error::{RailError, RailResult};

/// Run the init command to set up cargo-rail configuration
pub fn run_init(all: bool) -> RailResult<()> {
  // Find workspace root
  let current_dir = env::current_dir()?;
  let workspace_root = find_workspace_root(&current_dir).map_err(RailError::Other)?;

  println!("📦 Found workspace at: {}", workspace_root.display());

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

  // Load workspace metadata
  println!("🔍 Discovering workspace crates...");
  let metadata = WorkspaceMetadata::load(&workspace_root).map_err(RailError::Other)?;
  let crates = metadata.list_crates();

  if crates.is_empty() {
    return Err(RailError::Other(anyhow::anyhow!("No crates found in workspace")));
  }

  println!("\n📋 Found {} crates:", crates.len());
  for (idx, pkg) in crates.iter().enumerate() {
    println!("  {}. {} ({})", idx + 1, pkg.name, pkg.manifest_path.parent().unwrap());
  }

  // Select crates to track
  let selected_indices = if all {
    println!("\n✨ Using --all flag, selecting all crates");
    (0..crates.len()).collect()
  } else {
    print!("\nEnter crate numbers to track (comma-separated, e.g., 1,3,5): ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    parse_selection(&input, crates.len()).map_err(RailError::Other)?
  };

  if selected_indices.is_empty() {
    println!("No crates selected. Exiting.");
    return Ok(());
  }

  // Create config with selected crates
  let mut config = RailConfig::new(workspace_root.clone());

  println!("\n🔧 Scaffolding configuration for selected crates...");
  for idx in selected_indices {
    let pkg = &crates[idx];

    // Get crate path relative to workspace root
    let crate_path = pkg
      .manifest_path
      .parent()
      .unwrap()
      .strip_prefix(&workspace_root)
      .unwrap()
      .to_path_buf();

    config.splits.push(SplitConfig {
      name: pkg.name.to_string(),
      remote: String::new(), // Empty - user will fill this in
      branch: "main".to_string(),
      mode: SplitMode::Single,
      paths: vec![CratePath {
        path: crate_path.into(),
      }],
      include: vec![
        "src/**".to_string(),
        "tests/**".to_string(),
        "examples/**".to_string(),
        "benches/**".to_string(),
        "Cargo.toml".to_string(),
      ],
      exclude: vec![],
    });

    println!("  ✅ {}", pkg.name);
  }

  // Save configuration
  println!("\n💾 Saving configuration...");
  config.save(&workspace_root).map_err(RailError::Other)?;

  println!("\n✅ Successfully initialized cargo-rail!");
  println!("   Configuration saved to: {}/rail.toml", workspace_root.display());
  println!("\n🚀 Next steps:");
  println!("   1. Edit rail.toml and fill in the remote URLs for each crate");
  println!("      Example: remote = \"git@github.com:user/crate-name.git\"");
  println!("   2. Create empty target repositories on GitHub/GitLab");
  println!("   3. Run: cargo rail split <crate-name>");

  Ok(())
}

/// Find the workspace root by looking for Cargo.toml with \[workspace\]
fn find_workspace_root(start: &Path) -> anyhow::Result<PathBuf> {
  let mut current = start.to_path_buf();

  loop {
    let cargo_toml = current.join("Cargo.toml");
    if cargo_toml.exists() {
      // Check if it's a workspace
      let content = std::fs::read_to_string(&cargo_toml).context("Failed to read Cargo.toml")?;
      if content.contains("[workspace]") {
        return Ok(current);
      }
    }

    // Try parent directory
    if let Some(parent) = current.parent() {
      current = parent.to_path_buf();
    } else {
      anyhow::bail!("Could not find workspace root (no Cargo.toml with [workspace] found)");
    }
  }
}

/// Parse user selection input like "1,3,5" or "1-3,5"
fn parse_selection(input: &str, max: usize) -> anyhow::Result<Vec<usize>> {
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
        anyhow::bail!("Invalid range format: {}", part);
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
        anyhow::bail!("Range out of bounds: {}", part);
      }
      if start > end {
        anyhow::bail!("Invalid range (start > end): {}", part);
      }

      for i in start..=end {
        indices.push(i - 1); // Convert to 0-indexed
      }
    } else {
      // Single number
      let num: usize = part.parse().with_context(|| format!("Invalid number: {}", part))?;
      if num == 0 || num > max {
        anyhow::bail!("Number out of range: {}", num);
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
