//! Integration tests for plan/apply flows and graph introspection.

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;
use tempfile::TempDir;

#[test]
fn test_graph_outputs_json_and_dot() -> Result<()> {
  let ws = TestWorkspace::new_named("graph-output")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}")?;
  ws.commit("Change lib-a")?;

  let json_out = run_cargo_rail(&ws.path, &["rail", "graph", "--since", "HEAD~1"])?;
  assert!(json_out.status.success(), "graph json should succeed");
  let json: serde_json::Value = serde_json::from_slice(&json_out.stdout)?;
  assert_eq!(json["schema_version"], serde_json::json!(1));
  assert_eq!(json["command"], serde_json::json!("graph"));
  assert_eq!(json["mode"], serde_json::json!("inspect"));
  assert_eq!(json["result"], serde_json::json!("success"));
  assert_eq!(json["exit_code"], serde_json::json!(0));
  assert!(json["nodes"].is_array());
  assert!(json["edges"].is_array());

  let dot_out = run_cargo_rail(&ws.path, &["rail", "graph", "--since", "HEAD~1", "--dot"])?;
  assert!(dot_out.status.success(), "graph dot should succeed");
  let dot = String::from_utf8_lossy(&dot_out.stdout);
  assert!(dot.contains("digraph rail_plan"));

  Ok(())
}

#[test]
fn test_split_apply_from_plan_file() -> Result<()> {
  let ws = TestWorkspace::new_named("split-plan-apply")?;
  ws.add_crate("mylib", "0.1.0", &[])?;
  ws.commit("Add mylib")?;

  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[crates.mylib.split]
remote = "{}"
branch = "main"
mode = "single"
paths = [{{ crate = "crates/mylib" }}]
"#,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  let check = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "split",
      "run",
      "mylib",
      "--check",
      "--allow-dirty",
      "-f",
      "json",
    ],
  )?;
  assert_eq!(check.status.code(), Some(1), "split check should exit 1");
  let plan_path = ws.path.join("split-plan.json");
  std::fs::write(&plan_path, &check.stdout)?;

  let apply = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "split",
      "run",
      "mylib",
      "--allow-dirty",
      "--plan",
      plan_path.to_string_lossy().as_ref(),
      "--yes",
    ],
  )?;
  assert!(
    apply.status.success(),
    "split apply --plan should succeed.\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&apply.stdout),
    String::from_utf8_lossy(&apply.stderr)
  );

  Ok(())
}

#[test]
fn test_sync_apply_from_plan_file() -> Result<()> {
  let ws = TestWorkspace::new_named("sync-plan-apply")?;
  ws.add_crate("mylib", "0.1.0", &[])?;
  ws.commit("Add mylib")?;

  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[crates.mylib.split]
remote = "{}"
branch = "main"
mode = "single"
paths = [{{ crate = "crates/mylib" }}]
"#,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  let split = run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;
  assert!(split.status.success(), "initial split should succeed");

  let check = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "sync",
      "mylib",
      "--to-remote",
      "--check",
      "--allow-dirty",
      "-f",
      "json",
    ],
  )?;
  assert_eq!(
    check.status.code(),
    Some(1),
    "sync --check -f json should exit 1 when pending changes are detected"
  );
  let plan_path = ws.path.join("sync-plan.json");
  std::fs::write(&plan_path, &check.stdout)?;

  let apply = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "sync",
      "mylib",
      "--to-remote",
      "--allow-dirty",
      "--plan",
      plan_path.to_string_lossy().as_ref(),
      "--yes",
    ],
  )?;
  assert!(
    apply.status.success(),
    "sync apply --plan should succeed.\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&apply.stdout),
    String::from_utf8_lossy(&apply.stderr)
  );

  Ok(())
}

#[test]
fn test_release_apply_from_plan_file() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("relplan", "0.1.0")?;
  ws.write_release_config(
    r#"tag_prefix = "v"
tag_format = "v{version}"
require_clean = false
"#,
  )?;

  let check = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--check",
      "--bump",
      "patch",
      "--skip-publish",
      "--skip-tag",
      "--json",
    ],
  )?;
  assert_eq!(check.status.code(), Some(1), "release check should report changes");
  let plan_path = ws.path.join("release-plan.json");
  std::fs::write(&plan_path, &check.stdout)?;

  let apply = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
      "--skip-tag",
      "--yes",
      "--plan",
      plan_path.to_string_lossy().as_ref(),
    ],
  )?;
  assert!(
    apply.status.success(),
    "release apply --plan should succeed.\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&apply.stdout),
    String::from_utf8_lossy(&apply.stderr)
  );

  Ok(())
}

#[test]
fn test_run_emits_decision_receipt() -> Result<()> {
  let ws = TestWorkspace::new_named("run-decision-receipt")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}")?;
  ws.commit("Change lib-a")?;

  let out = run_cargo_rail(&ws.path, &["rail", "run", "--since", "HEAD~1", "--dry-run"])?;
  assert!(out.status.success(), "run dry-run should succeed");

  let receipts_dir = ws.path.join("target/cargo-rail/receipts");
  let has_decision = std::fs::read_dir(&receipts_dir)?
    .filter_map(|entry| entry.ok().map(|e| e.path()))
    .any(|path| {
      path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with("run-decision-") && n.ends_with(".json"))
        .unwrap_or(false)
    });
  assert!(
    has_decision,
    "expected run decision receipt in {}",
    receipts_dir.display()
  );

  Ok(())
}
