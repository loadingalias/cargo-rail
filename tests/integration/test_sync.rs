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
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "split",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--apply",
    ],
  )?;

  // Make changes in monorepo
  workspace.modify_file("my-crate", "src/lib.rs", "// Monorepo change\npub fn new() {}")?;
  workspace.commit("Update in monorepo")?;

  // Sync to remote
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "sync",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--to-remote",
      "--apply",
    ],
  )?;

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
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "split",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--apply",
    ],
  )?;

  // Make changes in split repo
  std::fs::write(
    split_dir.join("src/lib.rs"),
    "// Split repo change\npub fn from_split() {}",
  )?;
  git(&split_dir, &["add", "."])?;
  git(&split_dir, &["commit", "-m", "Update in split repo"])?;

  // Sync from remote
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "sync",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--from-remote",
      "--apply",
    ],
  )?;

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
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "split",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--apply",
    ],
  )?;

  // Change in monorepo
  workspace.modify_file("my-crate", "README.md", "# Updated from mono")?;
  workspace.commit("Mono change")?;

  // Sync to remote
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "sync",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--to-remote",
      "--apply",
    ],
  )?;

  // Change in split repo
  std::fs::write(split_dir.join("src/lib.rs"), "// From split")?;
  git(&split_dir, &["add", "."])?;
  git(&split_dir, &["commit", "-m", "Split change"])?;

  // Sync from remote
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "sync",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--from-remote",
      "--apply",
    ],
  )?;

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
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "split",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--apply",
    ],
  )?;

  // Make a change in mono
  workspace.modify_file("my-crate", "src/lib.rs", "// Change 1")?;
  workspace.commit("Change 1")?;

  // Sync to remote
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "sync",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--to-remote",
      "--apply",
    ],
  )?;

  // Try syncing again (should be no-op)
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "sync",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--to-remote",
      "--apply",
    ],
  )?;

  // Count commits in split repo (should not have duplicates)
  let log = git(&split_dir, &["log", "--oneline"])?;
  let commit_count = String::from_utf8_lossy(&log.stdout).lines().count();

  // Should have exactly 2 commits (initial + change 1), no duplicates
  assert_eq!(commit_count, 2);

  Ok(())
}

#[test]
fn test_conflict_resolution_ours_strategy() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create and split a crate
  workspace.add_crate("my-crate", "0.1.0", &[])?;
  workspace.commit("Add my-crate")?;

  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let split_dir = workspace.path.join("split-repos/my-crate-split");

  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "split",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--apply",
    ],
  )?;

  // Make change in monorepo
  workspace.modify_file("my-crate", "src/lib.rs", "// Monorepo version\npub fn mono() {}")?;
  workspace.commit("Monorepo change")?;

  // Sync to remote first (establish baseline)
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "sync",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--to-remote",
      "--apply",
    ],
  )?;

  // Make another change in monorepo
  workspace.modify_file("my-crate", "src/lib.rs", "// Monorepo version v2\npub fn mono() {}")?;
  workspace.commit("Monorepo change v2")?;

  // Make conflicting change in split repo (same file)
  std::fs::write(split_dir.join("src/lib.rs"), "// Split version\npub fn split() {}")?;
  git(&split_dir, &["add", "."])?;
  git(&split_dir, &["commit", "-m", "Split change"])?;

  // Sync from remote with --strategy=ours (should keep monorepo version)
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "sync",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--from-remote",
      "--strategy=ours",
      "--no-protected-branches",
      "--apply",
    ],
  )?;

  // Verify monorepo version is kept
  let lib = workspace.read_file("crates/my-crate/src/lib.rs")?;
  assert!(
    lib.contains("Monorepo version v2"),
    "Expected 'Monorepo version v2' in:\n{}",
    lib
  );
  assert!(lib.contains("pub fn mono()"));
  assert!(!lib.contains("Split version"));
  assert!(!lib.contains("pub fn split()"));

  Ok(())
}

#[test]
fn test_conflict_resolution_theirs_strategy() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create and split a crate
  workspace.add_crate("my-crate", "0.1.0", &[])?;
  workspace.commit("Add my-crate")?;

  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let split_dir = workspace.path.join("split-repos/my-crate-split");

  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "split",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--apply",
    ],
  )?;

  // Make change in monorepo
  workspace.modify_file("my-crate", "src/lib.rs", "// Monorepo version\npub fn mono() {}")?;
  workspace.commit("Monorepo change")?;

  // Sync to remote first
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "sync",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--to-remote",
      "--apply",
    ],
  )?;

  // Make another change in monorepo
  workspace.modify_file("my-crate", "src/lib.rs", "// Monorepo version v2\npub fn mono() {}")?;
  workspace.commit("Monorepo change v2")?;

  // Make conflicting change in split repo
  std::fs::write(split_dir.join("src/lib.rs"), "// Split version\npub fn split() {}")?;
  git(&split_dir, &["add", "."])?;
  git(&split_dir, &["commit", "-m", "Split change"])?;

  // Sync from remote with --strategy=theirs (should use remote version)
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "sync",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--from-remote",
      "--strategy=theirs",
      "--no-protected-branches",
      "--apply",
    ],
  )?;

  // Verify remote version is used
  let lib = workspace.read_file("crates/my-crate/src/lib.rs")?;
  assert!(lib.contains("Split version"));
  assert!(lib.contains("pub fn split()"));
  assert!(!lib.contains("Monorepo version"));
  assert!(!lib.contains("pub fn mono()"));

  Ok(())
}

#[test]
fn test_conflict_resolution_manual_strategy() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create and split a crate
  workspace.add_crate("my-crate", "0.1.0", &[])?;
  workspace.commit("Add my-crate")?;

  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let split_dir = workspace.path.join("split-repos/my-crate-split");

  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "split",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--apply",
    ],
  )?;

  // Make change in monorepo
  workspace.modify_file("my-crate", "src/lib.rs", "// Monorepo version\npub fn mono() {}")?;
  workspace.commit("Monorepo change")?;

  // Sync to remote first
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "sync",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--to-remote",
      "--apply",
    ],
  )?;

  // Make another change in monorepo
  workspace.modify_file("my-crate", "src/lib.rs", "// Monorepo version v2\npub fn mono() {}")?;
  workspace.commit("Monorepo change v2")?;

  // Make conflicting change in split repo
  std::fs::write(split_dir.join("src/lib.rs"), "// Split version\npub fn split() {}")?;
  git(&split_dir, &["add", "."])?;
  git(&split_dir, &["commit", "-m", "Split change"])?;

  // Sync from remote with --strategy=manual (should create conflict markers)
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "sync",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--from-remote",
      "--strategy=manual",
      "--no-protected-branches",
      "--apply",
    ],
  )?;

  // Verify conflict markers are present
  let lib = workspace.read_file("crates/my-crate/src/lib.rs")?;
  assert!(lib.contains("<<<<<<<"));
  assert!(lib.contains("======="));
  assert!(lib.contains(">>>>>>>"));
  // Should contain both versions in the conflict markers
  assert!(lib.contains("Monorepo version v2") || lib.contains("Split version"));

  Ok(())
}

#[test]
fn test_conflict_resolution_union_strategy() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create and split a crate
  workspace.add_crate("my-crate", "0.1.0", &[])?;
  workspace.commit("Add my-crate")?;

  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let split_dir = workspace.path.join("split-repos/my-crate-split");

  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "split",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--apply",
    ],
  )?;

  // Make change in monorepo
  workspace.modify_file("my-crate", "src/lib.rs", "// Monorepo version\npub fn mono() {}")?;
  workspace.commit("Monorepo change")?;

  // Sync to remote first
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "sync",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--to-remote",
      "--apply",
    ],
  )?;

  // Make another change in monorepo
  workspace.modify_file("my-crate", "src/lib.rs", "// Monorepo version v2\npub fn mono() {}")?;
  workspace.commit("Monorepo change v2")?;

  // Make conflicting change in split repo
  std::fs::write(split_dir.join("src/lib.rs"), "// Split version\npub fn split() {}")?;
  git(&split_dir, &["add", "."])?;
  git(&split_dir, &["commit", "-m", "Split change"])?;

  // Sync from remote with --strategy=union (should combine both)
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "sync",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--from-remote",
      "--strategy=union",
      "--no-protected-branches",
      "--apply",
    ],
  )?;

  // Verify both versions are present (union merge combines them)
  let lib = workspace.read_file("crates/my-crate/src/lib.rs")?;
  // Union merge should contain elements from both sides
  let has_mono_content = lib.contains("pub fn mono()") || lib.contains("Monorepo");
  let has_split_content = lib.contains("pub fn split()") || lib.contains("Split");
  assert!(
    has_mono_content && has_split_content,
    "Union merge should contain both versions. File content:\n{}",
    lib
  );

  Ok(())
}

#[test]
fn test_no_conflict_with_non_overlapping_changes() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create and split a crate
  workspace.add_crate("my-crate", "0.1.0", &[])?;
  workspace.commit("Add my-crate")?;

  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let split_dir = workspace.path.join("split-repos/my-crate-split");
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "split",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--apply",
    ],
  )?;

  // Make change in monorepo (modify README)
  workspace.modify_file("my-crate", "README.md", "# Updated from mono")?;
  workspace.commit("Mono change")?;

  // Make non-conflicting change in split repo (modify lib.rs)
  std::fs::write(split_dir.join("src/lib.rs"), "// New function\npub fn new_fn() {}")?;
  git(&split_dir, &["add", "."])?;
  git(&split_dir, &["commit", "-m", "Add new function"])?;

  // Sync from remote (should auto-merge cleanly)
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "sync",
      "my-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--from-remote",
      "--apply",
    ],
  )?;

  // Verify both changes are present
  let readme = workspace.read_file("crates/my-crate/README.md")?;
  assert!(readme.contains("Updated from mono"));

  let lib = workspace.read_file("crates/my-crate/src/lib.rs")?;
  assert!(lib.contains("New function"));
  assert!(lib.contains("pub fn new_fn()"));

  // Should not have conflict markers
  assert!(!lib.contains("<<<<<<<"));
  assert!(!lib.contains(">>>>>>>"));

  Ok(())
}
