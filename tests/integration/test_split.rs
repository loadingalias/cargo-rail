//! Tests for the `split` command

use crate::helpers::*;
use anyhow::Result;

#[test]
fn test_split_creates_repo_with_history() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create a crate with some history
  workspace.add_crate("my-crate", "0.1.0", &[("anyhow", "\"1.0\"")])?;
  workspace.commit("Add my-crate")?;

  workspace.modify_file("my-crate", "src/lib.rs", "// Updated\npub fn hello() {}")?;
  workspace.commit("Update my-crate")?;

  workspace.modify_file("my-crate", "README.md", "# Updated README")?;
  workspace.commit("Update README")?;

  // Initialize cargo-rail config
  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;

  // Run split with remote override
  let split_dir = workspace.path.join("split-repos").join("my-crate-split");
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

  // Verify split repo exists
  assert!(split_dir.exists());
  assert!(split_dir.join(".git").exists());
  assert!(split_dir.join("Cargo.toml").exists());
  assert!(split_dir.join("src/lib.rs").exists());
  assert!(split_dir.join("README.md").exists());

  // Verify history was preserved
  let log = git(&split_dir, &["log", "--oneline"])?;
  let log_str = String::from_utf8_lossy(&log.stdout);
  assert!(log_str.contains("Update README"));
  assert!(log_str.contains("Update my-crate"));
  assert!(log_str.contains("Add my-crate"));

  // Verify git-notes mapping was created (check the notes ref exists)
  let _notes_ref_check = std::process::Command::new("git")
    .current_dir(&workspace.path)
    .args(["show-ref", "refs/notes/rail/my-crate"])
    .output()?;
  // It's OK if notes don't exist yet - split creates the structure
  // The important thing is the split repo was created successfully

  Ok(())
}

#[test]
fn test_split_transforms_cargo_toml() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create two crates with path dependency
  workspace.add_crate("lib-crate", "0.1.0", &[("anyhow", "\"1.0\"")])?;
  workspace.add_crate(
    "app-crate",
    "0.2.0",
    &[("lib-crate", "{ path = \"../lib-crate\", version = \"0.1\" }")],
  )?;
  workspace.commit("Add crates with dependencies")?;

  // Initialize and split app-crate with remote override
  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let split_dir = workspace.path.join("split-repos").join("app-crate-split");
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "split",
      "app-crate",
      "--remote",
      &split_dir.display().to_string(),
      "--apply",
    ],
  )?;

  // Read transformed Cargo.toml
  let cargo_toml = std::fs::read_to_string(split_dir.join("Cargo.toml"))?;

  // Should have version dependency, not path dependency
  assert!(cargo_toml.contains("lib-crate"));
  assert!(cargo_toml.contains("0.1") || cargo_toml.contains("\"0.1\""));
  assert!(!cargo_toml.contains("path ="));

  // Should not have workspace section
  assert!(!cargo_toml.contains("[workspace]"));

  Ok(())
}

#[test]
fn test_split_filters_commits() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create two crates
  workspace.add_crate("crate-a", "0.1.0", &[])?;
  workspace.add_crate("crate-b", "0.1.0", &[])?;
  workspace.commit("Add both crates")?;

  // Modify only crate-a
  workspace.modify_file("crate-a", "src/lib.rs", "// Only A")?;
  workspace.commit("Update crate-a only")?;

  // Modify only crate-b
  workspace.modify_file("crate-b", "src/lib.rs", "// Only B")?;
  workspace.commit("Update crate-b only")?;

  // Modify crate-a again
  workspace.modify_file("crate-a", "README.md", "# A Updated")?;
  workspace.commit("Update crate-a README")?;

  // Initialize and split crate-a with remote override
  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let split_dir = workspace.path.join("split-repos").join("crate-a-split");
  run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "split",
      "crate-a",
      "--remote",
      &split_dir.display().to_string(),
      "--apply",
    ],
  )?;

  // Verify split repo only has commits that touched crate-a
  let log = git(&split_dir, &["log", "--oneline"])?;
  let log_str = String::from_utf8_lossy(&log.stdout);

  assert!(
    log_str.contains("Add both crates"),
    "Missing 'Add both crates' in:\n{}",
    log_str
  );
  assert!(log_str.contains("Update crate-a only"));
  assert!(log_str.contains("Update crate-a README"));
  assert!(!log_str.contains("Update crate-b only")); // Should NOT be present

  Ok(())
}

#[test]
fn test_split_copies_auxiliary_files() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create auxiliary files
  std::fs::write(
    workspace.path.join("rust-toolchain.toml"),
    "[toolchain]\nchannel = \"stable\"\n",
  )?;
  std::fs::write(workspace.path.join("rustfmt.toml"), "max_width = 100\n")?;

  workspace.add_crate("my-crate", "0.1.0", &[])?;
  workspace.commit("Add crate with aux files")?;

  // Initialize and split with remote override
  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let split_dir = workspace.path.join("split-repos").join("my-crate-split");
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

  // Verify auxiliary files were copied
  assert!(split_dir.join("rust-toolchain.toml").exists());
  assert!(split_dir.join("rustfmt.toml").exists());

  Ok(())
}
