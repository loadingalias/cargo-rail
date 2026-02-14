//! Integration tests for `cargo rail hash` and `cargo rail diff-hash`.

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;

#[test]
fn test_hash_json_is_deterministic_for_same_inputs() -> Result<()> {
  let ws = TestWorkspace::new_named("hash-deterministic")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  let out1 = run_cargo_rail(&ws.path, &["rail", "hash", "--since", "HEAD~1", "-f", "json"])?;
  let out2 = run_cargo_rail(&ws.path, &["rail", "hash", "--since", "HEAD~1", "-f", "json"])?;
  assert!(out1.status.success(), "first hash failed");
  assert!(out2.status.success(), "second hash failed");

  let j1: serde_json::Value = serde_json::from_slice(&out1.stdout)?;
  let j2: serde_json::Value = serde_json::from_slice(&out2.stdout)?;
  assert_eq!(j1["hash"], j2["hash"], "hash must be deterministic");

  Ok(())
}

#[test]
fn test_diff_hash_reports_changes_between_plan_contracts() -> Result<()> {
  let ws = TestWorkspace::new_named("diff-hash-changes")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn a() {}")?;
  ws.commit("Change a")?;
  let plan_a = ws.path.join("plan-a.json");
  let out_a = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "plan",
      "--since",
      "HEAD~1",
      "-f",
      "json",
      "-o",
      plan_a.to_string_lossy().as_ref(),
    ],
  )?;
  assert!(out_a.status.success(), "plan A generation failed");

  ws.modify_file("lib-a", "README.md", "# docs")?;
  ws.commit("Change docs")?;
  let plan_b = ws.path.join("plan-b.json");
  let out_b = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "plan",
      "--since",
      "HEAD~1",
      "-f",
      "json",
      "-o",
      plan_b.to_string_lossy().as_ref(),
    ],
  )?;
  assert!(out_b.status.success(), "plan B generation failed");

  let diff = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "diff-hash",
      plan_a.to_string_lossy().as_ref(),
      plan_b.to_string_lossy().as_ref(),
      "-f",
      "json",
    ],
  )?;
  assert!(diff.status.success(), "diff-hash failed");

  let json: serde_json::Value = serde_json::from_slice(&diff.stdout)?;
  assert_eq!(json["equal"], serde_json::Value::Bool(false));
  assert!(json["changes"].as_array().is_some_and(|c| !c.is_empty()));

  Ok(())
}
