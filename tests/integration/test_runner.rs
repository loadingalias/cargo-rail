//! Integration tests for `cargo rail test` command
//!
//! Tests the smart test runner with change detection

use crate::helpers::{TestWorkspace, git, run_cargo_rail};
use anyhow::Result;

#[test]
fn test_runner_basic_change_detection() -> Result<()> {
  // Setup workspace with two crates
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.commit("Add lib-a and lib-b")?;

  // Create baseline
  git(&ws.path, &["branch", "baseline"])?;

  // Modify lib-a source
  ws.modify_file("lib-a", "src/lib.rs", "pub fn modified() -> u32 { 42 }")?;
  ws.commit("Modify lib-a")?;

  // Run test with change detection
  let output = run_cargo_rail(&ws.path, &["rail", "test", "--since", "baseline"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "Test command should succeed");
  assert!(stdout.contains("Running tests for"), "Should invoke runner");
  assert!(
    stdout.contains("lib-a") && stdout.contains("lib-b"),
    "Should include dependent crates"
  );

  Ok(())
}

#[test]
fn test_runner_no_changes() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  // Create baseline
  git(&ws.path, &["branch", "baseline"])?;

  // Run test with no changes
  let output = run_cargo_rail(&ws.path, &["rail", "test", "--since", "baseline"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should skip all tests
  assert!(
    stdout.contains("No affected crates") || stdout.contains("all tests skipped"),
    "Should skip tests when no changes. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_docs_only_change() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Modify only README
  ws.modify_file("lib-a", "README.md", "# Updated Documentation\n")?;
  ws.commit("Update README")?;

  // Run test
  let output = run_cargo_rail(&ws.path, &["rail", "test", "--since", "baseline"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Documentation-only changes might still trigger tests depending on implementation
  // The key is that it should be detected and handled appropriately
  assert!(
    output.status.success(),
    "Test command should succeed. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_transitive_dependencies() -> Result<()> {
  // Setup: lib-a <- lib-b <- lib-c (chain)
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.add_crate("lib-c", "0.1.0", &[("lib-b", r#"{ path = "../lib-b" }"#)])?;
  ws.commit("Add dependency chain")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Modify lib-a (root of chain)
  ws.modify_file("lib-a", "src/lib.rs", "pub fn chain_changed() {}")?;
  ws.commit("Modify lib-a")?;

  // Run test
  let output = run_cargo_rail(&ws.path, &["rail", "test", "--since", "baseline"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // All three should be tested (lib-a changed, lib-b and lib-c depend on it)
  assert!(
    stdout.contains("lib-a"),
    "Should test lib-a (directly changed). Output:\n{}",
    stdout
  );
  assert!(
    stdout.contains("lib-b"),
    "Should test lib-b (depends on lib-a). Output:\n{}",
    stdout
  );
  assert!(
    stdout.contains("lib-c"),
    "Should test lib-c (transitive dependent). Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_isolated_change() -> Result<()> {
  // Setup: two independent crates
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add independent crates")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Modify only lib-a
  ws.modify_file("lib-a", "src/lib.rs", "pub fn isolated_change() {}")?;
  ws.commit("Modify lib-a only")?;

  // Run test
  let output = run_cargo_rail(&ws.path, &["rail", "test", "--since", "baseline"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should test only lib-a, not lib-b
  assert!(
    stdout.contains("lib-a"),
    "Should test lib-a (changed). Output:\n{}",
    stdout
  );
  assert!(
    !stdout.contains("• lib-b"),
    "Should NOT list lib-b as affected. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_with_explain() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  git(&ws.path, &["branch", "baseline"])?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn explained() {}")?;
  ws.commit("Modify lib-a")?;

  // Run with --explain flag
  let output = run_cargo_rail(&ws.path, &["rail", "test", "--since", "baseline", "--explain"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show detailed explanation
  assert!(
    stdout.contains("Change Impact Analysis") || stdout.contains("File Breakdown"),
    "Should show detailed explanation with --explain. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_auto_detect_base_ref() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  // Create a base branch (if main doesn't exist, create it; otherwise use it)
  let _ = git(&ws.path, &["branch", "base-branch"]);
  git(&ws.path, &["checkout", "-b", "feature-branch"])?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn feature_work() {}")?;
  ws.commit("Feature work")?;

  // Run without --since (should auto-detect base ref or use HEAD)
  let output = run_cargo_rail(&ws.path, &["rail", "test"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should successfully run (whether it detects changes or not is okay)
  assert!(
    output.status.success(),
    "Should successfully handle auto-detect. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_config_file_changes() -> Result<()> {
  // Test that Cargo.toml changes are detected
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Modify Cargo.toml (add a comment or metadata)
  let cargo_toml = std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?;
  std::fs::write(
    ws.path.join("crates/lib-a/Cargo.toml"),
    format!("# Modified\n{}", cargo_toml),
  )?;
  ws.commit("Modify Cargo.toml")?;

  let output = run_cargo_rail(&ws.path, &["rail", "test", "--since", "baseline"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Config changes should trigger testing
  assert!(
    stdout.contains("lib-a"),
    "Cargo.toml changes should trigger testing. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_test_file_changes() -> Result<()> {
  // Test that test file changes are detected
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Add an integration test
  std::fs::create_dir_all(ws.path.join("crates/lib-a/tests"))?;
  std::fs::write(
    ws.path.join("crates/lib-a/tests/integration_test.rs"),
    "#[test]\nfn new_test() { assert!(true); }",
  )?;
  ws.commit("Add integration test")?;

  let output = run_cargo_rail(&ws.path, &["rail", "test", "--since", "baseline"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Test file changes should trigger testing
  assert!(
    stdout.contains("lib-a"),
    "Test file changes should trigger testing. Output:\n{}",
    stdout
  );

  Ok(())
}
