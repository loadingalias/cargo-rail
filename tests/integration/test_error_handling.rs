//! Integration tests for error handling across commands
//!
//! Tests that the CLI handles errors gracefully and provides useful feedback.

use super::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;
use std::fs;

// Invalid Git Reference Tests

/// Test plan with non-existent git ref
#[test]
fn test_plan_invalid_since_ref() -> Result<()> {
  let ws = TestWorkspace::new_named("error-invalid-ref")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  // Use a non-existent ref
  let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "nonexistent-branch-xyz"])?;

  // Should fail (non-zero exit code)
  assert!(!output.status.success(), "plan with invalid ref should fail");

  Ok(())
}

/// Test plan with invalid SHA pair
#[test]
fn test_plan_invalid_sha_pair() -> Result<()> {
  let ws = TestWorkspace::new_named("error-invalid-sha")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  // Use invalid SHAs
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "plan",
      "--from",
      "0000000000000000000000000000000000000000",
      "--to",
      "HEAD",
    ],
  )?;

  assert!(!output.status.success(), "plan with invalid SHA should fail");

  Ok(())
}

// Configuration Error Tests

/// Test commands fail gracefully with corrupted rail.toml
#[test]
fn test_corrupted_config_toml() -> Result<()> {
  let ws = TestWorkspace::new_named("error-corrupted-config")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  // Write invalid TOML
  fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[workspace
this is not valid toml { } [
"#,
  )?;

  // Commands that load config should fail gracefully
  let output = run_cargo_rail(&ws.path, &["rail", "status"])?;

  // Should fail due to invalid config
  assert!(!output.status.success(), "status with corrupted config should fail");

  Ok(())
}

/// Test unify with missing config falls back gracefully
#[test]
fn test_unify_no_config() -> Result<()> {
  let ws = TestWorkspace::new_named("error-no-config")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  // Remove the config
  ws.remove_config()?;
  ws.commit("Remove config")?;

  // Unify should still work (uses defaults)
  let output = run_cargo_rail(&ws.path, &["rail", "unify", "--check"])?;

  // Should succeed with default config
  assert!(
    output.status.success(),
    "unify should work without config. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

/// Test release with invalid crate name
#[test]
fn test_release_invalid_crate_name() -> Result<()> {
  let ws = TestWorkspace::new_named("error-invalid-crate")?;
  ws.add_crate("real-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  // Try to release non-existent crate
  let output = run_cargo_rail(&ws.path, &["rail", "release", "run", "--check", "does-not-exist"])?;
  assert!(!output.status.success(), "release with invalid crate should fail");

  Ok(())
}

// Split/Sync Error Tests

/// Test split with invalid crate name
#[test]
fn test_split_invalid_crate() -> Result<()> {
  let ws = TestWorkspace::new_named("error-split-invalid")?;
  ws.add_crate("real-crate", "0.1.0", &[])?;

  // Configure split for real crate using new format
  fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[workspace]
root = "."

[crates.real-crate.split]
remote = "/tmp/fake-remote"
branch = "main"
mode = "single"
paths = [{ crate = "crates/real-crate" }]
"#,
  )?;

  ws.commit("Add crate with split config")?;

  // Try to split non-existent crate
  let output = run_cargo_rail(&ws.path, &["rail", "split", "run", "nonexistent-crate"])?;
  assert!(!output.status.success(), "split with invalid crate should fail");

  Ok(())
}

/// Test sync with no splits configured
#[test]
fn test_sync_no_splits_configured() -> Result<()> {
  let ws = TestWorkspace::new_named("error-sync-no-splits")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;

  // Config without splits
  fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[workspace]
root = "."
"#,
  )?;

  ws.commit("Add crate without splits")?;

  // Try to sync
  let output = run_cargo_rail(&ws.path, &["rail", "sync", "--all"])?;
  assert!(!output.status.success(), "sync with no splits should fail");

  Ok(())
}

// Workspace Error Tests

/// Test running outside a cargo workspace
#[test]
fn test_not_a_workspace() -> Result<()> {
  // Create a temp dir that's NOT a cargo workspace
  let temp = tempfile::TempDir::new()?;
  let path = temp.path();

  // Just create an empty directory
  fs::create_dir_all(path)?;

  // Try to run a command
  let output = run_cargo_rail(path, &["rail", "status"])?;
  assert!(!output.status.success(), "running outside workspace should fail");

  Ok(())
}

/// Test running in a single crate (non-workspace)
#[test]
fn test_single_crate_workspace() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("single", "0.1.0")?;

  // Add a second commit so HEAD~1 exists
  fs::write(ws.path.join("src/lib.rs"), "// modified")?;
  super::helpers::git(&ws.path, &["add", "."])?;
  super::helpers::git(&ws.path, &["commit", "-m", "Add modification"])?;

  // Planner should work (single crate is technically a workspace)
  let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD~1"])?;

  // This should succeed for a single crate
  assert!(
    output.status.success(),
    "plan in single crate should work. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

// Test Command Error Tests

/// Test run command with invalid --since ref
#[test]
fn test_run_invalid_since() -> Result<()> {
  let ws = TestWorkspace::new_named("error-test-since")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  // Use invalid since ref
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "invalid-ref-xyz"])?;

  // Should fail with error
  assert!(!output.status.success(), "run with invalid ref should fail");

  Ok(())
}

// Init Error Tests

/// Test init refuses to overwrite existing config without --force
#[test]
fn test_init_no_overwrite_without_force() -> Result<()> {
  let ws = TestWorkspace::new_named("error-init-exists")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  // Config already exists from TestWorkspace::new_named

  // Try to init again without --force
  let output = run_cargo_rail(&ws.path, &["rail", "init", "--non-interactive"])?;
  assert!(
    !output.status.success(),
    "init without --force should fail when config exists"
  );

  Ok(())
}

// Unify Error Tests

/// Test unify undo when no backups exist
#[test]
fn test_unify_undo_no_backups() -> Result<()> {
  let ws = TestWorkspace::new_named("error-undo-empty")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  // Make sure no backups exist
  let backup_dir = ws.path.join("target/cargo-rail/backups");
  if backup_dir.exists() {
    fs::remove_dir_all(&backup_dir)?;
  }

  // Try to undo when no backups exist
  let output = run_cargo_rail(&ws.path, &["rail", "unify", "undo"])?;
  assert!(!output.status.success(), "undo with no backups should fail");

  Ok(())
}

// Path Handling Tests

/// Test handling of paths with special characters
#[test]
fn test_path_with_spaces() -> Result<()> {
  // Create workspace with space in path
  let temp = tempfile::TempDir::new()?;
  let path_with_space = temp.path().join("my workspace");
  fs::create_dir_all(&path_with_space)?;

  // Initialize git
  super::helpers::git(&path_with_space, &["init", "--initial-branch=main"])?;
  super::helpers::git(&path_with_space, &["config", "user.name", "Test"])?;
  super::helpers::git(&path_with_space, &["config", "user.email", "test@test.com"])?;

  // Create workspace Cargo.toml with actual member
  fs::write(
    path_with_space.join("Cargo.toml"),
    r#"[workspace]
members = ["crates/*"]
resolver = "2"

[workspace.package]
edition = "2021"
"#,
  )?;

  // Create a crate so workspace has actual members
  let crate_path = path_with_space.join("crates/test-crate");
  fs::create_dir_all(crate_path.join("src"))?;
  fs::write(
    crate_path.join("Cargo.toml"),
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition.workspace = true
"#,
  )?;
  fs::write(crate_path.join("src/lib.rs"), "// test")?;

  fs::create_dir_all(path_with_space.join(".config"))?;
  fs::write(
    path_with_space.join(".config/rail.toml"),
    r#"[workspace]
root = "."
"#,
  )?;

  super::helpers::git(&path_with_space, &["add", "."])?;
  super::helpers::git(&path_with_space, &["commit", "-m", "Initial"])?;

  // Should handle paths with spaces
  let output = run_cargo_rail(&path_with_space, &["rail", "plan", "--since", "HEAD"])?;

  // Should succeed
  assert!(
    output.status.success(),
    "Should handle paths with spaces. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}
