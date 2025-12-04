//! Integration tests for `cargo rail config validate` command

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;

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
