//! Integration tests for plan/apply flows and graph introspection.

use crate::helpers::{TestWorkspace, git, run_cargo_rail};
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
"#,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  let split = run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;
  assert!(split.status.success(), "initial split should succeed");

  std::fs::write(ws.path.join("crates/mylib/src/lib.rs"), "pub fn changed() {}\n")?;
  ws.commit("Change mylib after split")?;

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
  std::fs::create_dir_all(ws.path.join(".changes"))?;
  std::fs::write(
    ws.path.join(".changes/release-plan.md"),
    "---\n\"relplan\" = \"patch\"\n---\n\nExercise release apply from a reviewed plan.\n",
  )?;
  ws.commit("Configure release plan test")?;

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

  let apply_receipt = std::fs::read_dir(ws.path.join("target/cargo-rail/receipts"))?
    .filter_map(Result::ok)
    .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
    .filter_map(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
    .find(|receipt| receipt["operation"] == "release" && receipt["phase"] == "apply")
    .expect("release apply receipt");
  assert_eq!(apply_receipt["plan"]["contract_version"], 2);
  assert!(
    apply_receipt["verified_inputs"]["worktree_fingerprint"]
      .as_str()
      .is_some_and(|fingerprint| fingerprint.starts_with("git-object:"))
  );
  assert!(
    apply_receipt["applied_actions"]
      .as_array()
      .is_some_and(|actions| !actions.is_empty())
  );
  assert!(
    apply_receipt["resulting_objects"]
      .as_array()
      .is_some_and(|objects| objects.iter().any(|object| object["kind"] == "commit"))
  );

  Ok(())
}

#[test]
fn test_release_plan_rejects_unreviewed_file_before_mutation() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("release-authority", "0.1.0")?;
  ws.write_release_config(
    r#"tag_prefix = "v"
tag_format = "v{version}"
require_clean = false
"#,
  )?;
  let config_head = ws.commit("Configure release")?;

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
  assert_eq!(check.status.code(), Some(1));
  let plan_dir = TempDir::new()?;
  let plan_path = plan_dir.path().join("release-plan.json");
  std::fs::write(&plan_path, &check.stdout)?;

  let unreviewed = ws.path.join("unreviewed.txt");
  std::fs::write(&unreviewed, "must not enter the release commit\n")?;
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
    !apply.status.success(),
    "release must reject post-approval worktree drift"
  );
  let stderr = String::from_utf8_lossy(&apply.stderr);
  assert!(
    stderr.contains("worktree changed") && stderr.contains("unreviewed.txt"),
    "error must identify the unreviewed path\nstderr:\n{}",
    stderr
  );
  let head = git(&ws.path, &["rev-parse", "HEAD"])?;
  assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), config_head);
  assert!(
    unreviewed.exists(),
    "rejected input should be left untouched for recovery"
  );
  assert!(
    !String::from_utf8_lossy(&git(&ws.path, &["ls-tree", "-r", "--name-only", "HEAD"])?.stdout)
      .contains("unreviewed.txt"),
    "unreviewed file must never be committed"
  );

  Ok(())
}

#[test]
fn test_run_emits_decision_receipt() -> Result<()> {
  let ws = TestWorkspace::new_named("run-decision-receipt")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  let out = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--surface",
      "build",
      "--surface",
      "docs",
      "--dry-run",
    ],
  )?;
  assert!(out.status.success(), "run dry-run should succeed");

  let receipts_dir = ws.path.join("target/cargo-rail/receipts");
  let receipt_path = std::fs::read_dir(&receipts_dir)?
    .filter_map(|entry| entry.ok().map(|e| e.path()))
    .find(|path| {
      path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with("run-decision-") && n.ends_with(".json"))
        .unwrap_or(false)
    })
    .ok_or_else(|| anyhow::anyhow!("missing run decision receipt in {}", receipts_dir.display()))?;
  let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(receipt_path)?)?;

  assert_eq!(receipt["artifact"], "decision_receipt");
  assert_eq!(receipt["version"], 4);
  assert_eq!(receipt["inputs"]["execution_profile"], "normal");
  assert!(receipt["execution"]["fetch_action"].is_null());
  assert!(
    receipt["snapshot_id"]
      .as_str()
      .is_some_and(|id| id.starts_with("v1-sha256-")),
    "receipt must bind actions to the authoritative workspace snapshot"
  );
  let platform = receipt["actions"][0]["platform"]
    .as_str()
    .ok_or_else(|| anyhow::anyhow!("expanded action must record its platform"))?;
  let package_id = receipt["actions"][0]["resolution_views"][0]["root_package_ids"][0]
    .as_str()
    .ok_or_else(|| anyhow::anyhow!("expanded action must bind its exact root PackageId"))?;
  let resolution_digest = receipt["actions"][0]["resolution_views"][0]["resolution_digest"]
    .as_str()
    .ok_or_else(|| anyhow::anyhow!("expanded action must bind its portable resolution digest"))?;
  let build_action_key = receipt["actions"][0]["action_key"].clone();
  let docs_action_key = receipt["actions"][1]["action_key"].clone();
  assert_eq!(
    receipt["actions"],
    serde_json::json!([
      {
        "id": "build",
        "kind": "build",
        "argv": ["cargo", "check", "--workspace"],
        "working_directory": "workspace",
        "selected_packages": ["lib-a"],
        "dependencies": [],
        "reasons": [{ "kind": "all" }],
        "package_selector": "workspace_or_selected",
        "target_selector": "cargo_resolution",
        "selected_targets": [platform],
        "selected_features": { "all_features": false, "default_features": true, "named": [] },
        "resolution_views": [{
          "root_package_ids": [package_id],
          "target": platform,
          "features": { "all_features": false, "default_features": true, "named": [] },
          "resolution_digest": resolution_digest,
          "resolved_node_count": 1
        }],
        "platform": platform,
        "inputs": [{ "kind": "workspace_snapshot" }, { "kind": "ambient_host" }],
        "outputs": [{ "kind": "ambient_process" }],
        "environment": { "inherit": true, "entries": [] },
        "action_key": build_action_key
      },
      {
        "id": "docs",
        "kind": "docs",
        "argv": ["cargo", "doc", "--workspace", "--no-deps"],
        "working_directory": "workspace",
        "selected_packages": [],
        "dependencies": [],
        "reasons": [{ "kind": "all" }],
        "package_selector": "none",
        "target_selector": "cargo_resolution",
        "selected_targets": [platform],
        "selected_features": { "all_features": false, "default_features": true, "named": [] },
        "resolution_views": [{
          "root_package_ids": [package_id],
          "target": platform,
          "features": { "all_features": false, "default_features": true, "named": [] },
          "resolution_digest": resolution_digest,
          "resolved_node_count": 1
        }],
        "platform": platform,
        "inputs": [{ "kind": "workspace_snapshot" }, { "kind": "ambient_host" }],
        "outputs": [{ "kind": "ambient_process" }],
        "environment": { "inherit": true, "entries": [] },
        "action_key": docs_action_key
      }
    ]),
    "receipt must contain the exact ordered action expansion"
  );

  Ok(())
}

#[test]
fn test_run_decision_receipt_binds_actions_to_planner_trace_reasons() -> Result<()> {
  let ws = TestWorkspace::new_named("run-decision-receipt-planner-reasons")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add crates")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}")?;
  ws.commit("Change lib-a")?;

  let out = run_cargo_rail(
    &ws.path,
    &["rail", "run", "--since", "HEAD~1", "--surface", "build", "--dry-run"],
  )?;
  assert!(out.status.success(), "run dry-run should succeed");

  let receipts_dir = ws.path.join("target/cargo-rail/receipts");
  let receipt_path = std::fs::read_dir(&receipts_dir)?
    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
    .find(|path| {
      path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("run-decision-") && name.ends_with(".json"))
    })
    .ok_or_else(|| anyhow::anyhow!("missing run decision receipt in {}", receipts_dir.display()))?;
  let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(receipt_path)?)?;

  assert_eq!(
    receipt["actions"][0]["argv"],
    serde_json::json!(["cargo", "check", "-p", "lib-a"])
  );
  let action_reasons = receipt["actions"][0]["reasons"]
    .as_array()
    .ok_or_else(|| anyhow::anyhow!("action reasons must be an array"))?;
  assert!(
    !action_reasons.is_empty(),
    "planner-enabled action must retain its reasons"
  );
  let trace_ids = receipt["plan"]["trace"]
    .as_array()
    .ok_or_else(|| anyhow::anyhow!("plan trace must be an array"))?
    .iter()
    .filter_map(|reason| reason["id"].as_u64())
    .collect::<std::collections::BTreeSet<_>>();
  for reason in action_reasons {
    assert_eq!(reason["kind"], "planner");
    assert_eq!(reason["surface"], "build");
    let trace_id = reason["trace_id"]
      .as_u64()
      .ok_or_else(|| anyhow::anyhow!("planner action reason must contain a numeric trace_id"))?;
    assert!(
      trace_ids.contains(&trace_id),
      "expanded action reason {trace_id} must refer to the embedded planner trace"
    );
  }

  Ok(())
}
