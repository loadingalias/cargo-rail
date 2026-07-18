use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, ensure};
use tempfile::TempDir;

use crate::helpers::{TestWorkspace, run_cargo_rail};

fn read_counters(path: &Path) -> Result<serde_json::Value> {
  let bytes = std::fs::read(path).with_context(|| format!("reading diagnostics from {}", path.display()))?;
  Ok(serde_json::from_slice(&bytes)?)
}

fn host_target() -> Result<String> {
  let output = Command::new("rustc").arg("-vV").output()?;
  ensure!(output.status.success(), "rustc -vV failed");
  String::from_utf8(output.stdout)?
    .lines()
    .find_map(|line| line.strip_prefix("host: ").map(str::to_owned))
    .context("rustc -vV did not report a host target")
}

#[test]
fn plan_diagnostics_are_out_of_band_and_count_real_boundaries() -> Result<()> {
  let ws = TestWorkspace::new_named("diagnostic-plan")?;
  ws.add_crate("member-a", "0.1.0", &[])?;
  ws.add_crate("member-b", "0.1.0", &[("member-a", "{ path = \"../member-a\" }")])?;
  let lockfile = Command::new("cargo")
    .current_dir(&ws.path)
    .args(["generate-lockfile", "--offline"])
    .output()?;
  ensure!(
    lockfile.status.success(),
    "offline lockfile generation failed: {}",
    String::from_utf8_lossy(&lockfile.stderr)
  );
  ws.commit("Add members")?;
  ws.modify_file("member-a", "src/lib.rs", "pub fn changed() {}\n")?;
  ws.commit("Change member-a")?;

  let args = ["rail", "plan", "--since", "HEAD~1", "--format", "json"];
  let expected = run_cargo_rail(&ws.path, &args)?;
  ensure!(expected.status.success(), "warm-up plan failed");

  let output_dir = TempDir::new()?;
  let diagnostics = output_dir.path().join("plan.json");
  let measured = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "--diagnostics-file",
      diagnostics.to_str().context("non-UTF-8 diagnostics path")?,
      "plan",
      "--since",
      "HEAD~1",
      "--format",
      "json",
    ],
  )?;

  assert_eq!(measured.status, expected.status);
  assert_eq!(measured.stdout, expected.stdout, "diagnostics changed normal stdout");
  assert_eq!(measured.stderr, expected.stderr, "diagnostics changed normal stderr");

  let counters = read_counters(&diagnostics)?;
  assert_eq!(counters["schema_version"], 2);
  assert!(
    counters["snapshot_id"]
      .as_str()
      .is_some_and(|identity| identity.starts_with("v1-sha256-")),
    "current plan commands must expose one versioned authoritative identity"
  );
  assert_eq!(counters["cargo_metadata_loads"], 1);
  assert_eq!(counters["cargo_metadata_cache_hits"], 0);
  assert_eq!(counters["target_view_loads"], 0);
  assert!(counters["hash_operations"].as_u64().is_some_and(|count| count >= 3));
  assert!(counters["hash_input_bytes"].as_u64().is_some_and(|bytes| bytes > 0));
  assert!(
    counters["hashed_file_bytes_read"]
      .as_u64()
      .is_some_and(|bytes| bytes > 0)
  );
  assert_eq!(counters["git_subprocesses"], 11);
  assert_eq!(counters["graph_traversals"], 2);
  assert!(counters["graph_node_visits"].as_u64().is_some_and(|count| count >= 4));
  assert!(counters["graph_edge_visits"].as_u64().is_some_and(|count| count >= 2));
  Ok(())
}

#[test]
fn unchanged_plan_and_run_share_snapshot_without_native_or_target_reloads() -> Result<()> {
  let ws = TestWorkspace::new_named("diagnostic-shared-snapshot")?;
  ws.add_crate("member", "0.1.0", &[])?;
  let lockfile = Command::new("cargo")
    .current_dir(&ws.path)
    .args(["generate-lockfile", "--offline"])
    .output()?;
  ensure!(lockfile.status.success(), "offline lockfile generation failed");
  ws.commit("Add shared snapshot fixture")?;

  let output_dir = TempDir::new()?;
  let plan_diagnostics = output_dir.path().join("plan.json");
  let run_diagnostics = output_dir.path().join("run.json");
  let plan = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "--diagnostics-file",
      plan_diagnostics.to_str().context("non-UTF-8 plan diagnostics path")?,
      "plan",
      "--since",
      "HEAD",
      "--format",
      "json",
    ],
  )?;
  ensure!(plan.status.success(), "plan snapshot fixture failed");
  let run = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "--diagnostics-file",
      run_diagnostics.to_str().context("non-UTF-8 run diagnostics path")?,
      "run",
      "--all",
      "--surface",
      "build",
      "--dry-run",
    ],
  )?;
  ensure!(run.status.success(), "run snapshot fixture failed");

  let plan = read_counters(&plan_diagnostics)?;
  let run = read_counters(&run_diagnostics)?;
  assert_eq!(plan["snapshot_id"], run["snapshot_id"]);
  for counters in [&plan, &run] {
    assert_eq!(counters["cargo_metadata_loads"], 1);
    assert_eq!(counters["cargo_metadata_cache_hits"], 0);
    assert_eq!(counters["target_view_loads"], 0);
  }
  Ok(())
}

#[test]
fn snapshot_identity_records_credential_capability_not_raw_token_material() -> Result<()> {
  let ws = TestWorkspace::new_named("diagnostic-credential-capability")?;
  ws.add_crate("member", "0.1.0", &[])?;
  let cargo_home = TempDir::new()?;
  let cargo_config_dir = ws.path.join(".cargo");
  std::fs::create_dir_all(&cargo_config_dir)?;
  let credential_config = cargo_config_dir.join("config.toml");
  std::fs::write(
    &credential_config,
    "[registries.private]\ntoken = \"first-private-token\"\n",
  )?;
  let lockfile = Command::new("cargo")
    .current_dir(&ws.path)
    .args(["generate-lockfile", "--offline"])
    .output()?;
  ensure!(lockfile.status.success(), "offline lockfile generation failed");
  ws.commit("Add credential capability fixture")?;

  let output_dir = TempDir::new()?;
  let first_diagnostics = output_dir.path().join("first.json");
  let second_diagnostics = output_dir.path().join("second.json");
  let run = |diagnostics: &Path| -> Result<std::process::Output> {
    Ok(
      Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
        .current_dir(&ws.path)
        .env("CARGO_HOME", cargo_home.path())
        .args([
          "rail",
          "--diagnostics-file",
          diagnostics.to_str().context("non-UTF-8 diagnostics path")?,
          "plan",
          "--since",
          "HEAD",
          "--format",
          "json",
        ])
        .output()?,
    )
  };

  let first = run(&first_diagnostics)?;
  ensure!(
    first.status.success(),
    "first capability plan failed: stdout={} stderr={}",
    String::from_utf8_lossy(&first.stdout),
    String::from_utf8_lossy(&first.stderr)
  );
  std::fs::write(
    &credential_config,
    "[registries.private]\ntoken = \"different-private-token\"\n",
  )?;
  let second = run(&second_diagnostics)?;
  ensure!(
    second.status.success(),
    "second capability plan failed: stdout={} stderr={}",
    String::from_utf8_lossy(&second.stdout),
    String::from_utf8_lossy(&second.stderr)
  );

  let first_counters = read_counters(&first_diagnostics)?;
  let second_counters = read_counters(&second_diagnostics)?;
  assert_eq!(first_counters["snapshot_id"], second_counters["snapshot_id"]);
  for bytes in [
    first.stdout,
    first.stderr,
    second.stdout,
    second.stderr,
    std::fs::read(first_diagnostics)?,
    std::fs::read(second_diagnostics)?,
  ] {
    let rendered = String::from_utf8_lossy(&bytes);
    assert!(!rendered.contains("private-token"), "raw credential escaped capture");
  }
  Ok(())
}

#[test]
fn unify_diagnostics_distinguish_base_and_target_metadata_loads() -> Result<()> {
  let ws = TestWorkspace::new_named("diagnostic-target-metadata")?;
  ws.add_crate("demo", "0.1.0", &[])?;
  std::fs::write(
    ws.path.join(".config/rail.toml"),
    format!("targets = [\"{}\"]\n\n[unify]\nmsrv = false\n", host_target()?),
  )?;
  ws.commit("Add target fixture")?;

  let output_dir = TempDir::new()?;
  let diagnostics = output_dir.path().join("unify.json");
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "--diagnostics-file",
      diagnostics.to_str().context("non-UTF-8 diagnostics path")?,
      "unify",
      "--check",
      "--format",
      "json",
    ],
  )?;
  ensure!(
    output.status.success() || output.status.code() == Some(1),
    "unify diagnostics failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  let counters = read_counters(&diagnostics)?;
  assert_eq!(counters["cargo_metadata_loads"], 2);
  assert_eq!(counters["cargo_metadata_cache_hits"], 0);
  assert_eq!(counters["target_view_loads"], 1);
  Ok(())
}

#[test]
fn diagnostics_refuse_to_replace_existing_files_before_dispatch() -> Result<()> {
  let root = TempDir::new()?;
  let diagnostics = root.path().join("existing.json");
  std::fs::write(&diagnostics, "keep\n")?;

  let output = run_cargo_rail(
    root.path(),
    &[
      "rail",
      "--diagnostics-file",
      diagnostics.to_str().context("non-UTF-8 diagnostics path")?,
      "plan",
      "--schema",
    ],
  )?;

  assert_eq!(output.status.code(), Some(2));
  assert!(String::from_utf8_lossy(&output.stderr).contains("failed to reserve diagnostic counter file"));
  assert_eq!(std::fs::read_to_string(diagnostics)?, "keep\n");
  Ok(())
}
