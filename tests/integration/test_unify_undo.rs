//! Integration tests for cargo rail unify undo command

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;

#[test]
fn test_unify_undo_restores_latest_backup() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Add some crates
  workspace.add_crate("crate-a", "0.1.0", &[("serde", r#""1.0""#)])?;
  workspace.add_crate("crate-b", "0.1.0", &[("serde", r#""1.0""#)])?;
  workspace.commit("Add test crates")?;

  // Create initial state
  let original_workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;

  // Run unify with --backup flag to create a backup
  let apply_output = run_cargo_rail(&workspace.path, &["rail", "unify", "--backup"])?;
  assert!(apply_output.status.success(), "Unify apply should succeed");

  // Verify files were modified
  let modified_workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert_ne!(
    original_workspace_toml, modified_workspace_toml,
    "Workspace Cargo.toml should be modified"
  );

  // Run undo
  let undo_output = run_cargo_rail(&workspace.path, &["rail", "unify", "undo"])?;
  let undo_stdout = String::from_utf8_lossy(&undo_output.stdout);

  assert!(
    undo_output.status.success(),
    "Undo should succeed.\nOutput:\n{}",
    undo_stdout
  );
  assert!(
    undo_stdout.contains("Restoring") || undo_stdout.contains("restored"),
    "Should mention restoration in output"
  );

  // Verify files were restored
  let restored_workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert_eq!(
    original_workspace_toml, restored_workspace_toml,
    "Workspace Cargo.toml should be restored to original"
  );

  Ok(())
}

#[test]
fn test_unify_undo_list() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Add some crates
  workspace.add_crate("crate-a", "0.1.0", &[("serde", r#""1.0""#)])?;
  workspace.add_crate("crate-b", "0.1.0", &[("serde", r#""1.0""#)])?;
  workspace.commit("Add test crates")?;

  // Run unify with --backup flag to create a backup
  let apply_output = run_cargo_rail(&workspace.path, &["rail", "unify", "--backup"])?;
  assert!(apply_output.status.success());

  // Run undo --list
  let list_output = run_cargo_rail(&workspace.path, &["rail", "unify", "undo", "--list"])?;
  let list_stdout = String::from_utf8_lossy(&list_output.stdout);

  assert!(list_output.status.success(), "Undo --list should succeed");
  assert!(
    list_stdout.contains("Available backups") || list_stdout.contains("backup"),
    "Should list backups.\nOutput:\n{}",
    list_stdout
  );
  assert!(list_stdout.contains("latest"), "Should mark latest backup");
  assert!(list_stdout.contains("cargo rail unify"), "Should show command");

  Ok(())
}

#[test]
fn test_unify_undo_no_backups() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Add some crates
  workspace.add_crate("crate-a", "0.1.0", &[("serde", r#""1.0""#)])?;
  workspace.commit("Add test crates")?;

  // Run undo without any backups - should fail
  let undo_output = run_cargo_rail(&workspace.path, &["rail", "unify", "undo"])?;
  let undo_stderr = String::from_utf8_lossy(&undo_output.stderr);

  assert!(!undo_output.status.success(), "Should fail when no backups exist");
  assert!(
    undo_stderr.contains("No backups") || undo_stderr.contains("no backups"),
    "Should mention no backups found.\nStderr:\n{}",
    undo_stderr
  );

  Ok(())
}

#[test]
fn test_unify_undo_specific_backup_id() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Add some crates
  workspace.add_crate("crate-a", "0.1.0", &[("serde", r#""1.0""#)])?;
  workspace.add_crate("crate-b", "0.1.0", &[("serde", r#""1.0""#)])?;
  workspace.commit("Add test crates")?;

  // Save the original content before any modifications
  let original_content = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;

  // Create two backups by running unify twice
  run_cargo_rail(&workspace.path, &["rail", "unify", "--backup"])?;
  std::thread::sleep(std::time::Duration::from_secs(1)); // Ensure different timestamp
  run_cargo_rail(&workspace.path, &["rail", "unify", "--backup"])?;

  // Get list of backups
  let list_output = run_cargo_rail(&workspace.path, &["rail", "unify", "undo", "--list"])?;
  let list_stdout = String::from_utf8_lossy(&list_output.stdout);

  // Extract the first (latest) backup ID from the output
  // Format is: "1. 2024-01-15-143022 (latest)"
  let backup_id = list_stdout
    .lines()
    .find(|line| line.contains("(latest)"))
    .and_then(|line| line.split_whitespace().nth(1))
    .expect("Should find latest backup ID");

  // Restore specific backup
  let undo_output = run_cargo_rail(&workspace.path, &["rail", "unify", "undo", "--backup-id", backup_id])?;
  assert!(undo_output.status.success(), "Should restore specific backup");

  // Verify restoration
  let restored_content = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert_eq!(
    original_content, restored_content,
    "Should restore to the specified backup"
  );

  Ok(())
}
