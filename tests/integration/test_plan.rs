//! Integration tests for `cargo rail plan`.

use crate::helpers::{TestWorkspace, git, run_cargo_rail};
use anyhow::{Result, anyhow};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

const GOLDEN_PLAN_JSON: &str = include_str!("../fixtures/plan/plan_json.golden");
const GOLDEN_PLAN_GITHUB: &str = include_str!("../fixtures/plan/plan_github.golden");
const GOLDEN_PLAN_GITHUB_DEBUG: &str = include_str!("../fixtures/plan/plan_github_debug.golden");
const PLAN_V3_SCHEMA: &str = include_str!("../../schemas/plan-v3.schema.json");

#[test]
fn test_plan_schema_command_matches_published_schema() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-schema-command")?;
  let output = run_cargo_rail(&ws.path, &["rail", "plan", "--schema"])?;

  assert!(
    output.status.success(),
    "plan --schema should not require workspace metadata"
  );
  assert_eq!(String::from_utf8_lossy(&output.stdout), PLAN_V3_SCHEMA);
  assert!(output.stderr.is_empty(), "schema output must keep stderr empty");

  Ok(())
}

#[test]
fn test_plan_json_validates_against_published_schema() -> Result<()> {
  let ws = setup_golden_workspace("plan-schema-validation")?;
  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success(), "plan json should succeed");

  let schema: Value = serde_json::from_str(PLAN_V3_SCHEMA)?;
  let instance: Value = serde_json::from_slice(&output.stdout)?;
  let validator = jsonschema::validator_for(&schema).map_err(|error| anyhow!("invalid planner schema: {error}"))?;
  let errors: Vec<_> = validator
    .iter_errors(&instance)
    .map(|error| error.to_string())
    .collect();
  assert!(
    errors.is_empty(),
    "planner output failed its published schema: {errors:#?}"
  );

  let mut invalid = instance;
  invalid
    .as_object_mut()
    .ok_or_else(|| anyhow!("plan output is not an object"))?
    .remove("scope");
  assert!(
    !validator.is_valid(&invalid),
    "schema must reject a contract missing a required field"
  );

  Ok(())
}

#[test]
fn test_plan_json_contract_and_impact() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-contract")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.commit("add crates")?;

  git(&ws.path, &["branch", "origin/main"])?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn hello() -> &'static str { \"changed\" }")?;
  ws.commit("change lib-a src")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success(), "plan command should succeed");

  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: Value = serde_json::from_str(&stdout)?;

  assert_eq!(json["schema_version"], serde_json::Value::Number(1.into()));
  assert_eq!(json["command"], serde_json::Value::String("plan".to_string()));
  assert_eq!(json["mode"], serde_json::Value::String("inspect".to_string()));
  assert_eq!(json["result"], serde_json::Value::String("success".to_string()));
  assert_eq!(json["exit_code"], serde_json::Value::Number(0.into()));
  assert_eq!(json["plan_contract_version"], serde_json::Value::Number(3.into()));
  assert!(json.get("inputs").is_some(), "missing inputs");
  assert!(json.get("files").is_some(), "missing files");
  assert!(json.get("impact").is_some(), "missing impact");
  assert!(json.get("scope").is_some(), "missing scope");
  assert!(json.get("surfaces").is_some(), "missing surfaces");
  assert!(json.get("trace").is_some(), "missing trace");

  let direct = json["impact"]["direct_crates"]
    .as_array()
    .expect("direct_crates should be an array")
    .iter()
    .filter_map(|v| v.as_str())
    .collect::<Vec<_>>();

  let transitive = json["impact"]["transitive_crates"]
    .as_array()
    .expect("transitive_crates should be an array")
    .iter()
    .filter_map(|v| v.as_str())
    .collect::<Vec<_>>();

  assert!(direct.contains(&"lib-a"), "lib-a should be direct");
  assert!(transitive.contains(&"lib-b"), "lib-b should be transitive");
  assert_eq!(
    json["impact"]["execution_transitive_crates"],
    serde_json::json!(["lib-b"])
  );
  assert_eq!(
    json["scope"]["scope_contract_version"],
    serde_json::Value::Number(2.into())
  );
  assert_eq!(
    json["scope"]["mode"],
    serde_json::Value::String("workspace".to_string())
  );
  assert_eq!(json["scope"]["crates"], serde_json::json!([]));
  assert_eq!(json["scope"]["cargo_args"], serde_json::json!(["--workspace"]));

  assert_eq!(json["surfaces"]["build"]["enabled"], serde_json::Value::Bool(true));
  assert_eq!(json["surfaces"]["test"]["enabled"], serde_json::Value::Bool(true));
  assert_eq!(
    json["inputs"]["confidence_profile"],
    serde_json::Value::String("balanced".to_string())
  );
  assert_eq!(
    json["inputs"]["confidence_profile_source"],
    serde_json::Value::String("config".to_string())
  );

  Ok(())
}

#[test]
fn test_explicit_object_range_ignores_head_index_worktree_and_untracked_state() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-object-range-isolation")?;
  ws.add_crate("demo", "0.1.0", &[])?;
  std::fs::write(ws.path.join("crates/demo/hidden.txt"), "stable\n")?;
  generate_lockfile(&ws)?;
  let from = ws.commit("add demo")?;

  ws.modify_file("demo", "src/lib.rs", "pub fn historical_change() -> bool { true }\n")?;
  let to = ws.commit("historical range change")?;

  std::fs::write(ws.path.join("crates/demo/head_only.rs"), "// later HEAD state\n")?;
  ws.commit("later HEAD change")?;

  let plan_args = [
    "rail",
    "plan",
    "--from",
    from.as_str(),
    "--to",
    to.as_str(),
    "--format",
    "json",
  ];
  let hash_args = [
    "rail",
    "hash",
    "--from",
    from.as_str(),
    "--to",
    to.as_str(),
    "--format",
    "json",
  ];
  let graph_args = ["rail", "graph", "--from", from.as_str(), "--to", to.as_str()];

  let clean_plan = run_cargo_rail(&ws.path, &plan_args)?;
  let clean_hash = run_cargo_rail(&ws.path, &hash_args)?;
  let clean_graph = run_cargo_rail(&ws.path, &graph_args)?;
  assert!(clean_plan.status.success(), "clean historical plan should succeed");
  assert!(clean_hash.status.success(), "clean historical hash should succeed");
  assert!(clean_graph.status.success(), "clean historical graph should succeed");

  let plan: Value = serde_json::from_slice(&clean_plan.stdout)?;
  assert_eq!(plan["inputs"]["refs"]["from"], from);
  assert_eq!(plan["inputs"]["refs"]["to"], to);
  assert_eq!(plan["inputs"]["refs"]["resolved_base"], from);
  assert_eq!(plan["inputs"]["refs"]["resolved_head"], to);
  let planned_paths = plan["files"]
    .as_array()
    .expect("files should be an array")
    .iter()
    .filter_map(|file| file["path"].as_str())
    .collect::<Vec<_>>();
  assert_eq!(planned_paths, ["crates/demo/src/lib.rs"]);

  git(
    &ws.path,
    &["update-index", "--assume-unchanged", "crates/demo/hidden.txt"],
  )?;
  std::fs::write(ws.path.join("crates/demo/staged_only.rs"), "// index state\n")?;
  git(&ws.path, &["add", "crates/demo/staged_only.rs"])?;
  ws.modify_file("demo", "README.md", "# unstaged worktree state\n")?;
  std::fs::write(ws.path.join("crates/demo/untracked_only.rs"), "// untracked state\n")?;

  let dirty_plan = run_cargo_rail(&ws.path, &plan_args)?;
  let dirty_hash = run_cargo_rail(&ws.path, &hash_args)?;
  let dirty_graph = run_cargo_rail(&ws.path, &graph_args)?;
  assert!(dirty_plan.status.success(), "dirty historical plan should succeed");
  assert!(dirty_hash.status.success(), "dirty historical hash should succeed");
  assert!(dirty_graph.status.success(), "dirty historical graph should succeed");
  assert_eq!(
    dirty_plan.stdout, clean_plan.stdout,
    "dirty state changed historical plan"
  );
  assert_eq!(
    dirty_hash.stdout, clean_hash.stdout,
    "dirty state changed historical identity"
  );
  assert_eq!(
    dirty_graph.stdout, clean_graph.stdout,
    "dirty state changed historical graph"
  );

  let option_shaped_ref = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--from=--cached", "--to", "HEAD", "--format", "json"],
  )?;
  assert!(
    !option_shaped_ref.status.success(),
    "an option-shaped ref must not turn an object comparison into an index diff"
  );

  Ok(())
}

#[test]
fn test_plan_worktree_includes_untracked_non_ignored_files() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-untracked-worktree")?;
  ws.add_crate("demo", "0.1.0", &[])?;
  generate_lockfile(&ws)?;
  std::fs::write(ws.path.join(".gitignore"), "crates/demo/src/ignored.rs\n")?;
  ws.commit("Add demo crate and ignore policy")?;

  std::fs::write(ws.path.join("crates/demo/src/untracked.rs"), "pub fn untracked() {}\n")?;
  std::fs::write(ws.path.join("crates/demo/src/ignored.rs"), "pub fn ignored() {}\n")?;

  let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
  assert!(
    output.status.success(),
    "plan failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  let json: Value = serde_json::from_slice(&output.stdout)?;
  let files: Vec<_> = json["files"]
    .as_array()
    .expect("files should be an array")
    .iter()
    .filter_map(|file| file["path"].as_str())
    .collect();
  assert_eq!(files, ["crates/demo/src/untracked.rs"]);
  assert_eq!(json["impact"]["direct_crates"], serde_json::json!(["demo"]));
  Ok(())
}

#[test]
fn test_plan_json_golden_output() -> Result<()> {
  let ws = setup_golden_workspace("plan-json-golden")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success(), "plan json should succeed");

  let actual = normalize_plan_json_output(&String::from_utf8_lossy(&output.stdout))?;
  assert_eq!(actual, GOLDEN_PLAN_JSON.trim_end());

  Ok(())
}

#[test]
fn test_plan_github_golden_output() -> Result<()> {
  let ws = setup_golden_workspace("plan-github-golden")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "github"],
  )?;
  assert!(output.status.success(), "plan github should succeed");

  let actual = normalize_plan_github_output(&String::from_utf8_lossy(&output.stdout))?;
  assert_eq!(actual, GOLDEN_PLAN_GITHUB.trim_end());

  Ok(())
}

#[test]
fn test_plan_github_debug_golden_output() -> Result<()> {
  let ws = setup_golden_workspace("plan-github-debug-golden")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "github-debug"],
  )?;
  assert!(output.status.success(), "plan github-debug should succeed");

  let actual = normalize_plan_github_debug_output(&String::from_utf8_lossy(&output.stdout))?;
  assert_eq!(actual, GOLDEN_PLAN_GITHUB_DEBUG.trim_end());

  Ok(())
}

#[test]
fn test_plan_text_output_is_concise() -> Result<()> {
  let ws = setup_golden_workspace("plan-text-summary")?;

  let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "origin/main"])?;
  assert!(output.status.success(), "plan text should succeed");

  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(stdout.contains("surfaces: build, test"));
  assert!(stdout.contains("scope: workspace"));
  assert!(stdout.contains("why:"));
  assert!(!stdout.contains("transitive crates:"));
  assert!(!stdout.contains("trace:"));

  Ok(())
}

#[test]
fn test_plan_docs_only_surfaces() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-docs")?;
  ws.add_crate("docs-a", "0.1.0", &[])?;
  ws.commit("add crate")?;

  git(&ws.path, &["branch", "origin/main"])?;

  ws.modify_file("docs-a", "README.md", "# Updated docs\n")?;
  ws.commit("docs only")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success(), "plan command should succeed");

  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: Value = serde_json::from_str(&stdout)?;

  assert_eq!(json["scope"]["mode"], Value::String("empty".to_string()));
  assert_eq!(json["surfaces"]["docs"]["enabled"], serde_json::Value::Bool(true));
  assert_eq!(json["surfaces"]["build"]["enabled"], serde_json::Value::Bool(false));
  assert_eq!(json["surfaces"]["test"]["enabled"], serde_json::Value::Bool(false));

  Ok(())
}

#[test]
fn test_plan_rust_src_fixture() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-rust-src")?;
  ws.add_crate("src-a", "0.1.0", &[])?;
  ws.commit("add crate")?;

  git(&ws.path, &["branch", "origin/main"])?;

  ws.modify_file("src-a", "src/lib.rs", "pub fn changed() -> bool { true }")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success());

  let json: Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(json["files"][0]["kind"], Value::String("rust".to_string()));
  assert_eq!(json["files"][0]["sub_kind"], Value::String("src".to_string()));
  assert_eq!(json["scope"]["mode"], Value::String("workspace".to_string()));
  assert_eq!(json["scope"]["crates"], serde_json::json!([]));
  assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(true));

  Ok(())
}

#[test]
fn test_plan_bench_fixture() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-bench")?;
  ws.add_crate("bench-a", "0.1.0", &[])?;
  ws.commit("add crate")?;

  git(&ws.path, &["branch", "origin/main"])?;

  let bench_file = ws.path.join("crates/bench-a/benches/smoke.rs");
  std::fs::create_dir_all(bench_file.parent().ok_or_else(|| anyhow!("missing bench dir"))?)?;
  std::fs::write(&bench_file, "fn main() {}\n")?;
  ws.commit("add bench file")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success());

  let json: Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(json["files"][0]["kind"], Value::String("rust".to_string()));
  assert_eq!(json["files"][0]["sub_kind"], Value::String("bench".to_string()));
  assert_eq!(json["surfaces"]["bench"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(false));
  assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(false));

  Ok(())
}

#[test]
fn test_plan_ci_and_script_fixture() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-ci-script")?;
  ws.add_crate("infra-a", "0.1.0", &[])?;
  ws.commit("add crate")?;

  git(&ws.path, &["branch", "origin/main"])?;

  std::fs::create_dir_all(ws.path.join(".github/workflows"))?;
  std::fs::create_dir_all(ws.path.join("scripts"))?;
  std::fs::write(ws.path.join(".github/workflows/ci.yml"), "name: CI\n")?;
  std::fs::write(ws.path.join("scripts/check.sh"), "#!/usr/bin/env bash\necho check\n")?;
  ws.commit("add ci and script files")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success());

  let json: Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(json["surfaces"]["infra"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(false));
  assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(false));

  let kinds: Vec<String> = json["files"]
    .as_array()
    .ok_or_else(|| anyhow!("files should be an array"))?
    .iter()
    .filter_map(|file| file["kind"].as_str().map(ToString::to_string))
    .collect();

  assert!(kinds.contains(&"ci".to_string()));
  assert!(kinds.contains(&"script".to_string()));

  Ok(())
}

#[test]
fn test_plan_toml_infra_fixture() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-toml-infra")?;
  ws.add_crate("toml-a", "0.1.0", &[])?;
  ws.commit("add crate")?;

  git(&ws.path, &["branch", "origin/main"])?;

  std::fs::write(
    ws.path.join("rust-toolchain.toml"),
    "[toolchain]\nchannel = \"stable\"\n",
  )?;
  ws.commit("add rust-toolchain file")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success());

  let json: Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(json["files"][0]["kind"], Value::String("toml".to_string()));
  assert_eq!(json["files"][0]["sub_kind"], Value::String("tooling".to_string()));
  assert_eq!(json["scope"]["mode"], Value::String("workspace".to_string()));
  assert_eq!(json["scope"]["crates"], serde_json::json!([]));
  assert_eq!(json["surfaces"]["infra"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(true));

  Ok(())
}

#[test]
fn test_plan_partial_workspace_scope_uses_crates_mode() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-partial-scope")?;
  ws.add_crate("scope-a", "0.1.0", &[])?;
  ws.add_crate("scope-b", "0.1.0", &[])?;
  ws.commit("add crates")?;

  git(&ws.path, &["branch", "origin/main"])?;

  ws.modify_file("scope-a", "src/lib.rs", "pub fn changed() -> bool { true }")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success(), "plan should succeed");

  let json: Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(json["scope"]["mode"], Value::String("crates".to_string()));
  assert_eq!(json["scope"]["crates"], serde_json::json!(["scope-a"]));

  Ok(())
}

#[test]
fn test_plan_json_deterministic_output() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-deterministic")?;
  ws.add_crate("det-a", "0.1.0", &[])?;
  generate_lockfile(&ws)?;
  ws.commit("add crate")?;

  git(&ws.path, &["branch", "origin/main"])?;

  ws.modify_file("det-a", "src/lib.rs", "pub fn stable() -> &'static str { \"yes\" }")?;
  ws.commit("change src")?;

  let first = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  let second = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;

  assert!(
    first.status.success() && second.status.success(),
    "both plan runs should succeed"
  );

  let first_stdout = String::from_utf8_lossy(&first.stdout);
  let second_stdout = String::from_utf8_lossy(&second.stdout);

  assert_eq!(
    first_stdout, second_stdout,
    "plan JSON output should be byte-identical across identical runs"
  );

  Ok(())
}

#[test]
fn test_plan_github_deterministic_output() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-gh-deterministic")?;
  ws.add_crate("det-gh-a", "0.1.0", &[])?;
  generate_lockfile(&ws)?;
  ws.commit("add crate")?;

  git(&ws.path, &["branch", "origin/main"])?;

  ws.modify_file("det-gh-a", "src/lib.rs", "pub fn stable() -> i32 { 1 }")?;

  let first = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "github"],
  )?;
  let second = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "github"],
  )?;

  assert!(first.status.success() && second.status.success());
  assert_eq!(
    String::from_utf8_lossy(&first.stdout),
    String::from_utf8_lossy(&second.stdout),
    "plan GitHub output should be byte-identical across identical runs"
  );

  Ok(())
}

#[test]
fn test_plan_output_file_overwrites_existing_content() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-output-overwrite")?;
  ws.add_crate("write-a", "0.1.0", &[])?;
  ws.commit("add crate")?;

  git(&ws.path, &["branch", "origin/main"])?;
  ws.modify_file("write-a", "src/lib.rs", "pub fn updated() -> bool { true }")?;
  ws.commit("change src")?;

  let output_path = ws.path.join("plan-output.json");
  let output_path_str = output_path.to_string_lossy().to_string();

  let first = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "plan",
      "--since",
      "origin/main",
      "--format",
      "json",
      "--output",
      &output_path_str,
    ],
  )?;
  assert!(first.status.success(), "first output write should succeed");

  let second = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "plan",
      "--since",
      "origin/main",
      "--format",
      "json",
      "--output",
      &output_path_str,
    ],
  )?;
  assert!(second.status.success(), "second output write should succeed");

  let content = std::fs::read_to_string(&output_path)?;
  let parsed: Value = serde_json::from_str(&content)?;
  assert_eq!(parsed["plan_contract_version"], Value::Number(3.into()));
  assert_eq!(
    content.matches("\"plan_contract_version\"").count(),
    1,
    "output file should contain a single JSON document, not appended documents"
  );

  Ok(())
}

#[test]
fn test_plan_enabled_surfaces_always_have_trace_reasons() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-surface-reasons")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("add crate")?;

  git(&ws.path, &["branch", "origin/main"])?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn reasoned() -> bool { true }")?;
  ws.commit("change src")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success(), "plan should succeed");

  let json: Value = serde_json::from_slice(&output.stdout)?;
  let trace_ids: BTreeSet<u64> = json["trace"]
    .as_array()
    .ok_or_else(|| anyhow!("trace should be array"))?
    .iter()
    .filter_map(|entry| entry["id"].as_u64())
    .collect();

  let surfaces = json["surfaces"]
    .as_object()
    .ok_or_else(|| anyhow!("surfaces should be object"))?;
  for (surface_name, decision) in surfaces {
    let enabled = decision["enabled"]
      .as_bool()
      .ok_or_else(|| anyhow!("enabled should be bool"))?;
    let reasons = decision["reasons"]
      .as_array()
      .ok_or_else(|| anyhow!("reasons should be array"))?;
    if enabled {
      assert!(
        !reasons.is_empty(),
        "enabled surface '{}' must have at least one reason",
        surface_name
      );
      for reason in reasons {
        let reason_id = reason
          .as_u64()
          .ok_or_else(|| anyhow!("reason id should be integer for {}", surface_name))?;
        assert!(
          trace_ids.contains(&reason_id),
          "surface '{}' references missing trace id {}",
          surface_name,
          reason_id
        );
      }
    }
  }

  Ok(())
}

#[test]
fn test_plan_docs_change_with_dependents_keeps_build_and_test_off() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-docs-dependents")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.commit("add crates")?;

  git(&ws.path, &["branch", "origin/main"])?;

  ws.modify_file("lib-a", "README.md", "# docs update\n")?;
  ws.commit("docs only on dependency")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success(), "plan command should succeed");

  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;

  assert_eq!(json["surfaces"]["docs"]["enabled"], serde_json::Value::Bool(true));
  assert_eq!(json["surfaces"]["build"]["enabled"], serde_json::Value::Bool(false));
  assert_eq!(json["surfaces"]["test"]["enabled"], serde_json::Value::Bool(false));

  Ok(())
}

#[test]
fn test_plan_custom_surface_and_github_output() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-custom-github")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;

  let config = r#"[change-detection.custom]
verify = ["verify/**"]
"#;
  std::fs::write(ws.path.join(".config/rail.toml"), config)?;
  ws.commit("configure custom change detection")?;

  git(&ws.path, &["branch", "origin/main"])?;

  std::fs::create_dir_all(ws.path.join("verify"))?;
  std::fs::write(ws.path.join("verify/run.sh"), "#!/usr/bin/env bash\necho ok\n")?;
  ws.commit("add verify script")?;

  let json_output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(json_output.status.success(), "plan json should succeed");
  let json_stdout = String::from_utf8_lossy(&json_output.stdout);
  let json: serde_json::Value = serde_json::from_str(&json_stdout)?;

  assert_eq!(
    json["surfaces"]["custom:verify"]["enabled"],
    serde_json::Value::Bool(true),
    "custom surface should be enabled"
  );

  let github_output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "github"],
  )?;
  assert!(github_output.status.success(), "plan github should succeed");
  let gh_stdout = String::from_utf8_lossy(&github_output.stdout);

  assert!(gh_stdout.contains("build="), "github output must include build key");
  assert!(gh_stdout.contains("test="), "github output must include test key");
  assert!(
    gh_stdout.contains("scope_json="),
    "github output must include scope_json key"
  );
  assert!(
    !gh_stdout.contains("plan_json="),
    "compact github output must omit plan_json"
  );

  Ok(())
}

#[test]
fn test_plan_custom_surface_is_additive_with_docs_classification() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-custom-precedence")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;

  let config = r#"[change-detection.custom]
verify = ["crates/**/README.md"]
"#;
  std::fs::write(ws.path.join(".config/rail.toml"), config)?;
  ws.commit("configure custom change detection")?;

  git(&ws.path, &["branch", "origin/main"])?;

  ws.modify_file("lib-a", "README.md", "# custom docs path\n")?;
  ws.commit("change crate readme")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(
    output.status.success(),
    "plan should succeed with custom pattern config"
  );

  let json: Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(json["files"][0]["kind"], Value::String("docs".to_string()));
  assert_eq!(
    json["files"][0]["custom_surfaces"],
    serde_json::json!(["custom:verify"])
  );
  assert_eq!(json["surfaces"]["custom:verify"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["docs"]["enabled"], Value::Bool(true));

  let trace = json["trace"].as_array().expect("trace should be array");
  assert!(
    trace
      .iter()
      .any(|entry| entry["code"] == Value::String("FILE_KIND_DOCS".to_string())),
    "trace should include FILE_KIND_DOCS for builtin classification"
  );
  assert!(
    trace
      .iter()
      .any(|entry| entry["code"] == Value::String("FILE_KIND_CUSTOM".to_string())),
    "trace should include FILE_KIND_CUSTOM for the overlay surface"
  );

  Ok(())
}

#[test]
fn test_plan_custom_surface_is_additive_with_infra_classification() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-custom-infra-overlap")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;

  let config = r#"[change-detection.custom]
merge_validation = [".github/workflows/**"]
"#;
  std::fs::write(ws.path.join(".config/rail.toml"), config)?;
  ws.commit("configure custom change detection")?;

  git(&ws.path, &["branch", "origin/main"])?;

  let workflow_dir = ws.path.join(".github/workflows");
  std::fs::create_dir_all(&workflow_dir)?;
  std::fs::write(workflow_dir.join("ci.yaml"), "name: CI\non: [push]\n")?;
  ws.commit("add workflow file")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(
    output.status.success(),
    "plan should succeed with overlapping infra/custom patterns"
  );

  let json: Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(json["files"][0]["kind"], Value::String("ci".to_string()));
  assert_eq!(
    json["files"][0]["custom_surfaces"],
    serde_json::json!(["custom:merge_validation"])
  );
  assert_eq!(json["surfaces"]["infra"]["enabled"], Value::Bool(true));
  assert_eq!(
    json["surfaces"]["custom:merge_validation"]["enabled"],
    Value::Bool(true)
  );

  let trace = json["trace"].as_array().expect("trace should be array");
  assert!(
    trace
      .iter()
      .any(|entry| entry["code"] == Value::String("FILE_KIND_CI".to_string())),
    "trace should include FILE_KIND_CI for builtin infra classification"
  );
  assert!(
    trace
      .iter()
      .any(|entry| entry["code"] == Value::String("FILE_KIND_CUSTOM".to_string())),
    "trace should include FILE_KIND_CUSTOM for the overlay surface"
  );

  Ok(())
}

#[test]
fn test_plan_configured_infra_pattern_adds_infra_surface() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-configured-infra-pattern")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;

  let config = r#"[change-detection]
infrastructure = ["notes/**"]
"#;
  std::fs::write(ws.path.join(".config/rail.toml"), config)?;
  ws.commit("configure infra pattern")?;

  git(&ws.path, &["branch", "origin/main"])?;

  let notes_dir = ws.path.join("notes");
  std::fs::create_dir_all(&notes_dir)?;
  std::fs::write(notes_dir.join("archive.bin"), "payload\n")?;
  ws.commit("add notes payload")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(
    output.status.success(),
    "plan should succeed with configured infra pattern"
  );

  let json: Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(json["files"][0]["kind"], Value::String("unknown".to_string()));
  assert_eq!(json["surfaces"]["infra"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(false));
  assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(false));

  let trace = json["trace"].as_array().expect("trace should be array");
  assert!(
    trace
      .iter()
      .any(|entry| entry["code"] == Value::String("FILE_KIND_INFRA_PATTERN".to_string())),
    "trace should include FILE_KIND_INFRA_PATTERN for configured infra matching"
  );

  Ok(())
}

#[test]
fn test_plan_file_can_enable_multiple_custom_surfaces() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-multiple-custom-overlap")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;

  let config = r#"[change-detection.custom]
docs_pipeline = ["docs/**"]
manual_gate = ["docs/**"]
"#;
  std::fs::write(ws.path.join(".config/rail.toml"), config)?;
  ws.commit("configure custom change detection")?;

  git(&ws.path, &["branch", "origin/main"])?;

  let docs_dir = ws.path.join("docs");
  std::fs::create_dir_all(&docs_dir)?;
  std::fs::write(docs_dir.join("guide.md"), "# Guide\n")?;
  ws.commit("add docs guide")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(
    output.status.success(),
    "plan should allow multiple custom surfaces per file"
  );

  let json: Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(json["files"][0]["kind"], Value::String("docs".to_string()));
  assert_eq!(
    json["files"][0]["custom_surfaces"],
    serde_json::json!(["custom:docs_pipeline", "custom:manual_gate"])
  );
  assert_eq!(json["surfaces"]["docs"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["custom:docs_pipeline"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["custom:manual_gate"]["enabled"], Value::Bool(true));

  Ok(())
}

#[test]
fn test_plan_runs_without_config_using_defaults() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-no-config-defaults")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("add crate")?;

  ws.remove_config()?;
  ws.commit("remove config")?;

  ws.modify_file(
    "lib-a",
    "src/lib.rs",
    "pub fn changed_without_config() -> bool { true }",
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD~1", "--format", "json"])?;
  assert!(output.status.success(), "plan should work without .config/rail.toml");

  let json: Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(true));

  Ok(())
}

#[test]
fn test_plan_no_changes() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-no-changes")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("add crate")?;

  git(&ws.path, &["branch", "origin/main"])?;

  // No changes after branching
  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(
    output.status.success(),
    "plan with no changes should succeed\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  let json: Value = serde_json::from_slice(&output.stdout)?;

  assert_eq!(json["files"].as_array().map(Vec::len), Some(0), "no files changed");
  assert_eq!(
    json["impact"]["direct_crates"].as_array().map(Vec::len),
    Some(0),
    "no direct crates"
  );
  assert_eq!(
    json["impact"]["transitive_crates"].as_array().map(Vec::len),
    Some(0),
    "no transitive crates"
  );
  assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(false));
  assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(false));
  assert_eq!(json["surfaces"]["bench"]["enabled"], Value::Bool(false));
  assert_eq!(json["surfaces"]["docs"]["enabled"], Value::Bool(false));
  assert_eq!(json["surfaces"]["infra"]["enabled"], Value::Bool(false));

  Ok(())
}

#[test]
fn test_plan_invalid_since_ref() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-invalid-ref")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("add crate")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "nonexistent-ref", "--format", "json"],
  )?;
  assert!(!output.status.success(), "plan with invalid ref should fail");

  Ok(())
}

#[test]
fn test_plan_workspace_cargo_toml_change() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-ws-toml")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("add crate")?;

  git(&ws.path, &["branch", "origin/main"])?;

  // Modify root Cargo.toml
  let cargo_toml = std::fs::read_to_string(ws.path.join("Cargo.toml"))?;
  std::fs::write(
    ws.path.join("Cargo.toml"),
    format!("{}\n# workspace change\n", cargo_toml),
  )?;
  ws.commit("modify workspace Cargo.toml")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success(), "plan should succeed");

  let json: Value = serde_json::from_slice(&output.stdout)?;

  // Workspace Cargo.toml is classified as toml:workspace
  assert_eq!(json["files"][0]["kind"], Value::String("toml".to_string()));
  assert_eq!(json["files"][0]["sub_kind"], Value::String("workspace".to_string()));

  // Workspace toml changes trigger infra, build, and test
  assert_eq!(json["surfaces"]["infra"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(true));

  Ok(())
}

#[test]
fn test_plan_test_file_no_transitive_surfaces() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-test-no-transitive")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.commit("add crates")?;

  git(&ws.path, &["branch", "origin/main"])?;

  // Create a test file in lib-a (not src — test files don't seed transitive build/test)
  let test_dir = ws.path.join("crates/lib-a/tests");
  std::fs::create_dir_all(&test_dir)?;
  std::fs::write(test_dir.join("integration.rs"), "fn main() {}\n")?;
  ws.commit("add test file to lib-a")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success(), "plan should succeed");

  let json: Value = serde_json::from_slice(&output.stdout)?;

  // Test file is classified as rust:test
  assert_eq!(json["files"][0]["kind"], Value::String("rust".to_string()));
  assert_eq!(json["files"][0]["sub_kind"], Value::String("test".to_string()));

  assert!(
    json["impact"]["direct_crates"]
      .as_array()
      .map(|a| a.iter().any(|v| v.as_str() == Some("lib-a")))
      .unwrap_or(false),
    "lib-a should be a direct crate"
  );

  // lib-b appears in transitive_crates (it's a dependent of direct crate lib-a),
  // but the key behavior is that test files do NOT seed build/test surface reasons
  // for transitive dependents. The trace should have no TRANSITIVE_DEPENDS_ON_DIRECT
  // entries because file_kind_seeds_build_test_transitive returns false for rust:test.
  let trace = json["trace"].as_array().expect("trace should be an array");

  let transitive_trace_entries: Vec<&Value> = trace
    .iter()
    .filter(|t| t["code"].as_str() == Some("TRANSITIVE_DEPENDS_ON_DIRECT"))
    .collect();

  assert!(
    transitive_trace_entries.is_empty(),
    "test files should NOT produce TRANSITIVE_DEPENDS_ON_DIRECT trace entries, \
     but found: {:?}",
    transitive_trace_entries
  );

  // Only 'test' surface should be enabled (from rust:test classification),
  // NOT 'build' (test files don't seed build)
  assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(false));

  Ok(())
}

#[test]
fn test_plan_unclassified_crate_owned_file_enables_conservative_build_test() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-unclassified-conservative")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.commit("add crates")?;

  git(&ws.path, &["branch", "origin/main"])?;

  let gen_dir = ws.path.join("crates/lib-a/generated");
  std::fs::create_dir_all(&gen_dir)?;
  std::fs::write(gen_dir.join("schema.capnp"), "struct Msg {}\n")?;
  ws.commit("add unclassified crate-owned file")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success(), "plan should succeed");

  let json: Value = serde_json::from_slice(&output.stdout)?;

  assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(true));

  let transitive = json["impact"]["transitive_crates"]
    .as_array()
    .expect("transitive_crates must be an array");
  assert!(
    transitive.iter().any(|name| name.as_str() == Some("lib-b")),
    "fallback should seed transitive build/test impact for dependents"
  );

  let trace = json["trace"].as_array().expect("trace must be an array");
  assert!(
    trace
      .iter()
      .any(|entry| entry["code"] == Value::String("OWNER_UNCERTAIN_FALLBACK".to_string())),
    "trace should include explicit conservative fallback reason"
  );

  Ok(())
}

#[test]
fn test_plan_unclassified_owner_fallback_can_be_disabled() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-unclassified-aggressive")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("add crate")?;

  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[workspace]
root = "."

[toolchain]
channel = "stable"

[change-detection]
conservative_unclassified_owner_fallback = false
"#,
  )?;
  ws.commit("disable conservative unclassified owner fallback")?;

  git(&ws.path, &["branch", "origin/main"])?;

  let gen_dir = ws.path.join("crates/lib-a/generated");
  std::fs::create_dir_all(&gen_dir)?;
  std::fs::write(gen_dir.join("schema.capnp"), "struct Msg {}\n")?;
  ws.commit("add unclassified crate-owned file")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success(), "plan should succeed");

  let json: Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(json["files"][0]["kind"], Value::String("unknown".to_string()));
  assert_eq!(json["surfaces"]["docs"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["infra"]["enabled"], Value::Bool(false));
  assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(false));
  assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(false));

  Ok(())
}

#[test]
fn test_plan_repo_config_files_no_build_test() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-repo-config")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("add crate")?;

  git(&ws.path, &["branch", "origin/main"])?;

  // Add .gitignore at workspace root - this is a repo config file
  std::fs::write(ws.path.join(".gitignore"), "target/\n*.log\n")?;
  ws.commit("add gitignore")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success(), "plan should succeed");

  let json: Value = serde_json::from_slice(&output.stdout)?;

  // .gitignore should be classified as config:repo
  assert_eq!(
    json["files"][0]["kind"],
    Value::String("config".to_string()),
    ".gitignore should be classified as config kind"
  );
  assert_eq!(
    json["files"][0]["sub_kind"],
    Value::String("repo".to_string()),
    ".gitignore should have repo sub_kind"
  );

  // Repo config files should only trigger docs surface (like docs files)
  // They should NOT trigger build/test surfaces
  assert_eq!(
    json["surfaces"]["docs"]["enabled"],
    Value::Bool(true),
    "repo config should enable docs surface"
  );
  assert_eq!(
    json["surfaces"]["build"]["enabled"],
    Value::Bool(false),
    "repo config should NOT enable build surface"
  );
  assert_eq!(
    json["surfaces"]["test"]["enabled"],
    Value::Bool(false),
    "repo config should NOT enable test surface"
  );

  // Verify the trace has the correct reason code
  let trace = json["trace"].as_array().expect("trace should be array");
  assert!(
    trace
      .iter()
      .any(|entry| entry["code"] == Value::String("FILE_KIND_REPO_CONFIG".to_string())),
    "trace should include FILE_KIND_REPO_CONFIG reason"
  );

  Ok(())
}

#[test]
fn test_plan_editorconfig_is_repo_config() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-editorconfig")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("add crate")?;

  git(&ws.path, &["branch", "origin/main"])?;

  std::fs::write(ws.path.join(".editorconfig"), "[*]\nindent_style = space\n")?;
  ws.commit("add editorconfig")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success(), "plan should succeed");

  let json: Value = serde_json::from_slice(&output.stdout)?;

  assert_eq!(json["files"][0]["kind"], Value::String("config".to_string()));
  assert_eq!(json["files"][0]["sub_kind"], Value::String("repo".to_string()));
  assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(false));
  assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(false));

  Ok(())
}

#[test]
fn test_plan_nested_gitignore_is_unclassified() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-nested-gitignore")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("add crate")?;

  git(&ws.path, &["branch", "origin/main"])?;

  // Nested .gitignore inside a crate - should NOT be repo config
  // (repo config is only root-level files)
  let crate_gitignore = ws.path.join("crates/lib-a/.gitignore");
  std::fs::write(&crate_gitignore, "*.tmp\n")?;
  ws.commit("add nested gitignore")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success(), "plan should succeed");

  let json: Value = serde_json::from_slice(&output.stdout)?;

  // Nested .gitignore should NOT be classified as config:repo
  // It's a crate-owned file and falls to unclassified
  assert_eq!(
    json["files"][0]["kind"],
    Value::String("unknown".to_string()),
    "nested .gitignore should classify as unknown"
  );

  Ok(())
}

#[test]
fn test_plan_confidence_profile_strict_expands_docs_owned_file() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-profile-strict")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.commit("add crates")?;

  git(&ws.path, &["branch", "origin/main"])?;
  ws.modify_file("lib-a", "README.md", "# docs changed\n")?;
  ws.commit("change docs in owned crate")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "plan",
      "--since",
      "origin/main",
      "--format",
      "json",
      "--confidence-profile",
      "strict",
    ],
  )?;
  assert!(output.status.success(), "plan should succeed");

  let json: Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(
    json["inputs"]["confidence_profile"],
    Value::String("strict".to_string())
  );
  assert_eq!(
    json["inputs"]["confidence_profile_source"],
    Value::String("cli".to_string())
  );
  assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(true));

  let transitive = json["impact"]["transitive_crates"]
    .as_array()
    .ok_or_else(|| anyhow!("transitive_crates should be array"))?;
  assert!(
    transitive.iter().any(|name| name.as_str() == Some("lib-b")),
    "strict mode should seed transitive impact from crate-owned docs changes"
  );
  assert!(
    json["impact"]["execution_transitive_crates"]
      .as_array()
      .ok_or_else(|| anyhow!("execution_transitive_crates should be array"))?
      .iter()
      .any(|name| name.as_str() == Some("lib-b")),
    "strict mode should widen execution scope for dependent crates"
  );

  Ok(())
}

#[test]
fn test_plan_confidence_profile_fast_disables_transitive_build_test() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-profile-fast")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.commit("add crates")?;

  git(&ws.path, &["branch", "origin/main"])?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() -> bool { true }")?;
  ws.commit("change lib-a src")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "plan",
      "--since",
      "origin/main",
      "--format",
      "json",
      "--confidence-profile",
      "fast",
    ],
  )?;
  assert!(output.status.success(), "plan should succeed");

  let json: Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(json["inputs"]["confidence_profile"], Value::String("fast".to_string()));
  assert_eq!(
    json["impact"]["transitive_crates"].as_array().map(Vec::len),
    Some(1),
    "transitive impact list remains graph-level"
  );
  assert_eq!(
    json["impact"]["execution_transitive_crates"].as_array().map(Vec::len),
    Some(0),
    "fast profile should not widen execution transitive crates"
  );

  let trace = json["trace"]
    .as_array()
    .ok_or_else(|| anyhow!("trace should be array"))?;
  assert!(
    trace
      .iter()
      .any(|entry| entry["code"] == Value::String("CONFIDENCE_FAST_SKIP_TRANSITIVE".to_string())),
    "fast profile should emit explicit skip-transitive trace"
  );

  assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(true));
  let reasons = json["surfaces"]["build"]["reasons"]
    .as_array()
    .ok_or_else(|| anyhow!("build reasons should be array"))?;
  assert_eq!(
    reasons.len(),
    1,
    "fast profile should avoid adding transitive build reasons"
  );
  assert_eq!(json["scope"]["mode"], Value::String("crates".to_string()));
  assert_eq!(json["scope"]["crates"], serde_json::json!(["lib-a"]));

  Ok(())
}

#[test]
fn test_plan_bot_pr_policy_override_to_strict() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-bot-policy")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("add crate")?;

  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[workspace]
root = "."

[toolchain]
channel = "stable"

[change-detection]
confidence_profile = "fast"
bot_pr_confidence_profile = "strict"
"#,
  )?;
  ws.commit("enable bot pr strict policy")?;

  git(&ws.path, &["branch", "origin/main"])?;
  ws.modify_file("lib-a", "README.md", "# docs update\n")?;
  ws.commit("change docs")?;

  let cargo_rail_bin = env!("CARGO_BIN_EXE_cargo-rail");
  let output = Command::new(cargo_rail_bin)
    .current_dir(&ws.path)
    .env("GIT_CONFIG_COUNT", "2")
    .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
    .env("GIT_CONFIG_VALUE_0", "false")
    .env("GIT_CONFIG_KEY_1", "tag.gpgsign")
    .env("GIT_CONFIG_VALUE_1", "false")
    .env("GITHUB_EVENT_NAME", "pull_request")
    .env("GITHUB_ACTOR", "dependabot[bot]")
    .args(["rail", "plan", "--since", "origin/main", "--format", "json"])
    .output()?;

  assert!(output.status.success(), "plan should succeed");

  let json: Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(
    json["inputs"]["confidence_profile"],
    Value::String("strict".to_string())
  );
  assert_eq!(
    json["inputs"]["confidence_profile_source"],
    Value::String("bot_pr_policy".to_string())
  );
  assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(true));

  Ok(())
}

#[test]
fn test_plan_github_projections() -> Result<()> {
  let ws = TestWorkspace::new_named("plan-gh-projections")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.commit("add crates")?;

  git(&ws.path, &["branch", "origin/main"])?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn projected() -> bool { true }")?;
  ws.commit("change lib-a src")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "plan", "--since", "origin/main", "--format", "github"],
  )?;
  assert!(output.status.success(), "plan github should succeed");

  let stdout = String::from_utf8_lossy(&output.stdout);
  let kv: BTreeMap<String, String> = stdout
    .lines()
    .filter(|l| !l.trim().is_empty())
    .filter_map(|l| l.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
    .collect();

  // All projection keys must be present
  let expected_keys = [
    "build",
    "test",
    "bench",
    "docs",
    "infra",
    "base_ref",
    "cargo_args",
    "scope_json",
  ];
  for key in expected_keys {
    assert!(kv.contains_key(key), "missing key: {}", key);
  }

  assert_eq!(kv["base_ref"], "origin/main");
  assert_eq!(kv["cargo_args"], "--workspace");

  let scope_json: Value = serde_json::from_str(&kv["scope_json"])?;
  assert_eq!(scope_json["mode"], serde_json::json!("workspace"));
  assert_eq!(scope_json["crates"], serde_json::json!([]));
  assert_eq!(scope_json["cargo_args"], serde_json::json!(["--workspace"]));
  assert_eq!(scope_json["scope_contract_version"], serde_json::json!(2));
  assert_eq!(scope_json["resolved_head"], serde_json::json!("WORKTREE"));
  assert!(
    !kv.contains_key("plan_json"),
    "compact github output should omit plan_json"
  );

  Ok(())
}

fn setup_golden_workspace(name: &str) -> Result<TestWorkspace> {
  let ws = TestWorkspace::new_named(name)?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.commit("add crates")?;

  git(&ws.path, &["branch", "origin/main"])?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn hello() -> i32 { 42 }")?;
  Ok(ws)
}

fn generate_lockfile(ws: &TestWorkspace) -> Result<()> {
  let output = Command::new("cargo")
    .current_dir(&ws.path)
    .args(["generate-lockfile", "--offline"])
    .output()?;
  assert!(
    output.status.success(),
    "offline lockfile generation failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  Ok(())
}

fn normalize_plan_json_output(stdout: &str) -> Result<String> {
  let mut value: Value = serde_json::from_str(stdout)?;
  normalize_plan_json_value(&mut value)?;
  Ok(serde_json::to_string_pretty(&value)?)
}

fn normalize_plan_json_value(value: &mut Value) -> Result<()> {
  if let Some(object) = value.as_object_mut() {
    object.remove("schema_version");
    object.remove("command");
    object.remove("mode");
    object.remove("result");
    object.remove("exit_code");
  }
  value["inputs"]["workspace_root"] = Value::String("<WORKSPACE_ROOT>".to_string());
  value["inputs"]["config_fingerprint"] = Value::String("<CONFIG_FP>".to_string());
  value["inputs"]["toolchain_fingerprint"] = Value::String("<TOOLCHAIN_FP>".to_string());
  value["scope"]["resolved_base"] = Value::String("origin/main".to_string());
  value["scope"]["resolved_head"] = Value::String("WORKTREE".to_string());
  value["reproducibility"]["cargo_rail_version"] = Value::String("<VERSION>".to_string());
  value["reproducibility"]["config_hash"] = Value::String("<CONFIG_HASH>".to_string());
  Ok(())
}

fn normalize_plan_github_output(stdout: &str) -> Result<String> {
  let mut kv = BTreeMap::new();
  for line in stdout.lines() {
    if line.trim().is_empty() {
      continue;
    }
    let (key, value) = line
      .split_once('=')
      .ok_or_else(|| anyhow!("invalid github output line: {}", line))?;
    kv.insert(key.to_string(), value.to_string());
  }

  let scope_json_raw = kv
    .get("scope_json")
    .ok_or_else(|| anyhow!("missing scope_json key in github output"))?;
  let scope_json: Value = serde_json::from_str(scope_json_raw)?;

  let ordered_keys = [
    "build",
    "test",
    "bench",
    "docs",
    "infra",
    "base_ref",
    "cargo_args",
    "scope_json",
  ];

  let mut lines = Vec::new();
  for key in ordered_keys {
    match key {
      "scope_json" => lines.push(format!("{}={}", key, serde_json::to_string(&scope_json)?)),
      _ => {
        let value = kv
          .get(key)
          .ok_or_else(|| anyhow!("missing {} key in github output", key))?;
        lines.push(format!("{}={}", key, value));
      }
    }
  }

  Ok(lines.join("\n"))
}

fn normalize_plan_github_debug_output(stdout: &str) -> Result<String> {
  let mut kv = BTreeMap::new();
  for line in stdout.lines() {
    if line.trim().is_empty() {
      continue;
    }
    let (key, value) = line
      .split_once('=')
      .ok_or_else(|| anyhow!("invalid github-debug output line: {}", line))?;
    kv.insert(key.to_string(), value.to_string());
  }

  let scope_json_raw = kv
    .get("scope_json")
    .ok_or_else(|| anyhow!("missing scope_json key in github-debug output"))?;
  let scope_json: Value = serde_json::from_str(scope_json_raw)?;

  let plan_json_raw = kv
    .get("plan_json")
    .ok_or_else(|| anyhow!("missing plan_json key in github-debug output"))?;
  let mut plan_json: Value = serde_json::from_str(plan_json_raw)?;
  normalize_plan_json_value(&mut plan_json)?;

  let ordered_keys = [
    "build",
    "test",
    "bench",
    "docs",
    "infra",
    "base_ref",
    "cargo_args",
    "scope_json",
    "plan_json",
  ];

  let mut lines = Vec::new();
  for key in ordered_keys {
    match key {
      "scope_json" => lines.push(format!("{}={}", key, serde_json::to_string(&scope_json)?)),
      "plan_json" => lines.push(format!("{}={}", key, serde_json::to_string(&plan_json)?)),
      _ => {
        let value = kv
          .get(key)
          .ok_or_else(|| anyhow!("missing {} key in github-debug output", key))?;
        lines.push(format!("{}={}", key, value));
      }
    }
  }

  Ok(lines.join("\n"))
}
