use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, ensure};
use tempfile::TempDir;

use crate::helpers::{TestWorkspace, run_cargo_rail};

#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) -> Result<()> {
  use std::os::unix::fs::PermissionsExt as _;

  std::fs::write(path, contents)?;
  let mut permissions = std::fs::metadata(path)?.permissions();
  permissions.set_mode(0o755);
  std::fs::set_permissions(path, permissions)?;
  Ok(())
}

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
  assert_eq!(counters["schema_version"], 10);
  assert_eq!(counters["phases"]["cli_pre_context_preparation"]["invocations"], 1);
  assert!(
    counters["phases"]["cli_pre_context_preparation"]["elapsed_ns"]
      .as_u64()
      .is_some_and(|elapsed| elapsed > 0)
  );
  assert_eq!(counters["phases"]["workspace_capture_cargo_metadata"]["invocations"], 1);
  assert!(
    counters["phases"]["workspace_capture_cargo_metadata"]["elapsed_ns"]
      .as_u64()
      .is_some_and(|elapsed| elapsed > 0)
  );
  for phase in [
    "action_expansion_key_construction",
    "native_cache_setup",
    "sysroot_fingerprinting",
    "cargo_child_execution",
    "cache_report_collection",
  ] {
    assert_eq!(counters["phases"][phase]["invocations"], 0, "unexpected {phase} phase");
    assert_eq!(counters["phases"][phase]["elapsed_ns"], 0, "unexpected {phase} time");
  }
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
  assert_eq!(counters["graph_traversals"], 1);
  assert!(counters["graph_node_visits"].as_u64().is_some_and(|count| count >= 2));
  assert!(counters["graph_edge_visits"].as_u64().is_some_and(|count| count >= 1));
  Ok(())
}

#[test]
fn split_diagnostics_prove_bounded_git_object_streams() -> Result<()> {
  let ws = TestWorkspace::new_named("diagnostic-split-streams")?;
  ws.add_crate("streamed", "0.1.0", &[])?;
  ws.commit("Add streamed crate")?;
  for revision in 1..=8 {
    ws.modify_file(
      "streamed",
      "src/lib.rs",
      &format!("pub const REVISION: u8 = {revision};\n"),
    )?;
    ws.commit(&format!("Streamed revision {revision}"))?;
  }
  let target = TempDir::new()?;
  std::fs::write(
    ws.path.join("rail.toml"),
    format!(
      "[workspace]\nroot = \".\"\n\n[crates.streamed.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
      target.path().display().to_string().replace('\\', "\\\\")
    ),
  )?;
  let output = TempDir::new()?;
  let diagnostics = output.path().join("split.json");
  let measured = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "--diagnostics-file",
      diagnostics.to_str().context("non-UTF-8 diagnostics path")?,
      "split",
      "run",
      "streamed",
      "--yes",
      "--allow-dirty",
    ],
  )?;
  ensure!(
    measured.status.success(),
    "measured split failed: {}",
    String::from_utf8_lossy(&measured.stderr)
  );
  let counters = read_counters(&diagnostics)?;
  let objects = counters["git_object_reads"].as_u64().context("missing object count")?;
  let batches = counters["git_object_read_batches"]
    .as_u64()
    .context("missing object batch count")?;
  let subprocesses = counters["git_subprocesses"]
    .as_u64()
    .context("missing Git subprocess count")?;
  ensure!(objects > 0 && batches > 0);
  assert!(
    batches < objects,
    "{objects} object reads used {batches} batches; per-object subprocess behavior regressed"
  );
  assert!(objects <= 24, "bounded split object-read baseline regressed: {objects}");
  assert!(batches <= 4, "bounded split stream baseline regressed: {batches}");
  assert!(
    subprocesses <= 170,
    "bounded split Git subprocess baseline regressed: {subprocesses}"
  );
  eprintln!("P5 split measurement: git_subprocesses={subprocesses}, object_reads={objects}, object_batches={batches}");
  Ok(())
}

#[test]
fn unchanged_plan_skips_impact_resolution_while_run_binds_one_exact_host_view() -> Result<()> {
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
  assert_eq!(plan["cargo_metadata_loads"], 1);
  assert_eq!(plan["cargo_metadata_cache_hits"], 0);
  assert_eq!(plan["target_view_loads"], 0);
  assert_eq!(run["cargo_metadata_loads"], 2);
  assert_eq!(run["cargo_metadata_cache_hits"], 0);
  assert_eq!(run["target_view_loads"], 1);
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

#[test]
fn pre_context_diagnostics_have_one_fixed_phase_schema() -> Result<()> {
  let output = TempDir::new()?;
  let diagnostics = output.path().join("schema.json");
  let measured = run_cargo_rail(
    output.path(),
    &[
      "rail",
      "--diagnostics-file",
      diagnostics.to_str().context("non-UTF-8 diagnostics path")?,
      "plan",
      "--schema",
    ],
  )?;
  ensure!(measured.status.success(), "schema output failed");

  let counters = read_counters(&diagnostics)?;
  assert_eq!(counters["schema_version"], 10);
  assert_eq!(counters["phases"]["cli_pre_context_preparation"]["invocations"], 1);
  assert_eq!(counters["phases"]["workspace_capture_cargo_metadata"]["invocations"], 0);
  assert_eq!(
    counters["phases"].as_object().map(serde_json::Map::len),
    Some(7),
    "phase keys are a versioned fixed contract"
  );
  Ok(())
}

#[test]
fn active_cargo_profile_delegation_skips_snapshot_work_and_retains_a_receipt() -> Result<()> {
  let ws = TestWorkspace::new_named("diagnostic-active-cargo-delegation")?;
  ws.add_crate("member", "0.1.0", &[])?;
  ws.commit("Add active Cargo fixture")?;
  let cargo_home = TempDir::new()?;
  let cache = TempDir::new()?;
  let seed = Command::new("cargo")
    .current_dir(&ws.path)
    .args(["check", "--workspace", "--quiet"])
    .env("CARGO_HOME", cargo_home.path())
    .env_remove("CARGO_INCREMENTAL")
    .env_remove("RUSTC_FORCE_INCREMENTAL")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  ensure!(
    seed.status.success(),
    "active Cargo seed failed: {}",
    String::from_utf8_lossy(&seed.stderr)
  );

  let output = TempDir::new()?;
  let diagnostics = output.path().join("active.json");
  let measured = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&ws.path)
    .args([
      "rail",
      "--diagnostics-file",
      diagnostics.to_str().context("non-UTF-8 diagnostics path")?,
      "run",
      "--all",
      "--action",
      "build",
      "--",
      "--quiet",
    ])
    .env("CARGO_HOME", cargo_home.path())
    .env("CARGO_RAIL_CACHE_DIR", cache.path())
    .env_remove("CARGO_INCREMENTAL")
    .env_remove("RUSTC_FORCE_INCREMENTAL")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  ensure!(
    measured.status.success(),
    "active Cargo delegation failed: {}",
    String::from_utf8_lossy(&measured.stderr)
  );
  assert!(
    String::from_utf8_lossy(&measured.stderr)
      .contains("native compiler cache: bypassed reason=active_cargo_profile_preferred"),
    "default text mode should emit one concise cache decision: {}",
    String::from_utf8_lossy(&measured.stderr)
  );

  let counters = read_counters(&diagnostics)?;
  assert_eq!(counters["phases"]["cli_pre_context_preparation"]["invocations"], 1);
  assert_eq!(counters["phases"]["workspace_capture_cargo_metadata"]["invocations"], 0);
  assert_eq!(
    counters["phases"]["action_expansion_key_construction"]["invocations"],
    0
  );
  assert_eq!(counters["phases"]["native_cache_setup"]["invocations"], 0);
  assert_eq!(counters["phases"]["cargo_child_execution"]["invocations"], 1);
  assert_eq!(counters["cargo_metadata_loads"], 0);
  assert_eq!(counters["git_subprocesses"], 0);
  assert_eq!(
    counters["hash_operations"], 1,
    "only the sanitized Cargo configuration is hashed"
  );

  let receipts = std::fs::read_dir(ws.path.join("target/cargo-rail/receipts"))?.collect::<std::io::Result<Vec<_>>>()?;
  assert_eq!(receipts.len(), 1);
  let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(receipts[0].path())?)?;
  assert!(receipt["snapshot_id"].is_null());
  assert_eq!(
    receipt["snapshot_status"],
    "not_loaded_for_active_cargo_profile_delegation"
  );
  assert_eq!(
    receipt["execution"]["execution_mode"],
    "active_cargo_profile_delegation"
  );
  assert_eq!(
    receipt["execution"]["native_cache_reason"],
    "active_cargo_profile_preferred"
  );
  assert_eq!(
    receipt["actions"][0]["argv"],
    serde_json::json!(["cargo", "check", "--workspace", "--quiet"])
  );

  let release_seed = Command::new("cargo")
    .current_dir(&ws.path)
    .args(["build", "--workspace", "--release", "--locked", "--quiet"])
    .env("CARGO_HOME", cargo_home.path())
    .env_remove("CARGO_INCREMENTAL")
    .env_remove("RUSTC_FORCE_INCREMENTAL")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  ensure!(
    release_seed.status.success(),
    "active release seed failed: {}",
    String::from_utf8_lossy(&release_seed.stderr)
  );
  let distribution_diagnostics = output.path().join("active-distribution.json");
  let distribution = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&ws.path)
    .args([
      "rail",
      "--diagnostics-file",
      distribution_diagnostics
        .to_str()
        .context("non-UTF-8 distribution diagnostics path")?,
      "run",
      "--all",
      "--action",
      "distribution",
      "--",
      "--quiet",
    ])
    .env("CARGO_HOME", cargo_home.path())
    .env("CARGO_RAIL_CACHE_DIR", cache.path())
    .env_remove("CARGO_INCREMENTAL")
    .env_remove("RUSTC_FORCE_INCREMENTAL")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  ensure!(
    distribution.status.success(),
    "active distribution delegation failed: {}",
    String::from_utf8_lossy(&distribution.stderr)
  );
  let distribution_counters = read_counters(&distribution_diagnostics)?;
  assert_eq!(
    distribution_counters["phases"]["workspace_capture_cargo_metadata"]["invocations"],
    0
  );
  assert_eq!(
    distribution_counters["phases"]["action_expansion_key_construction"]["invocations"],
    0
  );
  assert_eq!(
    distribution_counters["phases"]["cargo_child_execution"]["invocations"],
    1
  );
  let distribution_receipt = std::fs::read_dir(ws.path.join("target/cargo-rail/receipts"))?
    .map(|entry| -> Result<Option<serde_json::Value>> {
      let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(entry?.path())?)?;
      Ok((receipt["actions"][0]["id"] == "distribution").then_some(receipt))
    })
    .collect::<Result<Vec<_>>>()?
    .into_iter()
    .flatten()
    .next()
    .context("missing distribution decision receipt")?;
  assert_eq!(
    distribution_receipt["actions"][0]["argv"],
    serde_json::json!(["cargo", "build", "--workspace", "--release", "--locked", "--quiet"])
  );

  let captured_diagnostics = output.path().join("explicit-nonincremental.json");
  let captured = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&ws.path)
    .args([
      "rail",
      "--diagnostics-file",
      captured_diagnostics
        .to_str()
        .context("non-UTF-8 captured diagnostics path")?,
      "run",
      "--all",
      "--action",
      "build",
      "--",
      "--quiet",
    ])
    .env("CARGO_HOME", cargo_home.path())
    .env("CARGO_RAIL_CACHE_DIR", cache.path())
    .env("CARGO_INCREMENTAL", "0")
    .env_remove("RUSTC_FORCE_INCREMENTAL")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  ensure!(
    captured.status.success(),
    "explicit non-incremental run failed: {}",
    String::from_utf8_lossy(&captured.stderr)
  );
  let captured = read_counters(&captured_diagnostics)?;
  assert_eq!(
    captured["phases"]["workspace_capture_cargo_metadata"]["invocations"], 1,
    "an explicit non-incremental request must retain the captured cache path"
  );
  Ok(())
}

#[cfg(unix)]
#[test]
fn exact_cache_bypasses_skip_snapshot_work_and_keep_private_controls_out_of_sccache() -> Result<()> {
  let ws = TestWorkspace::new_named("diagnostic-cache-pass-through")?;
  ws.add_crate("member", "0.1.0", &[])?;
  let lockfile = Command::new("cargo")
    .current_dir(&ws.path)
    .args(["generate-lockfile", "--offline"])
    .output()?;
  ensure!(
    lockfile.status.success(),
    "offline lockfile generation failed: {lockfile:?}"
  );
  let tools = ws.path.join("tools");
  std::fs::create_dir_all(&tools)?;
  let sccache = tools.join("sccache");
  write_executable(
    &sccache,
    r#"#!/bin/sh
if [ "${CARGO_RAIL_CACHE_DIR+x}" = x ] \
  || [ "${CARGO_RAIL_CACHE_MAX_BYTES+x}" = x ] \
  || [ "${CARGO_RAIL_CACHE_TRUST_DOMAIN+x}" = x ] \
  || [ "${CARGO_RAIL_CACHE_TARGETS_FILE+x}" = x ]; then
  printf 'leaked\n' >> "$WRAPPER_LOG"
else
  printf 'clean\n' >> "$WRAPPER_LOG"
fi
exec "$@"
"#,
  )?;
  ws.commit("Add cache pass-through fixture")?;

  let cargo_home = TempDir::new()?;
  let cache = TempDir::new()?;
  let output = TempDir::new()?;
  let wrapper_log = output.path().join("sccache.log");
  let diagnostics = output.path().join("sccache.json");
  let measured = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&ws.path)
    .args([
      "rail",
      "--diagnostics-file",
      diagnostics.to_str().context("non-UTF-8 diagnostics path")?,
      "run",
      "--all",
      "--action",
      "build",
      "--",
      "--quiet",
      "--locked",
      "--offline",
    ])
    .env("CARGO_HOME", cargo_home.path())
    .env("CARGO_INCREMENTAL", "0")
    .env("RUSTC_WRAPPER", &sccache)
    .env("WRAPPER_LOG", &wrapper_log)
    .env("CARGO_RAIL_CACHE_DIR", cache.path())
    .env("CARGO_RAIL_CACHE_MAX_BYTES", "1048576")
    .env("CARGO_RAIL_CACHE_TRUST_DOMAIN", "0".repeat(64))
    .env(
      "CARGO_RAIL_CACHE_TARGETS_FILE",
      output.path().join("unused-cache-targets.json"),
    )
    .env_remove("RUSTC_FORCE_INCREMENTAL")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  ensure!(
    measured.status.success(),
    "sccache pass-through failed: {}",
    String::from_utf8_lossy(&measured.stderr)
  );
  assert!(
    String::from_utf8_lossy(&measured.stderr)
      .contains("native compiler cache: bypassed reason=sccache_wrapper_preserved")
  );
  let wrapper_observations = std::fs::read_to_string(&wrapper_log)?;
  assert!(
    !wrapper_observations.is_empty(),
    "Cargo never invoked the preserved wrapper"
  );
  assert!(
    wrapper_observations.lines().all(|observation| observation == "clean"),
    "cargo-rail private controls reached the preserved wrapper: {wrapper_observations}"
  );
  assert!(
    !cache.path().join("cargo-rail").exists(),
    "a pass-through action must not initialize cargo-rail's CAS"
  );

  let counters = read_counters(&diagnostics)?;
  assert_eq!(counters["phases"]["workspace_capture_cargo_metadata"]["invocations"], 0);
  assert_eq!(
    counters["phases"]["action_expansion_key_construction"]["invocations"],
    0
  );
  assert_eq!(counters["phases"]["native_cache_setup"]["invocations"], 0);
  assert_eq!(counters["phases"]["cargo_child_execution"]["invocations"], 1);
  assert_eq!(counters["cargo_metadata_loads"], 0);

  let receipts = std::fs::read_dir(ws.path.join("target/cargo-rail/receipts"))?
    .map(|entry| -> Result<serde_json::Value> { Ok(serde_json::from_slice(&std::fs::read(entry?.path())?)?) })
    .collect::<Result<Vec<_>>>()?;
  assert_eq!(receipts.len(), 1);
  assert_eq!(
    receipts[0]["snapshot_status"],
    "not_loaded_for_exact_cargo_pass_through"
  );
  assert_eq!(receipts[0]["execution"]["execution_mode"], "exact_cargo_pass_through");
  assert_eq!(
    receipts[0]["execution"]["native_cache_reason"],
    "sccache_wrapper_preserved"
  );

  let incremental_diagnostics = output.path().join("incremental.json");
  let incremental = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&ws.path)
    .args([
      "rail",
      "--diagnostics-file",
      incremental_diagnostics
        .to_str()
        .context("non-UTF-8 incremental diagnostics path")?,
      "run",
      "--all",
      "--action",
      "build",
      "--",
      "--quiet",
      "--locked",
      "--offline",
    ])
    .env("CARGO_HOME", cargo_home.path())
    .env("CARGO_INCREMENTAL", "1")
    .env("CARGO_RAIL_CACHE_DIR", cache.path())
    .env_remove("RUSTC_FORCE_INCREMENTAL")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  ensure!(
    incremental.status.success(),
    "incremental pass-through failed: {}",
    String::from_utf8_lossy(&incremental.stderr)
  );
  assert!(
    String::from_utf8_lossy(&incremental.stderr)
      .contains("native compiler cache: bypassed reason=explicit_incremental_compilation_preserved")
  );
  let counters = read_counters(&incremental_diagnostics)?;
  assert_eq!(counters["phases"]["workspace_capture_cargo_metadata"]["invocations"], 0);
  assert_eq!(
    counters["phases"]["action_expansion_key_construction"]["invocations"],
    0
  );
  assert_eq!(counters["phases"]["native_cache_setup"]["invocations"], 0);
  assert_eq!(counters["phases"]["cargo_child_execution"]["invocations"], 1);
  assert_eq!(counters["cargo_metadata_loads"], 0);
  Ok(())
}

#[test]
fn clean_native_cache_execution_skips_planner_acquisition_and_retains_a_receipt() -> Result<()> {
  let ws = TestWorkspace::new_named("diagnostic-clean-native-cache")?;
  ws.add_crate("dependency", "0.1.0", &[])?;
  ws.add_crate("consumer", "0.1.0", &[("dependency", "{ path = \"../dependency\" }")])?;
  let lockfile = Command::new("cargo")
    .current_dir(&ws.path)
    .args(["generate-lockfile", "--offline"])
    .output()?;
  ensure!(
    lockfile.status.success(),
    "offline lockfile generation failed: {}",
    String::from_utf8_lossy(&lockfile.stderr)
  );
  ws.commit("Add clean native-cache fixture")?;
  let cargo_home = TempDir::new()?;
  let cache = TempDir::new()?;

  let run = |diagnostics: &Path| -> Result<std::process::Output> {
    Ok(
      Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
        .current_dir(&ws.path)
        .args([
          "rail",
          "--diagnostics-file",
          diagnostics.to_str().context("non-UTF-8 diagnostics path")?,
          "run",
          "--all",
          "--action",
          "build",
          "--",
          "--quiet",
        ])
        .env("CARGO_HOME", cargo_home.path())
        .env("CARGO_RAIL_CACHE_DIR", cache.path())
        .env_remove("CARGO_INCREMENTAL")
        .env_remove("RUSTC_FORCE_INCREMENTAL")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
        .output()?,
    )
  };

  let output = TempDir::new()?;
  let cold_diagnostics = output.path().join("cold.json");
  let cold = run(&cold_diagnostics)?;
  ensure!(
    cold.status.success(),
    "clean native-cache seed failed: {}",
    String::from_utf8_lossy(&cold.stderr)
  );
  let cold_counters = read_counters(&cold_diagnostics)?;
  let cold_event = cold_counters["native_cache_wrapper"]["events"]
    .as_array()
    .and_then(|events| events.iter().find(|event| event["outcome"] == "miss"))
    .context("cold diagnostics did not retain a native-cache miss event")?;
  assert!(
    cold_event["reason"]
      .as_str()
      .is_some_and(|reason| reason.starts_with("empty_local_authority;stored_verified_result")),
    "an empty native authority must use discovery-only publication: {cold_event}"
  );
  assert_eq!(
    cold_counters["native_cache_wrapper"]["phases"]["action_lookup"]["invocations"],
    0
  );
  assert_eq!(
    cold_counters["native_cache_wrapper"]["phases"]["cas_open"]["invocations"],
    0
  );
  let cold_phases = cold_event["trace"]["phases"]
    .as_array()
    .context("native-cache miss event has no phase trace")?;
  for phase in ["cold_result_preparation", "cold_result_handoff"] {
    assert!(
      cold_phases.iter().any(|trace| trace["phase"] == phase),
      "native-cache miss trace omitted {phase}: {cold_event}"
    );
  }
  let clean = Command::new("cargo").current_dir(&ws.path).arg("clean").output()?;
  ensure!(clean.status.success(), "cargo clean failed: {clean:?}");

  let warm_diagnostics = output.path().join("warm.json");
  let warm = run(&warm_diagnostics)?;
  ensure!(
    warm.status.success(),
    "clean native-cache reuse failed: {}",
    String::from_utf8_lossy(&warm.stderr)
  );
  let stderr = String::from_utf8_lossy(&warm.stderr);
  assert!(
    stderr.contains("native compiler cache: hits=") && !stderr.contains("hits=0 "),
    "the clean fast path must reuse a verified compiler result: {stderr}"
  );

  let counters = read_counters(&warm_diagnostics)?;
  assert_eq!(counters["phases"]["workspace_capture_cargo_metadata"]["invocations"], 0);
  assert_eq!(
    counters["phases"]["action_expansion_key_construction"]["invocations"],
    0
  );
  assert_eq!(counters["phases"]["native_cache_setup"]["invocations"], 1);
  assert_eq!(counters["phases"]["cargo_child_execution"]["invocations"], 1);
  assert_eq!(counters["phases"]["cache_report_collection"]["invocations"], 1);
  assert_eq!(counters["cargo_metadata_loads"], 0);
  assert_eq!(counters["git_subprocesses"], 0);
  let wrapper = &counters["native_cache_wrapper"];
  assert_eq!(wrapper["phases"].as_object().map(serde_json::Map::len), Some(13));
  assert!(
    wrapper["process"]["invocations"]
      .as_u64()
      .is_some_and(|count| count > 0)
  );
  assert!(
    wrapper["phases"]["context_load"]["invocations"]
      .as_u64()
      .is_some_and(|count| count > 0)
  );
  let hit = wrapper["events"]
    .as_array()
    .and_then(|events| events.iter().find(|event| event["outcome"] == "hit"))
    .context("warm diagnostics did not retain a native-cache hit event")?;
  let hit_phases = hit["trace"]["phases"]
    .as_array()
    .context("native-cache hit event has no phase trace")?;
  for phase in [
    "session_load",
    "argument_normalization_input_capture",
    "action_lookup",
    "final_action_revalidation",
    "result_restore_materialization",
    "cargo_output_publication",
  ] {
    assert!(
      hit_phases.iter().any(|trace| trace["phase"] == phase),
      "native-cache hit trace omitted {phase}: {hit}"
    );
  }

  let receipts = std::fs::read_dir(ws.path.join("target/cargo-rail/receipts"))?
    .map(|entry| -> Result<serde_json::Value> { Ok(serde_json::from_slice(&std::fs::read(entry?.path())?)?) })
    .collect::<Result<Vec<_>>>()?;
  assert_eq!(receipts.len(), 1);
  let receipt = &receipts[0];
  assert!(receipt["snapshot_id"].is_null());
  assert_eq!(
    receipt["snapshot_status"],
    "not_loaded_for_exact_native_compiler_cache_execution"
  );
  assert_eq!(
    receipt["execution"]["execution_mode"],
    "exact_native_compiler_cache_execution"
  );
  assert_eq!(receipt["execution"]["native_cache"], "active");
  assert!(receipt["execution"]["native_cache_reason"].is_null());
  assert_eq!(
    receipt["actions"][0]["argv"],
    serde_json::json!(["cargo", "check", "--workspace", "--quiet"])
  );

  let fallback_diagnostics = output.path().join("explicit-nonincremental-print.json");
  let fallback = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&ws.path)
    .args([
      "rail",
      "--diagnostics-file",
      fallback_diagnostics
        .to_str()
        .context("non-UTF-8 fallback diagnostics path")?,
      "run",
      "--all",
      "--action",
      "build",
      "--print-cmd",
      "--",
      "--quiet",
    ])
    .env("CARGO_HOME", cargo_home.path())
    .env("CARGO_RAIL_CACHE_DIR", cache.path())
    .env("CARGO_INCREMENTAL", "0")
    .env_remove("RUSTC_FORCE_INCREMENTAL")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  ensure!(
    fallback.status.success(),
    "explicit non-incremental fallback failed: {}",
    String::from_utf8_lossy(&fallback.stderr)
  );
  let printed_commands = String::from_utf8_lossy(&fallback.stdout)
    .lines()
    .filter(|line| line.starts_with("build: cargo "))
    .count();
  assert_eq!(
    printed_commands,
    1,
    "a speculative pre-context path must not print before falling back: {}",
    String::from_utf8_lossy(&fallback.stdout)
  );
  Ok(())
}
