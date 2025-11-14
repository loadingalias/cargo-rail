//! Integration tests for `cargo rail release` commands

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;

#[test]
fn test_release_plan_basic() -> Result<()> {
  // Setup workspace with a crate
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("feat: Add lib-a")?;

  // Create rail.toml with release config
  let rail_toml = r#"
[workspace]
root = "."

[[releases]]
name = "lib-a"
crate = "crates/lib-a"
last_version = "0.1.0"
last_sha = "HEAD~1"
last_date = "2024-01-01T00:00:00Z"
"#;
  std::fs::write(ws.path.join("rail.toml"), rail_toml)?;

  // Make some changes
  ws.modify_file("lib-a", "src/lib.rs", "pub fn new_feature() {}")?;
  ws.commit("feat: Add new feature")?;

  // Run release plan
  let output = run_cargo_rail(&ws.path, &["rail", "release", "plan", "lib-a"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show version bump suggestion
  assert!(
    stdout.contains("version") || stdout.contains("bump") || stdout.contains("0."),
    "Should suggest version bump"
  );

  Ok(())
}

#[test]
fn test_release_plan_json_output() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Initial commit")?;

  // Create rail.toml
  let rail_toml = r#"
[workspace]
root = "."

[[releases]]
name = "lib-a"
crate = "crates/lib-a"
last_version = "0.1.0"
last_sha = "HEAD"
last_date = "2024-01-01T00:00:00Z"
"#;
  std::fs::write(ws.path.join("rail.toml"), rail_toml)?;

  // Run with --json
  let output = run_cargo_rail(&ws.path, &["rail", "release", "plan", "lib-a", "--json"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should be valid JSON
  let json: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
  assert!(json.is_object() || json.is_array(), "Output should be JSON");

  Ok(())
}

#[test]
fn test_release_plan_all() -> Result<()> {
  // Setup workspace with multiple crates
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add crates")?;

  // Create rail.toml with multiple releases
  let rail_toml = r#"
[workspace]
root = "."

[[releases]]
name = "lib-a"
crate = "crates/lib-a"
last_version = "0.1.0"
last_sha = "HEAD"
last_date = "2024-01-01T00:00:00Z"

[[releases]]
name = "lib-b"
crate = "crates/lib-b"
last_version = "0.1.0"
last_sha = "HEAD"
last_date = "2024-01-01T00:00:00Z"
"#;
  std::fs::write(ws.path.join("rail.toml"), rail_toml)?;

  // Run with --all
  let output = run_cargo_rail(&ws.path, &["rail", "release", "plan", "--all"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show plans for all releases
  assert!(
    stdout.contains("lib-a") || stdout.contains("lib-b") || stdout.contains("release"),
    "Should show release plans"
  );

  Ok(())
}

#[test]
fn test_release_apply_dry_run() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  let initial_sha = ws.commit("feat: Initial release")?;

  // Create rail.toml
  let rail_toml = format!(
    r#"
[workspace]
root = "."

[[releases]]
name = "lib-a"
crate = "crates/lib-a"
last_version = "0.1.0"
last_sha = "{}"
last_date = "2024-01-01T00:00:00Z"
"#,
    initial_sha
  );
  std::fs::write(ws.path.join("rail.toml"), rail_toml)?;

  // Make a change
  ws.modify_file("lib-a", "src/lib.rs", "pub fn new_fn() {}")?;
  ws.commit("feat: Add new function")?;

  // Run apply with --dry-run
  let output = run_cargo_rail(&ws.path, &["rail", "release", "apply", "lib-a", "--dry-run"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show what would happen
  assert!(
    stdout.contains("Dry-run") || stdout.contains("dry") || stdout.contains("would") || stdout.contains("version"),
    "Should show dry-run plan, got: {}",
    stdout
  );

  // Verify nothing actually changed
  let cargo_toml = ws.read_file("crates/lib-a/Cargo.toml")?;
  assert!(cargo_toml.contains("0.1.0"), "Version should not have changed");

  Ok(())
}

#[test]
fn test_release_detects_conventional_commits() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  let initial_sha = ws.commit("Initial commit")?;

  // Create rail.toml
  let rail_toml = format!(
    r#"
[workspace]
root = "."

[[releases]]
name = "lib-a"
crate = "crates/lib-a"
last_version = "0.1.0"
last_sha = "{}"
last_date = "2024-01-01T00:00:00Z"
"#,
    initial_sha
  );
  std::fs::write(ws.path.join("rail.toml"), rail_toml)?;

  // Make commits with conventional format
  ws.modify_file("lib-a", "src/lib.rs", "pub fn fix() {}")?;
  ws.commit("fix: Fix a bug")?;

  ws.modify_file("lib-a", "README.md", "# Updated")?;
  ws.commit("feat: Add feature")?;

  // Run release plan
  let output = run_cargo_rail(&ws.path, &["rail", "release", "plan", "lib-a"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should detect conventional commits and suggest appropriate bump
  assert!(
    stdout.contains("feat") || stdout.contains("fix") || stdout.contains("minor") || stdout.contains("0.2"),
    "Should detect conventional commits"
  );

  Ok(())
}
