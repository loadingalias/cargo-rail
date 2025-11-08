use anyhow::{Context, Result};
use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::cargo::metadata::WorkspaceMetadata;
use crate::core::config::{RailConfig, SplitConfig, SplitMode};

/// Run the init command to set up cargo-rail configuration
pub fn run_init(all: bool) -> Result<()> {
  // Find workspace root
  let current_dir = env::current_dir().context("Failed to get current directory")?;
  let workspace_root = find_workspace_root(&current_dir)?;

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
  let metadata = WorkspaceMetadata::load(&workspace_root)?;
  let crates = metadata.list_crates();

  if crates.is_empty() {
    anyhow::bail!("No crates found in workspace");
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

    parse_selection(&input, crates.len())?
  };

  if selected_indices.is_empty() {
    println!("No crates selected. Exiting.");
    return Ok(());
  }

  // Create config with selected crates
  let mut config = RailConfig::new(workspace_root.clone());

  println!("\n🔧 Configuring selected crates...");
  for idx in selected_indices {
    let pkg = &crates[idx];
    println!("\n📦 Configuring: {}", pkg.name);

    // Prompt for remote URL
    print!("  Remote URL (e.g., git@github.com:user/{}.git): ", pkg.name);
    io::stdout().flush()?;
    let mut remote = String::new();
    io::stdin().read_line(&mut remote)?;
    let remote = remote.trim().to_string();

    if remote.is_empty() {
      println!("  ⚠️  Skipping {} (no remote provided)", pkg.name);
      continue;
    }

    // Default to single mode and main branch
    let crate_path = pkg
      .manifest_path
      .parent()
      .unwrap()
      .strip_prefix(&workspace_root)
      .unwrap()
      .to_path_buf();

    config.splits.push(SplitConfig {
      name: pkg.name.to_string(),
      path: Some(crate_path.into()),
      paths: None,
      remote,
      branch: "main".to_string(),
      mode: SplitMode::Single,
      include: vec![
        "src/**".to_string(),
        "Cargo.toml".to_string(),
        "README.md".to_string(),
        "LICENSE".to_string(),
        "rust-toolchain.toml".to_string(),
      ],
      exclude: vec![],
    });

    println!("  ✅ Configured {}", pkg.name);
  }

  // Save configuration
  println!("\n💾 Saving configuration...");
  config.save(&workspace_root)?;

  println!("\n✅ Successfully initialized cargo-rail!");
  println!(
    "   Configuration saved to: {}/.rail/config.toml",
    workspace_root.display()
  );
  println!("\n🚀 Next steps:");
  println!("   1. Create empty target repositories on GitHub/GitLab");
  println!("   2. Run: cargo rail split --all");

  Ok(())
}

/// Find the workspace root by looking for Cargo.toml with \[workspace\]
fn find_workspace_root(start: &Path) -> Result<PathBuf> {
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
fn parse_selection(input: &str, max: usize) -> Result<Vec<usize>> {
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
