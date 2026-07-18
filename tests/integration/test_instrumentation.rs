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
  assert_eq!(counters["schema_version"], 1);
  assert_eq!(counters["cargo_metadata_loads"], 0);
  assert_eq!(counters["cargo_metadata_cache_hits"], 1);
  assert_eq!(counters["target_view_loads"], 0);
  assert!(counters["hash_operations"].as_u64().is_some_and(|count| count >= 3));
  assert!(counters["hash_input_bytes"].as_u64().is_some_and(|bytes| bytes > 0));
  assert!(
    counters["hashed_file_bytes_read"]
      .as_u64()
      .is_some_and(|bytes| bytes > 0)
  );
  assert_eq!(counters["git_subprocesses"], 8);
  assert_eq!(counters["graph_traversals"], 2);
  assert!(counters["graph_node_visits"].as_u64().is_some_and(|count| count >= 4));
  assert!(counters["graph_edge_visits"].as_u64().is_some_and(|count| count >= 2));
  Ok(())
}

#[test]
fn unify_diagnostics_distinguish_base_and_target_metadata_loads() -> Result<()> {
  let ws = TestWorkspace::new_named("diagnostic-target-metadata")?;
  ws.add_crate("demo", "0.1.0", &[])?;
  std::fs::write(
    ws.path.join(".config/rail.toml"),
    format!(
      "targets = [\"{}\"]\n\n[unify]\ndetect_unused = false\ndetect_undeclared_features = false\nmsrv = false\n",
      host_target()?
    ),
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
