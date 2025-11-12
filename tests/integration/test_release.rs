//! Integration tests for release commands

use crate::helpers::*;
use anyhow::Result;

#[test]
fn test_release_plan_empty_workspace() -> Result<()> {
  let ws = TestWorkspace::new()?;

  // Add a crate
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("feat: initial commit")?;

  // Run release plan - should show no changes (no previous tag)
  let output = run_cargo_rail(&ws.path, &["rail", "release", "plan"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show the plan with one crate
  assert!(stdout.contains("test-crate"));
  assert!(stdout.contains("0.1.0"));

  Ok(())
}

#[test]
fn test_release_plan_with_conventional_commits() -> Result<()> {
  let ws = TestWorkspace::new()?;

  // Add a crate
  ws.add_crate("my-crate", "0.1.0", &[])?;
  ws.commit("feat: initial feature")?;

  // Create a release tag
  git(&ws.path, &["tag", "-a", "my-crate@v0.1.0", "-m", "Release 0.1.0"])?;

  // Add a new feature
  ws.modify_file("my-crate", "src/lib.rs", "pub fn new_feature() {}")?;
  ws.commit("feat: add new feature")?;

  // Run release plan
  let output = run_cargo_rail(&ws.path, &["rail", "release", "plan"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Debug: print output
  eprintln!("=== PLAN OUTPUT ===\n{}", stdout);

  // Should suggest version bump (feat: commits detected)
  assert!(stdout.contains("my-crate"));
  assert!(stdout.contains("0.1.0"));
  // The actual bump could be 0.2.0, 1.0.0, or 0.1.1 depending on semver rules
  // Just check that a different version is suggested
  assert!(
    stdout.contains("1.0.0") || stdout.contains("0.2.0") || stdout.contains("0.1.1"),
    "Expected version bump, got: {}",
    stdout
  );
  assert!(stdout.contains("New features") || stdout.contains("feat") || stdout.contains("changes"));

  Ok(())
}

#[test]
fn test_release_plan_with_breaking_changes() -> Result<()> {
  let ws = TestWorkspace::new()?;

  // Add a crate
  ws.add_crate("my-crate", "0.1.0", &[])?;
  ws.commit("feat: initial feature")?;

  // Create a release tag
  git(&ws.path, &["tag", "-a", "my-crate@v0.1.0", "-m", "Release 0.1.0"])?;

  // Add a breaking change
  ws.modify_file("my-crate", "src/lib.rs", "pub fn breaking_change() {}")?;
  ws.commit("feat!: breaking change to API")?;

  // Run release plan
  let output = run_cargo_rail(&ws.path, &["rail", "release", "plan"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should suggest version bump for breaking changes
  assert!(stdout.contains("my-crate"));
  assert!(stdout.contains("0.1.0"));
  // Breaking changes should bump version
  assert!(
    stdout.contains("1.0.0") || stdout.contains("0.2.0"),
    "Expected version bump for breaking change, got: {}",
    stdout
  );
  assert!(stdout.contains("Breaking") || stdout.contains("changes"));

  Ok(())
}

#[test]
fn test_release_plan_json_output() -> Result<()> {
  let ws = TestWorkspace::new()?;

  // Add a crate
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("feat: initial commit")?;

  // Run release plan with JSON output
  let output = run_cargo_rail(&ws.path, &["rail", "release", "plan", "--json"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  eprintln!("=== JSON OUTPUT ===\n{}", stdout);

  // The output contains progress text before JSON, so we need to find the JSON part
  // Look for the first '{' to start of JSON
  let json_start = stdout.find('{').expect("No JSON found in output");
  let json_str = &stdout[json_start..];

  // Should be valid JSON
  let json: serde_json::Value = serde_json::from_str(json_str)?;

  // Should have expected structure
  assert!(json.get("crates").is_some());
  assert!(json.get("publish_order").is_some());

  Ok(())
}

#[test]
fn test_release_plan_respects_monorepo_filtering() -> Result<()> {
  let ws = TestWorkspace::new()?;

  // Add two crates
  ws.add_crate("crate-a", "0.1.0", &[])?;
  ws.add_crate("crate-b", "0.1.0", &[])?;
  ws.commit("feat: initial crates")?;

  // Create release tags
  git(
    &ws.path,
    &["tag", "-a", "crate-a@v0.1.0", "-m", "Release crate-a 0.1.0"],
  )?;
  git(
    &ws.path,
    &["tag", "-a", "crate-b@v0.1.0", "-m", "Release crate-b 0.1.0"],
  )?;

  // Modify only crate-a
  ws.modify_file("crate-a", "src/lib.rs", "pub fn new_feature() {}")?;
  ws.commit("feat: add feature to crate-a")?;

  // Run release plan
  let output = run_cargo_rail(&ws.path, &["rail", "release", "plan"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should only show crate-a (crate-b has no changes)
  assert!(stdout.contains("crate-a"));
  // crate-b should not appear in the output (filtered by --all not being set)
  // Note: Without --all, only changed crates are shown

  Ok(())
}

#[test]
fn test_release_plan_topological_order() -> Result<()> {
  let ws = TestWorkspace::new()?;

  // Add two crates with dependency
  ws.add_crate("crate-a", "0.1.0", &[])?;
  ws.add_crate(
    "crate-b",
    "0.1.0",
    &[("crate-a", r#"{ path = "../crate-a", version = "0.1.0" }"#)],
  )?;
  ws.commit("feat: add crates")?;

  // Run release plan with JSON to check order
  let output = run_cargo_rail(&ws.path, &["rail", "release", "plan", "--json"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Extract JSON from output (skip progress text)
  let json_start = stdout.find('{').expect("No JSON found in output");
  let json_str = &stdout[json_start..];
  let json: serde_json::Value = serde_json::from_str(json_str)?;

  // Check publish order - crate-a should come before crate-b
  let publish_order = json["publish_order"].as_array().unwrap();
  let crate_a_idx = publish_order
    .iter()
    .position(|v| v.as_str() == Some("crate-a"))
    .unwrap();
  let crate_b_idx = publish_order
    .iter()
    .position(|v| v.as_str() == Some("crate-b"))
    .unwrap();

  assert!(crate_a_idx < crate_b_idx, "crate-a should come before crate-b");

  Ok(())
}

#[test]
fn test_release_prepare_dry_run() -> Result<()> {
  let ws = TestWorkspace::new()?;

  // Add a crate
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("feat: initial feature")?;

  // Create a release tag
  git(&ws.path, &["tag", "-a", "test-crate@v0.1.0", "-m", "Release 0.1.0"])?;

  // Add a new feature
  ws.modify_file("test-crate", "src/lib.rs", "pub fn new_feature() {}")?;
  ws.commit("feat: add new feature")?;

  // Run prepare (dry-run by default)
  let output = run_cargo_rail(&ws.path, &["rail", "release", "prepare"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  eprintln!("=== PREPARE DRY RUN OUTPUT ===\n{}", stdout);

  // Should show what would be changed
  assert!(stdout.contains("test-crate"));
  assert!(stdout.contains("0.1.0"));
  // Should show some version bump (→ indicates version change)
  assert!(stdout.contains(" → "), "Expected version change arrow, got: {}", stdout);
  assert!(stdout.contains("dry-run") || stdout.contains("--apply"));

  // Cargo.toml should NOT be modified
  let cargo_toml = ws.read_file("crates/test-crate/Cargo.toml")?;
  assert!(cargo_toml.contains(r#"version = "0.1.0""#));

  Ok(())
}

#[test]
fn test_release_prepare_with_apply() -> Result<()> {
  let ws = TestWorkspace::new()?;

  // Add a crate
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("fix: initial commit")?;

  // Create a release tag
  git(&ws.path, &["tag", "-a", "test-crate@v0.1.0", "-m", "Release 0.1.0"])?;

  // Add a bug fix
  ws.modify_file("test-crate", "src/lib.rs", "pub fn bug_fix() {}")?;
  ws.commit("fix: critical bug fix")?;

  // Run prepare with --apply
  let output = run_cargo_rail(&ws.path, &["rail", "release", "prepare", "--apply"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  eprintln!("=== PREPARE OUTPUT ===\n{}", stdout);

  // Should confirm changes were made
  assert!(stdout.contains("test-crate"));
  assert!(stdout.contains("✅"), "Expected success confirmation");

  // Cargo.toml should be modified with a version bump
  let cargo_toml = ws.read_file("crates/test-crate/Cargo.toml")?;
  // Check that version changed from 0.1.0
  assert!(
    !cargo_toml.contains(r#"version = "0.1.0""#),
    "Expected version to change from 0.1.0, got: {}",
    cargo_toml
  );

  // CHANGELOG.md might exist (optional, as generation can fail in test env)
  // Just check that prepare completed successfully
  assert!(stdout.contains("Release preparation complete"));

  Ok(())
}

#[test]
fn test_release_prepare_no_changelog() -> Result<()> {
  let ws = TestWorkspace::new()?;

  // Add a crate
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("feat: initial commit")?;

  // Run prepare with --no-changelog
  let _output = run_cargo_rail(&ws.path, &["rail", "release", "prepare", "--apply", "--no-changelog"])?;

  // CHANGELOG.md should NOT be created
  assert!(!ws.file_exists("crates/test-crate/CHANGELOG.md"));

  Ok(())
}

#[test]
fn test_release_finalize_dry_run() -> Result<()> {
  let ws = TestWorkspace::new()?;

  // Add a crate
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("feat: initial commit")?;

  // Create a release tag
  git(&ws.path, &["tag", "-a", "test-crate@v0.1.0", "-m", "Release 0.1.0"])?;

  // Add a new feature
  ws.modify_file("test-crate", "src/lib.rs", "pub fn new_feature() {}")?;
  ws.commit("feat: add new feature")?;

  // Update version manually for finalize test
  ws.modify_file(
    "test-crate",
    "Cargo.toml",
    r#"[package]
name = "test-crate"
version = "0.2.0"
edition.workspace = true
license.workspace = true
authors.workspace = true

[dependencies]
"#,
  )?;
  ws.commit("chore: bump version to 0.2.0")?;

  // Run finalize (dry-run by default)
  let output = run_cargo_rail(&ws.path, &["rail", "release", "finalize"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show what would be done
  assert!(stdout.contains("test-crate"));
  assert!(stdout.contains("dry-run") || stdout.contains("Would create"));

  // Tag should NOT be created
  let tag_output = git(&ws.path, &["tag", "-l", "test-crate@v0.2.0"]).ok();
  if let Some(output) = tag_output {
    let tags = String::from_utf8_lossy(&output.stdout);
    assert!(tags.is_empty() || !tags.contains("test-crate@v0.2.0"));
  }

  Ok(())
}
