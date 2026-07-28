//! Integration tests for `cargo rail config` commands (locate, print, validate, explain, migrate)

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;
use std::fs;

// Config Locate Tests

#[test]
fn test_config_locate_finds_config() -> Result<()> {
  let ws = TestWorkspace::new_named("config-locate-finds")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  // Run config locate
  let output = run_cargo_rail(&ws.path, &["rail", "config", "locate"])?;

  // Verify success
  assert!(output.status.success(), "config locate should succeed");

  // Verify output contains path
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(stdout.contains("rail.toml"), "output should contain config path");

  Ok(())
}

#[test]
fn test_config_locate_no_config() -> Result<()> {
  let ws = TestWorkspace::new_named("config-locate-no-config")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;
  ws.remove_config()?;

  // Run config locate
  let output = run_cargo_rail(&ws.path, &["rail", "config", "locate"])?;

  // Verify failure
  assert!(!output.status.success(), "config locate should fail without config");

  // Verify helpful message
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("no config file found"),
    "output should say no config found"
  );
  assert!(stdout.contains("cargo rail init"), "output should suggest running init");

  Ok(())
}

#[test]
fn test_config_locate_json_output() -> Result<()> {
  let ws = TestWorkspace::new_named("config-locate-json")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  // Run config locate with JSON format
  let output = run_cargo_rail(&ws.path, &["rail", "config", "locate", "-f", "json"])?;

  // Verify success
  assert!(output.status.success(), "config locate -f json should succeed");

  // Verify JSON output
  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;

  assert_eq!(json["command"], "config");
  assert_eq!(json["action"], "locate");
  assert_eq!(json["found"], true);
  let path = json["path"].as_str().expect("path should be a string");
  assert!(path.ends_with("rail.toml"), "path should point to rail.toml");
  let search_paths = json["search_paths"]
    .as_array()
    .expect("search_paths should be an array");
  assert!(
    !search_paths.is_empty(),
    "search_paths should include checked config locations"
  );

  Ok(())
}

#[test]
fn test_config_locate_with_config_flag() -> Result<()> {
  let ws = TestWorkspace::new_named("config-locate-with-flag")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  // Create a custom config file
  let custom_config = ws.path.join("custom-rail.toml");
  fs::write(&custom_config, "targets = []\n")?;

  // Run config locate with --config flag
  let output = run_cargo_rail(&ws.path, &["rail", "--config", "custom-rail.toml", "config", "locate"])?;

  // Verify success
  assert!(output.status.success(), "config locate with --config should succeed");

  // Verify output contains the custom path
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("custom-rail.toml"),
    "output should contain custom config path"
  );

  Ok(())
}

// Config Print Tests

#[test]
fn test_config_print_shows_defaults() -> Result<()> {
  let ws = TestWorkspace::new_named("config-print-defaults")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  // Create minimal config
  let config_path = ws.path.join(".config").join("rail.toml");
  fs::write(&config_path, "targets = []\n")?;

  // Run config print
  let output = run_cargo_rail(&ws.path, &["rail", "config", "print"])?;

  // Verify success
  assert!(output.status.success(), "config print should succeed");

  // Verify output shows defaults
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("# Effective configuration"),
    "output should have header comment"
  );
  assert!(stdout.contains("[unify]"), "output should contain [unify] section");
  assert!(stdout.contains("msrv"), "output should contain default msrv setting");
  assert!(stdout.contains("[release]"), "output should contain [release] section");

  Ok(())
}

#[test]
fn test_config_print_json_output() -> Result<()> {
  let ws = TestWorkspace::new_named("config-print-json")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  // Run config print with JSON format
  let output = run_cargo_rail(&ws.path, &["rail", "config", "print", "-f", "json"])?;

  // Verify success
  assert!(output.status.success(), "config print -f json should succeed");

  // Verify JSON output
  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;

  assert_eq!(json["command"], "config");
  assert_eq!(json["action"], "print");
  let config_path = json["config_path"].as_str().expect("config_path should be a string");
  assert!(
    config_path.ends_with("rail.toml"),
    "config_path should point to rail.toml"
  );
  assert!(json["config"].is_object());
  assert!(json["config"]["unify"].is_object());

  Ok(())
}

#[test]
fn test_config_print_no_config() -> Result<()> {
  let ws = TestWorkspace::new_named("config-print-no-config")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;
  ws.remove_config()?;

  // Run config print
  let output = run_cargo_rail(&ws.path, &["rail", "config", "print"])?;

  // Verify failure
  assert!(!output.status.success(), "config print should fail without config");

  // Should suggest running init
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("cargo rail init"),
    "should suggest running 'cargo rail init'"
  );

  Ok(())
}

// Config Validate Tests

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
fn test_config_validate_accepts_empty_config() -> Result<()> {
  let ws = TestWorkspace::new_named("config-validate-empty")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  let config_path = ws.path.join(".config").join("rail.toml");
  fs::write(&config_path, "")?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--strict", "-f", "json"])?;
  assert!(output.status.success(), "an empty rail.toml must be valid");

  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;
  assert_eq!(json["valid"], true);
  assert_eq!(json["errors"], serde_json::Value::Array(vec![]));
  assert_eq!(json["warnings"], serde_json::Value::Array(vec![]));

  Ok(())
}

#[test]
fn test_config_validate_warns_with_migration_guidance_for_deprecated_fields() -> Result<()> {
  let ws = TestWorkspace::new_named("config-validate-deprecated-toggles")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[unify]
compiler_diag_cache = false
sort_dependencies = false

[release]
require_clean = true
publish_delay = 5
"#,
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--no-strict", "-f", "json"])?;
  assert!(
    output.status.success(),
    "deprecated compatibility input should warn locally"
  );

  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  let warnings = json["warnings"].as_array().expect("warnings array");
  assert!(warnings.iter().any(|warning| {
    warning["message"]
      .as_str()
      .is_some_and(|message| message.contains("unify.compiler_diag_cache") && message.contains("config migrate"))
  }));
  assert!(warnings.iter().any(|warning| {
    warning["message"]
      .as_str()
      .is_some_and(|message| message.contains("unify.sort_dependencies") && message.contains("config migrate"))
  }));
  for path in ["release.require_clean", "release.publish_delay"] {
    assert!(
      warnings.iter().any(|warning| {
        warning["message"]
          .as_str()
          .is_some_and(|message| message.contains(path) && message.contains("cargo rail config migrate"))
      }),
      "{path} warning must name the migration command"
    );
  }

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
  let config_path = json["config_path"].as_str().expect("config_path should be a string");
  assert!(
    config_path.ends_with("rail.toml"),
    "config_path should point to rail.toml"
  );
  let errors = json["errors"].as_array().expect("errors should be an array");
  let warnings = json["warnings"].as_array().expect("warnings should be an array");
  assert!(errors.is_empty(), "valid config should have no errors");
  assert!(warnings.is_empty(), "valid config should have no warnings");

  Ok(())
}

#[test]
fn test_config_validate_rejects_invalid_unify_glob() -> Result<()> {
  let ws = TestWorkspace::new_named("config-validate-invalid-unify-glob")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  fs::write(
    ws.path.join(".config/rail.toml"),
    "[unify]\npreserve_features = [\"[\"]\n",
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "-f", "json"])?;
  assert!(!output.status.success(), "invalid unify glob should fail validation");

  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(json["valid"], false);
  assert!(
    json["errors"]
      .as_array()
      .is_some_and(|errors| errors.iter().any(|error| {
        error["message"].as_str().is_some_and(|message| {
          message.contains("invalid glob pattern") && message.contains("unify.preserve_features")
        })
      }))
  );

  Ok(())
}

#[test]
fn test_config_validate_rejects_empty_split_branch() -> Result<()> {
  let ws = TestWorkspace::new_named("config-validate-empty-split-branch")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[crates.test-crate.split]
remote = "https://example.invalid/test-crate.git"
branch = ""
mode = "single"
members = ["test-crate"]
"#,
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "-f", "json"])?;
  assert!(!output.status.success(), "empty split branch should fail validation");

  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(json["valid"], false);
  assert!(
    json["errors"]
      .as_array()
      .is_some_and(|errors| errors.iter().any(|error| {
        error["message"]
          .as_str()
          .is_some_and(|message| message == "branch must not be empty")
      }))
  );

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

#[test]
fn test_config_validate_with_config_flag() -> Result<()> {
  let ws = TestWorkspace::new_named("config-validate-with-flag")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;
  ws.remove_config()?;

  let custom_config = ws.path.join("custom-rail.toml");
  fs::write(&custom_config, "targets = []\n")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "--config", "custom-rail.toml", "config", "validate"],
  )?;
  assert!(output.status.success(), "config validate with --config should succeed");

  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("custom-rail.toml"),
    "should validate override config path"
  );
  assert!(
    stdout.contains("configuration is valid"),
    "output should confirm valid config"
  );

  Ok(())
}

#[test]
fn test_config_validate_with_missing_config_flag_fails() -> Result<()> {
  let ws = TestWorkspace::new_named("config-validate-with-missing-flag")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;
  ws.remove_config()?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "--config", "missing.toml", "config", "validate", "-f", "json"],
  )?;
  assert!(!output.status.success(), "validate with missing --config should fail");

  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;
  assert_eq!(json["valid"], false);
  let errors = json["errors"].as_array().unwrap();
  assert!(
    errors
      .iter()
      .filter_map(|e| e["message"].as_str())
      .any(|msg| msg.contains("specified config file not found")),
    "expected missing override error. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_config_validate_rejects_invalid_run_profile_action() -> Result<()> {
  let ws = TestWorkspace::new_named("config-validate-run-invalid-surface")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  let config_path = ws.path.join(".config").join("rail.toml");
  fs::write(
    &config_path,
    r#"[run.profile.bad]
surfaces = ["not-a-surface"]
"#,
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "-f", "json"])?;
  assert!(!output.status.success(), "invalid run action should fail validation");

  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;
  assert_eq!(json["valid"], false);
  let errors = json["errors"].as_array().unwrap();
  assert!(
    errors
      .iter()
      .filter_map(|e| e["message"].as_str())
      .any(|msg| msg.contains("unknown action 'not-a-surface'")),
    "expected unknown action error. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_config_validate_rejects_infra_run_profile_surface() -> Result<()> {
  let ws = TestWorkspace::new_named("config-validate-run-infra-surface")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  let config_path = ws.path.join(".config").join("rail.toml");
  fs::write(
    &config_path,
    r#"[run.profile.bad]
surfaces = ["infra"]
"#,
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "-f", "json"])?;
  assert!(!output.status.success(), "infra run surface should fail validation");

  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;
  assert_eq!(json["valid"], false);
  let errors = json["errors"].as_array().unwrap();
  assert!(
    errors
      .iter()
      .filter_map(|e| e["message"].as_str())
      .any(|msg| msg.contains("`infra` is a planner output")),
    "expected infra planner-output error. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_config_validate_strict_reports_unknown_run_profile_key() -> Result<()> {
  let ws = TestWorkspace::new_named("config-validate-run-unknown-key")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  let config_path = ws.path.join(".config").join("rail.toml");
  fs::write(
    &config_path,
    r#"[run.profile.ci]
surfaces = ["build", "test"]
unexpected = true
"#,
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--strict", "-f", "json"])?;
  assert!(
    !output.status.success(),
    "strict mode should fail on unknown run profile key"
  );

  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;
  assert_eq!(json["valid"], false);
  let errors = json["errors"].as_array().unwrap();
  assert!(
    errors
      .iter()
      .filter_map(|e| e["message"].as_str())
      .any(|msg| msg.contains("unknown configuration key 'run.profile.ci.unexpected'")),
    "expected strict unknown-key error. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_config_validate_rejects_invalid_run_workflow_mapping() -> Result<()> {
  let ws = TestWorkspace::new_named("config-validate-run-invalid-workflow")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  let config_path = ws.path.join(".config").join("rail.toml");
  fs::write(
    &config_path,
    r#"[run.workflow]
commit = "missing_profile"
"#,
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "-f", "json"])?;
  assert!(
    !output.status.success(),
    "invalid run workflow mapping should fail validation"
  );

  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;
  assert_eq!(json["valid"], false);
  let errors = json["errors"].as_array().unwrap();
  assert!(
    errors
      .iter()
      .filter_map(|e| e["message"].as_str())
      .any(|msg| msg.contains("unknown profile 'missing_profile'")),
    "expected unknown workflow profile error. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_config_validate_rejects_invalid_run_profile_token() -> Result<()> {
  let ws = TestWorkspace::new_named("config-validate-run-invalid-token")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  let config_path = ws.path.join(".config").join("rail.toml");
  fs::write(
    &config_path,
    r#"[run.profile.docs_custom]
surfaces = ["docs"]
run_args = ["--manifest-path", "{bad_token}/Cargo.toml"]
"#,
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "-f", "json"])?;
  assert!(!output.status.success(), "invalid token should fail validation");

  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;
  assert_eq!(json["valid"], false);
  let errors = json["errors"].as_array().unwrap();
  assert!(
    errors
      .iter()
      .filter_map(|e| e["message"].as_str())
      .any(|msg| msg.contains("unknown token '{bad_token}'")),
    "expected unknown token validation error. Output:\n{}",
    stdout
  );

  Ok(())
}

// Config Explain Tests

#[test]
fn test_config_explain_json_reports_effective_default_and_source() -> Result<()> {
  let ws = TestWorkspace::new_named("config-explain-json")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  let config_path = ws.path.join(".config").join("rail.toml");
  fs::write(&config_path, "[unify]\nmsrv_policy = { mode = \"disabled\" }\n")?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "explain", "-f", "json"])?;
  assert!(output.status.success(), "config explain should succeed");
  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(json["command"], "config");
  assert_eq!(json["action"], "explain");

  let fields = json["fields"].as_array().expect("fields array");
  let msrv = fields
    .iter()
    .find(|field| field["path"] == "unify.msrv_policy.mode")
    .expect("unify.msrv_policy.mode explanation");
  assert_eq!(msrv["effective"], "disabled");
  assert_eq!(msrv["default"], "compute");
  assert_eq!(msrv["source"], json["config_path"]);
  assert_eq!(msrv["classification"], "project_policy");
  assert!(msrv["why"].as_str().is_some_and(|why| !why.is_empty()));

  let consumer_scope = fields
    .iter()
    .find(|field| field["path"] == "unify.consumer_scope")
    .expect("unify.consumer_scope explanation");
  assert_eq!(consumer_scope["effective"], "open");
  assert_eq!(consumer_scope["default"], "open");
  assert_eq!(consumer_scope["source"], "default");

  Ok(())
}

#[test]
fn test_config_explain_text_uses_same_field_values_as_json() -> Result<()> {
  let ws = TestWorkspace::new_named("config-explain-text")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;
  fs::write(
    ws.path.join(".config/rail.toml"),
    "[unify]\nmsrv_policy = { mode = \"disabled\" }\n",
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "explain"])?;
  assert!(output.status.success());
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(stdout.contains("unify.msrv_policy.mode"));
  assert!(stdout.contains("effective: disabled"));
  assert!(stdout.contains("default: compute"));
  assert!(stdout.contains("source:"));

  Ok(())
}

#[test]
fn test_config_explain_marks_removed_input_as_ineffective() -> Result<()> {
  let ws = TestWorkspace::new_named("config-explain-removed")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;
  fs::write(
    ws.path.join(".config/rail.toml"),
    "[unify]\ncompiler_diag_cache = false\n",
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "explain", "-f", "json"])?;
  assert!(output.status.success());
  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  let field = json["fields"]
    .as_array()
    .and_then(|fields| fields.iter().find(|field| field["path"] == "unify.compiler_diag_cache"))
    .expect("removed field explanation");
  assert_eq!(field["configured"], false);
  assert_eq!(field["effective"], serde_json::Value::Null);
  assert_eq!(field["default"], serde_json::Value::Null);
  assert_eq!(field["classification"], "implementation_detail");
  assert!(
    field["deprecation"]
      .as_str()
      .is_some_and(|message| message.contains("config migrate"))
  );

  Ok(())
}

#[test]
fn test_config_explain_attributes_legacy_policy_to_config() -> Result<()> {
  let ws = TestWorkspace::new_named("config-explain-legacy-policy")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;
  fs::write(ws.path.join(".config/rail.toml"), "[release]\npush = true\n")?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "explain", "-f", "json"])?;
  assert!(output.status.success());
  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  let field = json["fields"]
    .as_array()
    .and_then(|fields| fields.iter().find(|field| field["path"] == "release.remote_effects"))
    .expect("effective release policy explanation");
  assert_eq!(field["effective"], "push");
  assert_eq!(field["source"], json["config_path"]);

  Ok(())
}

// Config Migrate Tests

#[test]
fn test_config_migrate_check_is_read_only_and_exits_one() -> Result<()> {
  let ws = TestWorkspace::new_named("config-migrate-check")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  let config_path = ws.path.join(".config").join("rail.toml");
  let original_config = r#"[unify]
compiler_diag_cache = false
sort_dependencies = false

[release]
require_clean = true
publish_delay = 5
"#;
  fs::write(&config_path, original_config)?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "--check", "-f", "json"])?;
  assert_eq!(output.status.code(), Some(1), "pending migration must exit 1");

  let config_after = fs::read_to_string(&config_path)?;
  assert_eq!(original_config, config_after, "--check must not modify rail.toml");

  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(json["command"], "config");
  assert_eq!(json["action"], "migrate");
  assert_eq!(json["has_changes"], true);
  let changes = json["changes"].as_array().expect("changes array");
  assert_eq!(changes.len(), 4);
  assert!(changes.iter().all(|change| change["kind"] == "remove"));
  let paths = changes
    .iter()
    .filter_map(|change| change["path"].as_str())
    .collect::<std::collections::BTreeSet<_>>();
  assert_eq!(
    paths,
    std::collections::BTreeSet::from([
      "release.publish_delay",
      "release.require_clean",
      "unify.compiler_diag_cache",
      "unify.sort_dependencies",
    ])
  );

  Ok(())
}

#[test]
fn test_config_migrate_replaces_split_paths_with_cargo_member_names() -> Result<()> {
  let ws = TestWorkspace::new_named("config-migrate-split-members")?;
  ws.add_crate("member-a", "0.1.0", &[])?;
  ws.add_crate("member-b", "0.1.0", &[])?;
  ws.commit("Add split members")?;
  let config_path = ws.path.join(".config/rail.toml");
  fs::write(
    &config_path,
    r#"[crates.bundle.split]
remote = "../bundle"
branch = "main"
mode = "combined"
paths = [{ crate = "crates/member-b" }, { crate = "crates/member-a" }]
"#,
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
  assert!(output.status.success());
  let migrated = fs::read_to_string(config_path)?;
  assert!(!migrated.contains("paths ="));
  assert!(
    migrated.contains("members = [\"member-a\", \"member-b\"]"),
    "{migrated}"
  );
  Ok(())
}

#[test]
fn test_config_migrate_applies_explicit_renames_and_removals() -> Result<()> {
  let ws = TestWorkspace::new_named("config-migrate-apply")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;

  let config_path = ws.path.join(".config").join("rail.toml");
  fs::write(
    &config_path,
    r#"[workspace]
root = "."

[toolchain]
channel = "stable"

[unify]
compiler_diag_cache = false
sort_dependencies = false
prune_dead_features = false
detect_unused = false
remove_unused = false
detect_undeclared_features = false
fix_undeclared_features = false
pin_transitives = true
transitive_host = "root"
msrv = true
msrv_source = "workspace"
enforce_msrv_inheritance = true

[release]
require_clean = true
publish_delay = 5
push = true
create_github_release = true
forge = "gitlab"

[change-detection]
conservative_unclassified_owner_fallback = true
bot_pr_confidence_profile = "strict"

[run.profile.ci]
surfaces = ["build", "test"]
merge_base = true

[run.profile.local]
surfaces = ["test"]
since = "HEAD~1"

[crates.demo.sync]
"#,
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
  assert!(output.status.success(), "config migrate should apply known migrations");

  let migrated = fs::read_to_string(&config_path)?;
  assert!(!migrated.contains("compiler_diag_cache"));
  assert!(!migrated.contains("sort_dependencies"));
  assert!(!migrated.contains("prune_dead_features"));
  assert!(!migrated.contains("detect_unused"));
  assert!(!migrated.contains("remove_unused"));
  assert!(!migrated.contains("detect_undeclared_features"));
  assert!(!migrated.contains("fix_undeclared_features"));
  assert!(!migrated.contains("pin_transitives"));
  assert!(!migrated.contains("transitive_host"));
  assert!(migrated.contains("transitive_pinning = { host = \"root\" }"));
  assert!(!migrated.contains("msrv_source"));
  assert!(!migrated.contains("enforce_msrv_inheritance"));
  assert!(migrated.contains("msrv_policy = { mode = \"compute\", source = \"workspace\", inherit = true }"));
  assert!(!migrated.contains("create_github_release"));
  assert!(!migrated.contains("require_clean"));
  assert!(!migrated.contains("publish_delay"));
  assert!(!migrated.contains("push ="));
  assert!(!migrated.contains("forge ="));
  assert!(migrated.contains("remote_effects = \"gitlab\""));
  assert!(!migrated.contains("bot_pr_confidence_profile"));
  assert!(!migrated.contains("[workspace]"));
  assert!(!migrated.contains("[toolchain]"));
  assert!(!migrated.contains("[crates.demo.sync]"));
  assert!(!migrated.contains("conservative_unclassified_owner_fallback"));
  assert!(migrated.contains("unknown_file_policy = \"owned_build_test\""));
  assert!(!migrated.contains("merge_base"));
  assert!(migrated.contains("baseline = { kind = \"merge-base\" }"));
  assert!(!migrated.contains("since ="));
  assert!(migrated.contains("baseline = { kind = \"since\", reference = \"HEAD~1\" }"));

  let second = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "--check"])?;
  assert!(second.status.success(), "applied migrations must be idempotent");

  Ok(())
}

#[test]
fn test_config_migrate_normalizes_legacy_policy_values_without_overriding_explicit_policy() -> Result<()> {
  let ws = TestWorkspace::new_named("config-migrate-policy-values")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;
  let config_path = ws.path.join(".config/rail.toml");

  fs::write(&config_path, "[change-detection]\nunknown_file_policy = true\n")?;
  let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
  assert!(output.status.success());
  assert!(fs::read_to_string(&config_path)?.contains("unknown_file_policy = \"owned_build_test\""));

  fs::write(
    &config_path,
    "[change-detection]\nunknown_file_policy = \"workspace_infra\"\nconservative_unclassified_owner_fallback = false\n",
  )?;
  let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
  assert!(output.status.success());
  let migrated = fs::read_to_string(&config_path)?;
  assert!(migrated.contains("unknown_file_policy = \"workspace_infra\""));
  assert!(!migrated.contains("conservative_unclassified_owner_fallback"));

  Ok(())
}

#[test]
fn test_config_migrate_reports_default_equivalents_as_omitted() -> Result<()> {
  let ws = TestWorkspace::new_named("config-migrate-default-equivalents")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;
  let config_path = ws.path.join(".config/rail.toml");
  let original = r#"[unify]
pin_transitives = false
msrv = true
msrv_source = "max"
enforce_msrv_inheritance = false

[release]
push = false
create_github_release = false

[run.profile.ci]
surfaces = ["test"]
merge_base = false
"#;
  fs::write(&config_path, original)?;

  let check = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "--check", "-f", "json"])?;
  assert_eq!(check.status.code(), Some(1));
  assert_eq!(fs::read_to_string(&config_path)?, original);
  let json: serde_json::Value = serde_json::from_slice(&check.stdout)?;
  let replacements: Vec<_> = json["changes"]
    .as_array()
    .expect("migration changes")
    .iter()
    .filter_map(|change| change["replacement"].as_str())
    .collect();
  assert!(!replacements.is_empty());
  assert!(
    replacements
      .iter()
      .any(|replacement| { replacement == &"run.profile.ci.actions = [\"test\"]" })
  );
  assert!(
    replacements
      .iter()
      .filter(|replacement| !replacement.starts_with("run.profile.ci.actions"))
      .all(|replacement| replacement.starts_with("field omitted ("))
  );

  let apply = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
  assert!(apply.status.success());
  assert_eq!(
    fs::read_to_string(&config_path)?.trim_start(),
    "[run.profile.ci]\nactions = [\"test\"]\n"
  );

  Ok(())
}

#[test]
fn test_config_migrate_refuses_invalid_legacy_release_effects() -> Result<()> {
  let ws = TestWorkspace::new_named("config-migrate-invalid-release-effects")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;
  let config_path = ws.path.join(".config/rail.toml");
  let original = "[release]\ncreate_github_release = true\npush = false\n";
  fs::write(&config_path, original)?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
  assert_eq!(output.status.code(), Some(2));
  assert_eq!(fs::read_to_string(&config_path)?, original);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(stderr.contains("choose one explicit policy"));

  Ok(())
}

#[test]
fn test_config_migrate_refuses_conflicting_legacy_run_baseline() -> Result<()> {
  let ws = TestWorkspace::new_named("config-migrate-invalid-run-baseline")?;
  ws.add_crate("test-crate", "0.1.0", &[])?;
  ws.commit("Add test crate")?;
  let config_path = ws.path.join(".config/rail.toml");
  let original = "[run.profile.ci]\nsurfaces = [\"test\"]\nsince = \"HEAD~1\"\nmerge_base = true\n";
  fs::write(&config_path, original)?;

  let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
  assert_eq!(output.status.code(), Some(2));
  assert_eq!(fs::read_to_string(&config_path)?, original);
  assert!(String::from_utf8_lossy(&output.stderr).contains("selects one baseline mode"));

  Ok(())
}
