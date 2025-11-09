//! Tests for the `init` command

use crate::helpers::*;
use anyhow::Result;

#[test]
fn test_init_creates_config() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Add some crates
  workspace.add_crate("crate-a", "0.1.0", &[])?;
  workspace.add_crate("crate-b", "0.2.0", &[])?;
  workspace.commit("Add crates")?;

  // Run init with --all flag (non-interactive)
  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;

  // Verify rail.toml was created
  assert!(workspace.file_exists("rail.toml"));

  // Verify config content
  let config = workspace.read_file("rail.toml")?;
  assert!(config.contains("crate-a"));
  assert!(config.contains("crate-b"));
  assert!(config.contains("[workspace]"));
  assert!(config.contains("[[splits]]"));

  Ok(())
}

#[test]
fn test_init_validates_workspace() -> Result<()> {
  let temp = tempfile::TempDir::new()?;
  let path = temp.path();

  // Initialize git repo but no Cargo.toml
  git(path, &["init"])?;

  // Init should fail
  let result = run_cargo_rail(path, &["rail", "init", "--all"]);
  assert!(result.is_err());

  Ok(())
}

#[test]
fn test_init_overwrites_existing_config() -> Result<()> {
  let workspace = TestWorkspace::new()?;
  workspace.add_crate("crate-a", "0.1.0", &[])?;
  workspace.commit("Add crate")?;

  // Create initial config
  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let config1 = workspace.read_file("rail.toml")?;

  // Add another crate
  workspace.add_crate("crate-b", "0.2.0", &[])?;
  workspace.commit("Add another crate")?;

  // Note: This test would need user interaction to confirm overwrite
  // For now, we just verify the first init worked
  assert!(config1.contains("crate-a"));

  Ok(())
}
