//! Integration tests for `cargo rail graph affected` command

use crate::helpers::{TestWorkspace, git, run_cargo_rail};
use anyhow::Result;

#[test]
fn test_affected_basic() -> Result<()> {
  // Setup workspace with two crates
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.commit("Add lib-a and lib-b")?;

  // Create a baseline (origin/main)
  git(&ws.path, &["branch", "origin/main"])?;

  // Modify lib-a
  ws.modify_file("lib-a", "src/lib.rs", "pub fn hello() -> &'static str { \"Modified\" }")?;
  ws.commit("Modify lib-a")?;

  // Run affected command
  let output = run_cargo_rail(&ws.path, &["rail", "graph", "affected", "--since", "origin/main"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show lib-a as directly affected and lib-b as dependent
  assert!(stdout.contains("lib-a"), "lib-a should be affected");
  assert!(stdout.contains("lib-b"), "lib-b should be in dependents");

  Ok(())
}

#[test]
fn test_affected_no_changes() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  // Create baseline
  git(&ws.path, &["branch", "origin/main"])?;

  // Run affected with no changes
  let output = run_cargo_rail(&ws.path, &["rail", "graph", "affected", "--since", "origin/main"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should indicate no changes
  assert!(
    stdout.contains("Changed files: 0") || stdout.contains("Direct impact: 0"),
    "Should indicate no changes, got: {}",
    stdout
  );

  Ok(())
}

#[test]
fn test_affected_json_output() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  git(&ws.path, &["branch", "origin/main"])?;

  // Modify lib-a
  ws.modify_file("lib-a", "README.md", "# Modified\n")?;
  ws.commit("Modify lib-a README")?;

  // Run with --format json
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "graph",
      "affected",
      "--since",
      "origin/main",
      "--format",
      "json",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should be valid JSON
  let json: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
  assert!(json.is_object(), "Output should be JSON object");

  Ok(())
}

#[test]
fn test_affected_names_only() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.commit("Add crates")?;

  git(&ws.path, &["branch", "origin/main"])?;

  // Modify lib-a
  ws.modify_file("lib-a", "src/lib.rs", "pub fn hello() -> &'static str { \"Changed\" }")?;
  ws.commit("Change lib-a")?;

  // Run with --format names
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "graph",
      "affected",
      "--since",
      "origin/main",
      "--format",
      "names",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should be simple list of names
  let lines: Vec<&str> = stdout.trim().lines().collect();
  assert!(lines.contains(&"lib-a"), "Should contain lib-a");
  assert!(lines.contains(&"lib-b"), "Should contain lib-b");

  Ok(())
}

#[test]
fn test_affected_sha_pair_mode() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  let sha1 = ws.commit("Add lib-a")?;

  // Make a change
  ws.modify_file("lib-a", "README.md", "# Updated\n")?;
  let sha2 = ws.commit("Update lib-a")?;

  // Run with --from/--to
  let output = run_cargo_rail(&ws.path, &["rail", "graph", "affected", "--from", &sha1, "--to", &sha2])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(stdout.contains("lib-a"), "lib-a should be affected");

  Ok(())
}
