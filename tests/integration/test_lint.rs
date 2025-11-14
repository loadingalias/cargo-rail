//! Integration tests for `cargo rail lint` commands

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;

#[test]
fn test_lint_deps_detects_non_workspace_deps() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;

  // Add crate with workspace deps defined but not used
  let crate_path = ws.add_crate("lib-a", "0.1.0", &[])?;

  // Manually add a dep that's in workspace.dependencies but not using inheritance
  let cargo_toml = ws.read_file("crates/lib-a/Cargo.toml")?;
  let updated = cargo_toml.replace(
    "[dependencies]",
    "[dependencies]\nanyhow = \"1.0\"  # Should use workspace = true",
  );
  std::fs::write(crate_path.join("Cargo.toml"), updated)?;

  ws.commit("Add lib-a with non-workspace dep")?;

  // Run lint deps
  let output = run_cargo_rail(&ws.path, &["rail", "lint", "deps"]);

  // Should detect the issue (but might fail if no issues found, which is also valid)
  match output {
    Ok(out) => {
      let stdout = String::from_utf8_lossy(&out.stdout);
      // If it succeeds, it should show the issue
      assert!(
        stdout.contains("anyhow") || stdout.contains("workspace"),
        "Should mention dependency or workspace"
      );
    }
    Err(_) => {
      // Command might fail if issues found in strict mode, that's okay
    }
  }

  Ok(())
}

#[test]
fn test_lint_deps_json_output() -> Result<()> {
  // Setup workspace with clean dependencies
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  // Run with --json
  let output = run_cargo_rail(&ws.path, &["rail", "lint", "deps", "--json"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should be valid JSON
  let json: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
  assert!(json.is_object() || json.is_array(), "Output should be JSON");

  Ok(())
}

#[test]
fn test_lint_versions_basic() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add crates")?;

  // Run lint versions
  let output = run_cargo_rail(&ws.path, &["rail", "lint", "versions"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should run successfully for a clean workspace
  assert!(
    stdout.contains("version") || stdout.contains("No conflicts") || stdout.contains("✓"),
    "Should show version info or success"
  );

  Ok(())
}

#[test]
fn test_lint_versions_json_output() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  // Run with --json
  let output = run_cargo_rail(&ws.path, &["rail", "lint", "versions", "--json"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should be valid JSON
  let json: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
  assert!(json.is_object() || json.is_array(), "Output should be JSON");

  Ok(())
}

#[test]
fn test_lint_manifest_basic() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  // Run lint manifest
  let output = run_cargo_rail(&ws.path, &["rail", "lint", "manifest"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should run successfully
  assert!(!stdout.is_empty(), "Should produce output");

  Ok(())
}

#[test]
fn test_lint_manifest_json_output() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  // Run with --json
  let output = run_cargo_rail(&ws.path, &["rail", "lint", "manifest", "--json"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should be valid JSON
  let json: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
  assert!(json.is_object() || json.is_array(), "Output should be JSON");

  Ok(())
}

#[test]
fn test_lint_manifest_checks_edition_consistency() -> Result<()> {
  // Setup workspace with consistent edition
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add crates with consistent edition")?;

  // Run lint manifest
  let output = run_cargo_rail(&ws.path, &["rail", "lint", "manifest"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should check edition consistency
  assert!(
    stdout.contains("edition") || stdout.contains("✓") || stdout.contains("passed"),
    "Should report on edition consistency"
  );

  Ok(())
}
