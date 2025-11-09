//! Tests for the `sync` command

use crate::helpers::*;
use anyhow::Result;

#[test]
fn test_sync_mono_to_remote() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create and split a crate
  workspace.add_crate("my-crate", "0.1.0", &[])?;
  workspace.commit("Add my-crate")?;

  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let split_dir = workspace.path.join("split-repos/my-crate-split");
  let config = workspace.read_file("rail.toml")?;
  let updated_config = config.replace(r#"remote = """#, &format!(r#"remote = "{}""#, split_dir.display()));
  std::fs::write(workspace.path.join("rail.toml"), updated_config)?;

  run_cargo_rail(&workspace.path, &["rail", "split", "my-crate"])?;

  // Make changes in monorepo
  workspace.modify_file("my-crate", "src/lib.rs", "// Monorepo change\npub fn new() {}")?;
  workspace.commit("Update in monorepo")?;

  // Sync to remote
  run_cargo_rail(&workspace.path, &["rail", "sync", "my-crate", "--to-remote"])?;

  // Verify changes appear in split repo
  let split_lib = std::fs::read_to_string(split_dir.join("src/lib.rs"))?;
  assert!(split_lib.contains("Monorepo change"));
  assert!(split_lib.contains("pub fn new()"));

  // Verify commit exists in split repo
  let log = git(&split_dir, &["log", "-1", "--oneline"])?;
  let log_str = String::from_utf8_lossy(&log.stdout);
  assert!(log_str.contains("Update in monorepo"));

  Ok(())
}

#[test]
fn test_sync_remote_to_mono() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create and split a crate
  workspace.add_crate("my-crate", "0.1.0", &[])?;
  workspace.commit("Add my-crate")?;

  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let split_dir = workspace.path.join("split-repos/my-crate-split");
  let config = workspace.read_file("rail.toml")?;
  let updated_config = config.replace(r#"remote = """#, &format!(r#"remote = "{}""#, split_dir.display()));
  std::fs::write(workspace.path.join("rail.toml"), updated_config)?;

  run_cargo_rail(&workspace.path, &["rail", "split", "my-crate"])?;

  // Make changes in split repo
  std::fs::write(
    split_dir.join("src/lib.rs"),
    "// Split repo change\npub fn from_split() {}",
  )?;
  git(&split_dir, &["add", "."])?;
  git(&split_dir, &["commit", "-m", "Update in split repo"])?;

  // Sync from remote
  run_cargo_rail(&workspace.path, &["rail", "sync", "my-crate", "--from-remote"])?;

  // Verify changes appear in monorepo
  let mono_lib = workspace.read_file("crates/my-crate/src/lib.rs")?;
  assert!(mono_lib.contains("Split repo change"));
  assert!(mono_lib.contains("pub fn from_split()"));

  // Verify commit exists in monorepo
  let log = workspace.git_log(1)?;
  assert!(log[0].contains("Update in split repo"));

  Ok(())
}

#[test]
fn test_sync_bidirectional() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create and split a crate
  workspace.add_crate("my-crate", "0.1.0", &[])?;
  workspace.commit("Add my-crate")?;

  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let split_dir = workspace.path.join("split-repos/my-crate-split");
  let config = workspace.read_file("rail.toml")?;
  let updated_config = config.replace(r#"remote = """#, &format!(r#"remote = "{}""#, split_dir.display()));
  std::fs::write(workspace.path.join("rail.toml"), updated_config)?;

  run_cargo_rail(&workspace.path, &["rail", "split", "my-crate"])?;

  // Change in monorepo
  workspace.modify_file("my-crate", "README.md", "# Updated from mono")?;
  workspace.commit("Mono change")?;

  // Sync to remote
  run_cargo_rail(&workspace.path, &["rail", "sync", "my-crate", "--to-remote"])?;

  // Change in split repo
  std::fs::write(split_dir.join("src/lib.rs"), "// From split")?;
  git(&split_dir, &["add", "."])?;
  git(&split_dir, &["commit", "-m", "Split change"])?;

  // Sync from remote
  run_cargo_rail(&workspace.path, &["rail", "sync", "my-crate", "--from-remote"])?;

  // Verify both changes are present
  let readme = workspace.read_file("crates/my-crate/README.md")?;
  assert!(readme.contains("Updated from mono"));

  let lib = workspace.read_file("crates/my-crate/src/lib.rs")?;
  assert!(lib.contains("From split"));

  Ok(())
}

#[test]
fn test_sync_deduplicates_commits() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create and split a crate
  workspace.add_crate("my-crate", "0.1.0", &[])?;
  workspace.commit("Add my-crate")?;

  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let split_dir = workspace.path.join("split-repos/my-crate-split");
  let config = workspace.read_file("rail.toml")?;
  let updated_config = config.replace(r#"remote = """#, &format!(r#"remote = "{}""#, split_dir.display()));
  std::fs::write(workspace.path.join("rail.toml"), updated_config)?;

  run_cargo_rail(&workspace.path, &["rail", "split", "my-crate"])?;

  // Make a change in mono
  workspace.modify_file("my-crate", "src/lib.rs", "// Change 1")?;
  workspace.commit("Change 1")?;

  // Sync to remote
  run_cargo_rail(&workspace.path, &["rail", "sync", "my-crate", "--to-remote"])?;

  // Try syncing again (should be no-op)
  run_cargo_rail(&workspace.path, &["rail", "sync", "my-crate", "--to-remote"])?;

  // Count commits in split repo (should not have duplicates)
  let log = git(&split_dir, &["log", "--oneline"])?;
  let commit_count = String::from_utf8_lossy(&log.stdout).lines().count();

  // Should have exactly 2 commits (initial + change 1), no duplicates
  assert_eq!(commit_count, 2);

  Ok(())
}
