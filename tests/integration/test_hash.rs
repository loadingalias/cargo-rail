//! Integration tests for `cargo rail hash` and `cargo rail diff-hash`.

use crate::helpers::{TestWorkspace, git, run_cargo_rail};
use anyhow::{Result, anyhow};
use std::process::Command;

#[test]
fn test_hash_json_is_deterministic_for_same_inputs() -> Result<()> {
  let ws = TestWorkspace::new_named("hash-deterministic")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  let lockfile = Command::new("cargo")
    .current_dir(&ws.path)
    .args(["generate-lockfile", "--offline"])
    .output()?;
  assert!(
    lockfile.status.success(),
    "offline lockfile generation failed: {}",
    String::from_utf8_lossy(&lockfile.stderr)
  );
  ws.commit("Add lib-a")?;

  let out1 = run_cargo_rail(&ws.path, &["rail", "hash", "--since", "HEAD~1", "-f", "json"])?;
  let out2 = run_cargo_rail(&ws.path, &["rail", "hash", "--since", "HEAD~1", "-f", "json"])?;
  assert!(out1.status.success(), "first hash failed");
  assert!(out2.status.success(), "second hash failed");

  let j1: serde_json::Value = serde_json::from_slice(&out1.stdout)?;
  let j2: serde_json::Value = serde_json::from_slice(&out2.stdout)?;
  assert_eq!(j1["hash"], j2["hash"], "hash must be deterministic");
  assert_eq!(j1["identity"], j1["hash"], "hash remains a compatibility alias");
  assert_eq!(j1["algorithm"], "sha256");
  assert_eq!(j1["portable"], true);
  assert_eq!(j1["cache_key"], false);

  Ok(())
}

#[test]
fn test_plan_identity_is_portable_across_clone_paths() -> Result<()> {
  let ws = TestWorkspace::new_named("hash-portable-source")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  std::fs::write(ws.path.join(".config/rail.toml"), "[unify]\ninclude_paths = false\n")?;
  ws.commit("Add lib-a")?;

  let clone_root = tempfile::tempdir()?;
  let clone_path = clone_root.path().join("different-checkout-root");
  let clone = Command::new("git")
    .args(["-c", "core.autocrlf=true"])
    .arg("clone")
    .arg(&ws.path)
    .arg(&clone_path)
    .output()?;
  if !clone.status.success() {
    return Err(anyhow!("git clone failed: {}", String::from_utf8_lossy(&clone.stderr)));
  }
  git(&clone_path, &["config", "core.autocrlf", "true"])?;
  let dirty = git(&clone_path, &["diff", "--name-only"])?;
  assert!(dirty.stdout.is_empty(), "line-ending checkout must remain Git-clean");
  assert_ne!(
    std::fs::read(ws.path.join(".config/rail.toml"))?,
    std::fs::read(clone_path.join(".config/rail.toml"))?,
    "fixture must exercise Git's platform line-ending conversion"
  );

  let source_hash = run_cargo_rail(&ws.path, &["rail", "hash", "--since", "HEAD~1", "--format", "json"])?;
  let clone_hash = run_cargo_rail(&clone_path, &["rail", "hash", "--since", "HEAD~1", "--format", "json"])?;
  assert!(source_hash.status.success() && clone_hash.status.success());

  let source_identity: serde_json::Value = serde_json::from_slice(&source_hash.stdout)?;
  let clone_identity: serde_json::Value = serde_json::from_slice(&clone_hash.stdout)?;
  assert!(
    source_identity["identity"]
      .as_str()
      .is_some_and(|identity| identity.starts_with("plan-v1:sha256:"))
  );

  let source_plan = ws.path.join("source-plan.json");
  let clone_plan = clone_path.join("clone-plan.json");
  let source_output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "plan",
      "--since",
      "HEAD~1",
      "--format",
      "json",
      "--output",
      source_plan.to_string_lossy().as_ref(),
    ],
  )?;
  let clone_output = run_cargo_rail(
    &clone_path,
    &[
      "rail",
      "plan",
      "--since",
      "HEAD~1",
      "--format",
      "json",
      "--output",
      clone_plan.to_string_lossy().as_ref(),
    ],
  )?;
  assert!(source_output.status.success() && clone_output.status.success());

  let source_json: serde_json::Value = serde_json::from_slice(&std::fs::read(&source_plan)?)?;
  let clone_json: serde_json::Value = serde_json::from_slice(&std::fs::read(&clone_plan)?)?;
  assert_ne!(
    source_json["inputs"]["workspace_root"],
    clone_json["inputs"]["workspace_root"]
  );

  let diff = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "diff-hash",
      source_plan.to_string_lossy().as_ref(),
      clone_plan.to_string_lossy().as_ref(),
      "--format",
      "json",
    ],
  )?;
  assert!(diff.status.success(), "portable diff-hash should succeed");
  let diff_json: serde_json::Value = serde_json::from_slice(&diff.stdout)?;
  assert_eq!(
    source_identity["identity"],
    clone_identity["identity"],
    "checkout location must not affect plan identity\nsource config: {}\nclone config: {}\nsource files: {:#}\nclone files: {:#}\nportable diff: {diff_json:#}",
    source_json["inputs"]["config_fingerprint"],
    clone_json["inputs"]["config_fingerprint"],
    source_json["files"],
    clone_json["files"]
  );
  assert_eq!(diff_json["equal"], true, "portable diff: {diff_json:#}");
  assert_eq!(diff_json["changes"], serde_json::json!([]));

  Ok(())
}

#[test]
fn test_diff_hash_rejects_non_repository_relative_plan_paths() -> Result<()> {
  let ws = TestWorkspace::new_named("hash-reject-absolute-path")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}")?;

  let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
  assert!(output.status.success());

  let mut plan: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  plan["files"][0]["path"] = serde_json::Value::String("/tmp/checkout/crates/lib-a/src/lib.rs".to_string());
  let path = ws.path.join("invalid-plan.json");
  std::fs::write(&path, serde_json::to_vec_pretty(&plan)?)?;

  let diff = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "diff-hash",
      path.to_string_lossy().as_ref(),
      path.to_string_lossy().as_ref(),
      "--format",
      "json",
    ],
  )?;
  assert_eq!(diff.status.code(), Some(2));
  assert!(diff.stderr.is_empty(), "JSON errors keep stderr empty");
  let error: serde_json::Value = serde_json::from_slice(&diff.stdout)?;
  assert_eq!(error["error"], true);
  assert_eq!(error["code"], 2);
  assert!(
    error["message"]
      .as_str()
      .is_some_and(|message| message.contains("is not repository-relative")),
    "identity must reject local absolute paths: {error}"
  );

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
