//! Release preparation: version bumping and changelog generation

use crate::commands::release::{changelog, plan};
use crate::core::error::RailResult;
use cargo_metadata::MetadataCommand;
use similar::{ChangeTag, TextDiff};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::DocumentMut;

/// Run release prepare command
pub fn run_release_prepare(crate_name: Option<&str>, apply: bool, no_changelog: bool) -> RailResult<()> {
  // Load workspace metadata
  let metadata = load_workspace_metadata()?;
  let workspace_root = metadata.workspace_root.as_std_path();

  // TODO: Refactor plan.rs to export a function that returns ReleasePlan directly
  // For now, implement a simplified version that processes all workspace crates
  println!("📊 Analyzing workspace for release plan...");
  plan::run_release_plan(crate_name, false, false)?;
  println!();
  let workspace_pkgs: Vec<_> = metadata.workspace_packages().iter().cloned().cloned().collect();

  // Collect crate paths
  let crate_paths: HashMap<String, PathBuf> = workspace_pkgs
    .iter()
    .map(|pkg| {
      let crate_path = pkg
        .manifest_path
        .parent()
        .map(|p| p.as_std_path().to_path_buf())
        .unwrap_or_else(|| workspace_root.to_path_buf());
      (pkg.name.to_string(), crate_path)
    })
    .collect();

  // Filter to specific crate if requested
  let pkgs_to_process: Vec<_> = if let Some(name) = crate_name {
    workspace_pkgs
      .iter()
      .filter(|pkg| pkg.name.as_str() == name)
      .cloned()
      .collect()
  } else {
    workspace_pkgs.clone()
  };

  if pkgs_to_process.is_empty() {
    if let Some(name) = crate_name {
      return Err(anyhow::anyhow!("Crate '{}' not found in workspace", name).into());
    } else {
      println!("ℹ️  No crates to prepare");
      return Ok(());
    }
  }

  println!("📦 Preparing {} crate(s) for release", pkgs_to_process.len());
  println!();

  // Process each crate
  for pkg in &pkgs_to_process {
    let crate_path = crate_paths
      .get(pkg.name.as_str())
      .ok_or_else(|| anyhow::anyhow!("Failed to find path for crate {}", pkg.name))?;

    // For this simple implementation, we'll just bump patch version
    // TODO: Use actual release plan to determine correct bump
    let current_version = pkg.version.to_string();
    let next_version = bump_patch_version(&current_version)?;

    println!("📌 {} ({} → {})", pkg.name, current_version, next_version);

    // Bump version in Cargo.toml
    let cargo_toml_path = crate_path.join("Cargo.toml");
    let (old_manifest, new_manifest) = bump_version_in_manifest(&cargo_toml_path, &next_version)?;

    if apply {
      // Apply the change
      fs::write(&cargo_toml_path, &new_manifest)
        .map_err(|e| anyhow::anyhow!("Failed to write {}: {}", cargo_toml_path.display(), e))?;
      println!("   ✅ Updated Cargo.toml");
    } else {
      // Show diff
      show_diff("Cargo.toml", &old_manifest, &new_manifest);
    }

    // Generate changelog (if not disabled)
    if !no_changelog {
      let changelog_path = crate_path.join("CHANGELOG.md");
      let changelog_exists = changelog_path.exists();
      let old_changelog = if changelog_exists {
        fs::read_to_string(&changelog_path).unwrap_or_default()
      } else {
        String::new()
      };

      // Generate new changelog content
      let relative_path = crate_path
        .strip_prefix(workspace_root)
        .ok()
        .and_then(|p| p.to_str())
        .unwrap_or(".");

      let new_changelog = match changelog::generate_changelog_for_crate(
        workspace_root,
        pkg.name.as_str(),
        relative_path,
        Some(&current_version),
        &next_version,
      ) {
        Ok(content) => content,
        Err(e) => {
          eprintln!("   ⚠️  Warning: Failed to generate changelog: {}", e);
          continue;
        }
      };

      if apply {
        // Write changelog
        fs::write(&changelog_path, &new_changelog)
          .map_err(|e| anyhow::anyhow!("Failed to write CHANGELOG.md: {}", e))?;
        println!("   ✅ Generated CHANGELOG.md");
      } else {
        // Show diff
        show_diff("CHANGELOG.md", &old_changelog, &new_changelog);
      }
    }

    println!();
  }

  // Summary
  if !apply {
    println!("💡 This was a dry-run. Use --apply to make these changes.");
  } else {
    println!("✅ Release preparation complete!");
    println!();
    println!("Next steps:");
    println!("  1. Review changes: git diff");
    println!("  2. Run tests: cargo test --workspace");
    println!("  3. Commit: git add . && git commit -m \"chore: prepare release\"");
    println!("  4. Publish: cargo rail release publish --apply");
  }

  Ok(())
}

/// Load workspace metadata using cargo_metadata
fn load_workspace_metadata() -> RailResult<cargo_metadata::Metadata> {
  let current_dir = env::current_dir().map_err(|e| anyhow::anyhow!("Failed to get current directory: {}", e))?;

  Ok(MetadataCommand::new().current_dir(&current_dir).exec().map_err(|e| {
    anyhow::anyhow!(
      "Failed to load workspace metadata. Are you in a Cargo workspace?\n  Error: {}",
      e
    )
  })?)
}

/// Bump version in a Cargo.toml file
fn bump_version_in_manifest(path: &Path, new_version: &str) -> RailResult<(String, String)> {
  let old_content =
    fs::read_to_string(path).map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?;

  let mut doc = old_content
    .parse::<DocumentMut>()
    .map_err(|e| anyhow::anyhow!("Failed to parse TOML: {}", e))?;

  // Update package version
  if let Some(package) = doc.get_mut("package") {
    if let Some(table) = package.as_table_mut() {
      table["version"] = toml_edit::value(new_version);
    }
  } else {
    return Err(anyhow::anyhow!("No [package] section found in {}", path.display()).into());
  }

  let new_content = doc.to_string();
  Ok((old_content, new_content))
}

/// Simple patch version bump (temporary - should use release plan)
fn bump_patch_version(current: &str) -> RailResult<String> {
  let mut version: semver::Version = current
    .parse()
    .map_err(|e| anyhow::anyhow!("Invalid semver version '{}': {}", current, e))?;

  version.patch += 1;
  Ok(version.to_string())
}

/// Show a unified diff between old and new content
fn show_diff(filename: &str, old: &str, new: &str) {
  if old == new {
    println!("   (no changes to {})", filename);
    return;
  }

  println!("   📝 {}", filename);
  println!("   {}", "─".repeat(60));

  let diff = TextDiff::from_lines(old, new);
  let mut line_count = 0;
  const MAX_LINES: usize = 20;

  for change in diff.iter_all_changes() {
    if line_count >= MAX_LINES {
      println!("   ... ({} more lines)", diff.iter_all_changes().count() - line_count);
      break;
    }

    let (sign, color) = match change.tag() {
      ChangeTag::Delete => ("- ", "\x1b[31m"), // Red
      ChangeTag::Insert => ("+ ", "\x1b[32m"), // Green
      ChangeTag::Equal => ("  ", "\x1b[0m"),   // Normal
    };

    print!("   {}{}{}\x1b[0m", color, sign, change);
    line_count += 1;
  }

  println!();
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_bump_patch_version() {
    assert_eq!(bump_patch_version("1.2.3").unwrap(), "1.2.4");
    assert_eq!(bump_patch_version("0.1.0").unwrap(), "0.1.1");
    assert_eq!(bump_patch_version("2.0.0").unwrap(), "2.0.1");
  }

  #[test]
  fn test_bump_version_invalid() {
    assert!(bump_patch_version("invalid").is_err());
    assert!(bump_patch_version("1.2").is_err());
  }
}
