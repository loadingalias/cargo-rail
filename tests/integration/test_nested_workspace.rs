//! Integration tests for nested workspace layouts (git root != Cargo workspace root).

use crate::helpers::{NestedWorkspace, git, run_cargo_rail};
use anyhow::{Result, anyhow};
use serde_json::Value;

#[test]
fn test_plan_from_nested_workspace_strips_workspace_prefix() -> Result<()> {
  let ws = NestedWorkspace::new("rust")?;
  ws.add_crate("lib-a", "0.1.0")?;
  ws.commit("add lib-a")?;

  // Create a stable base ref from git root.
  git(&ws.git_root, &["branch", "origin/main"])?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() -> bool { true }")?;
  ws.commit("change lib-a src")?;

  // Run from cargo workspace root (nested under git root).
  let output = run_cargo_rail(
    &ws.workspace_root,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success(), "plan should succeed in nested workspace");

  let json: Value = serde_json::from_slice(&output.stdout)?;
  let files = json["files"]
    .as_array()
    .ok_or_else(|| anyhow!("files should be an array"))?;

  assert_eq!(
    files.len(),
    1,
    "expected one changed file: {files:#?}\n{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(
    files[0]["path"],
    Value::String("crates/lib-a/src/lib.rs".to_string()),
    "planner should emit workspace-relative path without nested prefix"
  );
  assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(true));

  Ok(())
}

#[test]
fn test_run_from_nested_workspace_uses_planner_selection() -> Result<()> {
  let ws = NestedWorkspace::new("rust")?;
  ws.add_crate("lib-a", "0.1.0")?;
  ws.commit("add lib-a")?;

  git(&ws.git_root, &["branch", "origin/main"])?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed_for_run() -> bool { true }")?;
  ws.commit("change lib-a src")?;

  let output = run_cargo_rail(
    &ws.workspace_root,
    &[
      "rail",
      "run",
      "--since",
      "origin/main",
      "--dry-run",
      "--print-cmd",
      "--explain",
    ],
  )?;
  assert!(output.status.success(), "run should succeed in nested workspace");

  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("direct crates: lib-a") || stdout.contains("why:"),
    "explain output should include planner summary context"
  );
  assert!(stdout.contains("test: "), "dry-run should print test command");

  Ok(())
}
