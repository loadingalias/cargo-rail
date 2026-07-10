//! Integration tests for command-specific output format contracts.

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;

#[test]
fn test_unsupported_output_formats_fail_during_cli_parsing() -> Result<()> {
  let ws = TestWorkspace::new_named("output-format-domains")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  let cases: &[(&str, &[&str], &str)] = &[
    (
      "sync",
      &["rail", "sync", "lib-a", "--format", "github"],
      "[possible values: text, json]",
    ),
    (
      "release",
      &["rail", "release", "check", "lib-a", "--format", "jsonl"],
      "[possible values: text, json]",
    ),
    (
      "clean",
      &["rail", "clean", "--format", "names-only"],
      "[possible values: text, json]",
    ),
    (
      "config",
      &["rail", "config", "locate", "--format", "cargo-args"],
      "[possible values: text, json]",
    ),
    (
      "hash",
      &["rail", "hash", "--format", "github-matrix"],
      "[possible values: text, json]",
    ),
    (
      "diff-hash",
      &["rail", "diff-hash", "a.json", "b.json", "--format", "jsonl"],
      "[possible values: text, json]",
    ),
    (
      "split",
      &["rail", "split", "run", "lib-a", "--format", "github"],
      "[possible values: text, json, names-only, jsonl]",
    ),
    (
      "change",
      &["rail", "change", "status", "--format", "github"],
      "[possible values: text, json, names-only]",
    ),
    (
      "unify",
      &["rail", "unify", "--check", "--format", "names-only"],
      "[possible values: text, json]",
    ),
    (
      "plan",
      &["rail", "plan", "--format", "jsonl"],
      "[possible values: text, json, github, github-debug]",
    ),
  ];

  for (name, args, expected_values) in cases {
    let output = run_cargo_rail(&ws.path, args)?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
      output.status.code(),
      Some(2),
      "{name} must reject an unsupported format during parsing. Stderr:\n{stderr}"
    );
    assert!(
      stderr.contains("invalid value") && stderr.contains(expected_values),
      "{name} should report its exact format domain. Stderr:\n{stderr}"
    );
  }

  Ok(())
}

#[test]
fn test_global_json_rejects_commands_without_structured_contracts() -> Result<()> {
  let ws = TestWorkspace::new_named("global-json-domains")?;
  let cases: &[(&str, &[&str])] = &[
    ("run", &["rail", "--json", "run", "--dry-run"]),
    ("unify undo", &["rail", "--json", "unify", "undo", "--list"]),
    ("split init", &["rail", "--json", "split", "init"]),
    ("release init", &["rail", "--json", "release", "init"]),
    ("release resume", &["rail", "--json", "release", "resume", "state.json"]),
    ("release abort", &["rail", "--json", "release", "abort", "state.json"]),
    ("graph --dot", &["rail", "--json", "graph", "--dot"]),
    ("completions", &["rail", "--json", "completions", "bash"]),
  ];

  for (command, args) in cases {
    let output = run_cargo_rail(&ws.path, args)?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
      output.status.code(),
      Some(2),
      "{command} should reject --json: {stderr}"
    );
    assert!(
      stderr.contains(&format!("--json is not supported by 'cargo rail {command}'")),
      "{command} should explain why --json was rejected: {stderr}"
    );
    assert!(
      output.stdout.is_empty(),
      "{command} should not write stdout on parse failure"
    );
  }

  Ok(())
}

#[test]
fn test_clean_check_json_has_stable_stream_and_exit_contract() -> Result<()> {
  let ws = TestWorkspace::new_named("clean-json-contract")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  let artifact = ws.path.join("target/cargo-rail/metadata.json");
  std::fs::create_dir_all(artifact.parent().expect("artifact has parent"))?;
  std::fs::write(&artifact, "{}")?;

  let output = run_cargo_rail(&ws.path, &["rail", "clean", "--cache", "--check", "--format", "json"])?;
  assert_eq!(output.status.code(), Some(1));
  assert!(output.stderr.is_empty(), "JSON check output must keep stderr empty");

  let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(value["schema_version"], 1);
  assert_eq!(value["command"], "clean");
  assert_eq!(value["mode"], "check");
  assert_eq!(value["result"], "pending_changes");
  assert_eq!(value["exit_code"], 1);
  assert_eq!(value["has_changes"], true);
  assert!(artifact.exists(), "check mode must not remove artifacts");

  Ok(())
}
