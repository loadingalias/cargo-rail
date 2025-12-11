//! Integration tests for `cargo rail config` commands (validate, sync)

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;
use std::fs;

#[test]
fn test_config_validate_valid_config() -> Result<()> {
  let ws = TestWorkspace::new_named("config-validate-valid")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  // Run config validate
  let output = run_cargo_rail(&ws.path, &["rail", "config", "validate"])?;

  // Verify success
  assert!(
    output.status.success(),
    "config validate should succeed with valid config"
  );

  // Verify output contains success message
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("configuration is valid"),
    "output should confirm valid config"
  );

  Ok(())
}

#[test]
fn test_config_validate_no_config() -> Result<()> {
  let ws = TestWorkspace::new_named("config-validate-no-config")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;
  ws.remove_config()?;

  // Run config validate
  let output = run_cargo_rail(&ws.path, &["rail", "config", "validate"])?;

  // Verify failure
  assert!(!output.status.success(), "config validate should fail without config");

  // Verify helpful message
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stdout.contains("no configuration file found") || stderr.contains("no configuration file found"),
    "output should say no config found. stdout: {}, stderr: {}",
    stdout,
    stderr
  );
  assert!(
    stdout.contains("cargo rail init") || stderr.contains("cargo rail init"),
    "output should suggest running init"
  );

  Ok(())
}

#[test]
fn test_config_validate_json_output() -> Result<()> {
  let ws = TestWorkspace::new_named("config-validate-json")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  // Run config validate with JSON format
  let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "-f", "json"])?;

  // Verify success
  assert!(output.status.success(), "config validate -f json should succeed");

  // Verify JSON output
  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;

  assert_eq!(json["command"], "config");
  assert_eq!(json["action"], "validate");
  assert_eq!(json["valid"], true);
  assert!(json["config_path"].as_str().is_some());
  assert!(json["errors"].as_array().is_some());
  assert!(json["warnings"].as_array().is_some());

  Ok(())
}

#[test]
fn test_config_validate_no_config_json() -> Result<()> {
  let ws = TestWorkspace::new_named("config-validate-no-config-json")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;
  ws.remove_config()?;

  // Run config validate with JSON format
  let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "-f", "json"])?;

  // Verify failure (exit code 2)
  assert!(!output.status.success());

  // Verify JSON output shows error
  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;

  assert_eq!(json["command"], "config");
  assert_eq!(json["valid"], false);
  assert!(json["config_path"].is_null());
  let errors = json["errors"].as_array().unwrap();
  assert!(!errors.is_empty());
  assert!(
    errors[0]["message"]
      .as_str()
      .unwrap()
      .contains("no configuration file found")
  );

  Ok(())
}

// Note: Tests for invalid targets and split config validation are skipped
// because those validations happen during config loading, not during
// `config validate`. The config validate command checks for semantic issues
// that can only be detected after the config is loaded successfully.

#[test]
fn test_config_validate_global_json_flag() -> Result<()> {
  let ws = TestWorkspace::new_named("config-validate-global-json")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  // Run config validate with global --json flag
  let output = run_cargo_rail(&ws.path, &["rail", "--json", "config", "validate"])?;

  // Verify success
  assert!(output.status.success(), "config validate with --json should succeed");

  // Verify JSON output
  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;

  assert_eq!(json["command"], "config");
  assert_eq!(json["valid"], true);

  Ok(())
}

// ============================================================================
// Config Sync Tests
// ============================================================================

#[test]
fn test_config_sync_adds_missing_fields() -> Result<()> {
  let ws = TestWorkspace::new_named("config-sync-adds-fields")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  // Create minimal config (missing most fields)
  let config_path = ws.path.join(".config").join("rail.toml");
  fs::write(
    &config_path,
    r#"targets = ["x86_64-unknown-linux-gnu"]

[unify]
msrv = true
"#,
  )?;

  // Run config sync
  let output = run_cargo_rail(&ws.path, &["rail", "config", "sync"])?;

  assert!(output.status.success(), "config sync should succeed");

  // Verify new fields were added
  let config = fs::read_to_string(&config_path)?;
  assert!(config.contains("msrv_source"), "msrv_source should be added");
  assert!(
    config.contains("preserve_features"),
    "preserve_features should be added"
  );
  assert!(
    config.contains("prune_dead_features"),
    "prune_dead_features should be added"
  );
  assert!(config.contains("tag_format"), "tag_format should be added to [release]");

  // Verify existing value preserved
  assert!(
    config.contains("msrv = true"),
    "existing msrv value should be preserved"
  );
  assert!(
    config.contains("x86_64-unknown-linux-gnu"),
    "existing target should be preserved"
  );

  Ok(())
}

#[test]
fn test_config_sync_preserves_user_values() -> Result<()> {
  let ws = TestWorkspace::new_named("config-sync-preserves-values")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  // Create config with custom values
  let config_path = ws.path.join(".config").join("rail.toml");
  fs::write(
    &config_path,
    r#"targets = ["wasm32-unknown-unknown"]

[unify]
msrv = false
msrv_source = "workspace"
max_backups = 10
exclude = ["some-dep"]

[release]
tag_prefix = "release-"
sign_tags = true
"#,
  )?;

  // Run config sync
  let output = run_cargo_rail(&ws.path, &["rail", "config", "sync"])?;
  assert!(output.status.success(), "config sync should succeed");

  // Verify user values preserved
  let config = fs::read_to_string(&config_path)?;
  assert!(config.contains("msrv = false"), "user msrv=false should be preserved");
  assert!(
    config.contains(r#"msrv_source = "workspace""#),
    "user msrv_source should be preserved"
  );
  assert!(
    config.contains("max_backups = 10"),
    "user max_backups should be preserved"
  );
  assert!(
    config.contains(r#"tag_prefix = "release-""#),
    "user tag_prefix should be preserved"
  );
  assert!(
    config.contains("sign_tags = true"),
    "user sign_tags should be preserved"
  );

  Ok(())
}

#[test]
fn test_config_sync_check_mode_does_not_modify() -> Result<()> {
  let ws = TestWorkspace::new_named("config-sync-check-mode")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  // Create minimal config
  let config_path = ws.path.join(".config").join("rail.toml");
  let original_config = r#"targets = ["x86_64-unknown-linux-gnu"]

[unify]
msrv = true
"#;
  fs::write(&config_path, original_config)?;

  // Run config sync --check
  let output = run_cargo_rail(&ws.path, &["rail", "config", "sync", "--check"])?;

  // Should exit with code 1 (changes needed)
  assert!(
    !output.status.success(),
    "config sync --check should exit 1 when changes needed"
  );

  // File should NOT be modified
  let config_after = fs::read_to_string(&config_path)?;
  assert_eq!(
    original_config, config_after,
    "config should not be modified in check mode"
  );

  Ok(())
}

#[test]
fn test_config_sync_idempotent() -> Result<()> {
  let ws = TestWorkspace::new_named("config-sync-idempotent")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  // Run config sync twice
  let output1 = run_cargo_rail(&ws.path, &["rail", "config", "sync"])?;
  assert!(output1.status.success(), "first sync should succeed");

  let output2 = run_cargo_rail(&ws.path, &["rail", "config", "sync"])?;
  assert!(output2.status.success(), "second sync should succeed");

  // Second run should report "up to date"
  let stdout = String::from_utf8_lossy(&output2.stdout);
  assert!(stdout.contains("up to date"), "second run should report 'up to date'");

  Ok(())
}

#[test]
fn test_config_sync_detects_yaml_targets() -> Result<()> {
  let ws = TestWorkspace::new_named("config-sync-yaml-targets")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  // Create minimal config
  let config_path = ws.path.join(".config").join("rail.toml");
  fs::write(
    &config_path,
    r#"targets = ["x86_64-unknown-linux-gnu"]

[unify]
msrv = true
"#,
  )?;

  // Add GitHub workflow with targets
  let workflows_dir = ws.path.join(".github").join("workflows");
  fs::create_dir_all(&workflows_dir)?;
  fs::write(
    workflows_dir.join("ci.yml"),
    r#"name: CI
jobs:
  build:
    strategy:
      matrix:
        target:
          - aarch64-apple-darwin
          - x86_64-pc-windows-msvc
"#,
  )?;

  // Run config sync --check to preview
  let output = run_cargo_rail(&ws.path, &["rail", "config", "sync", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should detect new targets
  assert!(
    stdout.contains("aarch64-apple-darwin"),
    "should detect aarch64-apple-darwin from YAML"
  );
  assert!(
    stdout.contains("x86_64-pc-windows-msvc"),
    "should detect x86_64-pc-windows-msvc from YAML"
  );

  Ok(())
}

#[test]
fn test_config_sync_preserves_existing_targets() -> Result<()> {
  let ws = TestWorkspace::new_named("config-sync-preserves-targets")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  // Create config with user-specified target
  let config_path = ws.path.join(".config").join("rail.toml");
  fs::write(
    &config_path,
    r#"targets = ["wasm32-unknown-unknown"]

[unify]
msrv = true
"#,
  )?;

  // Run config sync (no external targets to detect)
  let output = run_cargo_rail(&ws.path, &["rail", "config", "sync"])?;
  assert!(output.status.success(), "config sync should succeed");

  // Verify user target preserved
  let config = fs::read_to_string(&config_path)?;
  assert!(
    config.contains("wasm32-unknown-unknown"),
    "user-specified target should be preserved"
  );

  Ok(())
}

#[test]
fn test_config_sync_json_output() -> Result<()> {
  let ws = TestWorkspace::new_named("config-sync-json")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  // Create minimal config
  let config_path = ws.path.join(".config").join("rail.toml");
  fs::write(
    &config_path,
    r#"targets = ["x86_64-unknown-linux-gnu"]

[unify]
msrv = true
"#,
  )?;

  // Run config sync with JSON output
  let output = run_cargo_rail(&ws.path, &["rail", "config", "sync", "-f", "json"])?;
  assert!(output.status.success(), "config sync -f json should succeed");

  // Verify JSON structure
  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;

  assert_eq!(json["command"], "config");
  assert_eq!(json["action"], "sync");
  assert!(json["config_path"].as_str().is_some());
  assert!(json["fields_added"].as_array().is_some());
  assert!(json["has_changes"].as_bool().is_some());

  Ok(())
}

#[test]
fn test_config_sync_no_config_fails() -> Result<()> {
  let ws = TestWorkspace::new_named("config-sync-no-config")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;
  ws.remove_config()?;

  // Run config sync
  let output = run_cargo_rail(&ws.path, &["rail", "config", "sync"])?;

  // Should fail
  assert!(!output.status.success(), "config sync should fail without config");

  // Should suggest running init
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("cargo rail init"),
    "should suggest running 'cargo rail init'"
  );

  Ok(())
}
