//! Integration tests for `cargo rail plan`.

use crate::helpers::{TestWorkspace, git, run_cargo_rail};
use anyhow::{Result, anyhow};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

const GOLDEN_PLAN_JSON: &str = include_str!("../fixtures/plan/plan_json.golden");
const GOLDEN_PLAN_GITHUB: &str = include_str!("../fixtures/plan/plan_github.golden");
const GOLDEN_PLAN_GITHUB_DEBUG: &str = include_str!("../fixtures/plan/plan_github_debug.golden");
const PLAN_V7_SCHEMA: &str = include_str!("../../schemas/plan-v7.schema.json");

#[test]
fn test_plan_schema_command_matches_published_schema() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-schema-command")?;
        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--schema"])?;

        assert!(
            output.status.success(),
            "plan --schema should not require workspace metadata"
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), PLAN_V7_SCHEMA);
        assert!(output.stderr.is_empty(), "schema output must keep stderr empty");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_json_validates_against_published_schema() {
    let result: Result<()> = (|| {
        let ws = setup_golden_workspace("plan-schema-validation")?;
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "plan", "--since", "origin/main", "--format", "json"],
        )?;
        assert!(output.status.success(), "plan json should succeed");

        let schema: Value = serde_json::from_str(PLAN_V7_SCHEMA)?;
        let instance: Value = serde_json::from_slice(&output.stdout)?;
        let validator =
            jsonschema::validator_for(&schema).map_err(|error| anyhow!("invalid planner schema: {error}"))?;
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_json_contract_and_impact() {
    let result: Result<()> = (|| {
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
        assert_eq!(json["plan_contract_version"], serde_json::Value::Number(7.into()));
        assert!(json.get("inputs").is_some(), "missing inputs");
        assert_eq!(json["resolution_universe"]["mode"], "declared_dependencies");
        assert!(
            json["resolution_universe"]["identity"]
                .as_str()
                .is_some_and(|identity| identity.starts_with("resolution-universe-v1:sha256:"))
        );
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

        let transitive = json["impact"]["build_transitive_crates"]
            .as_array()
            .expect("build_transitive_crates should be an array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();

        assert!(direct.contains(&"lib-a"), "lib-a should be direct");
        assert!(transitive.contains(&"lib-b"), "lib-b should be transitive");
        assert_eq!(json["impact"]["development_transitive_crates"], serde_json::json!([]));
        assert_eq!(
            json["scope"]["scope_contract_version"],
            serde_json::Value::Number(4.into())
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_explicit_object_range_ignores_head_index_worktree_and_untracked_state() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_worktree_includes_untracked_non_ignored_files() {
    let result: Result<()> = (|| {
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
            "plan failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_json_golden_output() {
    let result: Result<()> = (|| {
        let ws = setup_golden_workspace("plan-json-golden")?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "plan", "--since", "origin/main", "--format", "json"],
        )?;
        assert!(output.status.success(), "plan json should succeed");

        let actual = normalize_plan_json_output(&String::from_utf8_lossy(&output.stdout))?;
        assert_eq!(actual, GOLDEN_PLAN_JSON.trim_end());

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_github_golden_output() {
    let result: Result<()> = (|| {
        let ws = setup_golden_workspace("plan-github-golden")?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "plan", "--since", "origin/main", "--format", "github"],
        )?;
        assert!(output.status.success(), "plan github should succeed");

        let actual = normalize_plan_github_output(&String::from_utf8_lossy(&output.stdout))?;
        assert_eq!(actual, GOLDEN_PLAN_GITHUB.trim_end());

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_github_debug_golden_output() {
    let result: Result<()> = (|| {
        let ws = setup_golden_workspace("plan-github-debug-golden")?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "plan", "--since", "origin/main", "--format", "github-debug"],
        )?;
        assert!(output.status.success(), "plan github-debug should succeed");

        let actual = normalize_plan_github_debug_output(&String::from_utf8_lossy(&output.stdout))?;
        assert_eq!(actual, GOLDEN_PLAN_GITHUB_DEBUG.trim_end());

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_text_output_is_concise() {
    let result: Result<()> = (|| {
        let ws = setup_golden_workspace("plan-text-summary")?;

        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "origin/main"])?;
        assert!(output.status.success(), "plan text should succeed");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("surfaces: bench, build, surface, test"));
        assert!(stdout.contains("scope: workspace"));
        assert!(stdout.contains("why:"));
        assert!(!stdout.contains("transitive crates:"));
        assert!(!stdout.contains("trace:"));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_docs_only_surfaces() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_rust_src_fixture() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_bench_fixture() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_ci_and_script_fixture() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_toml_infra_fixture() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_ignores_workspace_metadata_only_manifest_edits() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-workspace-metadata")?;
        ws.add_crate("metadata-a", "0.1.0", &[])?;
        let root_manifest = ws.path.join("Cargo.toml");
        let mut manifest = std::fs::read_to_string(&root_manifest)?;
        manifest.push_str("\n[workspace.metadata.example]\nvalue = 1\n");
        std::fs::write(&root_manifest, manifest)?;
        ws.commit("add workspace metadata")?;

        let manifest = std::fs::read_to_string(&root_manifest)?.replace("value = 1", "value = 2");
        std::fs::write(&root_manifest, manifest)?;
        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(
            output.status.success(),
            "plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;

        for surface in ["build", "test", "bench"] {
            assert_eq!(
                json["surfaces"][surface]["enabled"], false,
                "{surface} must stay disabled"
            );
        }
        assert_eq!(json["scope"]["mode"], "empty");
        assert!(json["trace"].as_array().is_some_and(|trace| trace.iter().any(|reason| {
            reason["code"] == "SEMANTIC_INPUT_UNCHANGED" && reason["semantic_input"] == "workspace_manifest"
        })));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_localizes_workspace_dependency_edits_to_declared_consumers() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-workspace-dependency")?;
        ws.add_crate("shared-core", "0.1.0", &[])?;
        let consumer = ws.add_crate("consumer", "0.1.0", &[])?;
        let optional_consumer = ws.add_crate("optional-consumer", "0.1.0", &[])?;
        ws.add_crate("unrelated", "0.1.0", &[])?;
        let root_manifest = ws.path.join("Cargo.toml");
        let mut root = std::fs::read_to_string(&root_manifest)?;
        root.push_str("shared-core = { path = \"crates/shared-core\" }\n");
        std::fs::write(&root_manifest, root)?;
        std::fs::write(
            consumer.join("Cargo.toml"),
            r#"[package]
name = "consumer"
version = "0.1.0"
edition.workspace = true

[dependencies]
shared-core.workspace = true
"#,
        )?;
        std::fs::write(
            optional_consumer.join("Cargo.toml"),
            r#"[package]
name = "optional-consumer"
version = "0.1.0"
edition.workspace = true

[features]
use-core = ["dep:shared-core"]

[dependencies]
shared-core = { workspace = true, optional = true }
"#,
        )?;
        ws.commit("add inherited workspace dependency")?;

        let root = std::fs::read_to_string(&root_manifest)?.replace(
            "shared-core = { path = \"crates/shared-core\" }",
            "shared-core = { path = \"crates/shared-core\", default-features = false }",
        );
        std::fs::write(&root_manifest, root)?;
        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(
            output.status.success(),
            "plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;

        assert_eq!(
            json["impact"]["direct_crates"],
            serde_json::json!(["consumer", "optional-consumer"])
        );
        assert_eq!(
            json["surfaces"]["build"]["scope"]["crates"],
            serde_json::json!(["consumer", "optional-consumer"])
        );
        assert!(
            !json["surfaces"]["build"]["scope"]["crates"]
                .as_array()
                .is_some_and(|crates| crates.iter().any(|package| package == "unrelated"))
        );
        assert!(json["trace"].as_array().is_some_and(|trace| trace.iter().any(|reason| {
            reason["code"] == "SEMANTIC_INPUT_PACKAGES"
                && reason["semantic_input"] == "workspace_manifest"
                && reason["crate"] == "consumer"
        })));
        assert!(json["trace"].as_array().is_some_and(|trace| trace.iter().any(|reason| {
            reason["code"] == "SEMANTIC_INPUT_PACKAGES"
                && reason["semantic_input"] == "workspace_manifest"
                && reason["crate"] == "optional-consumer"
        })));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_ignores_package_metadata_only_manifest_edits() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-package-metadata")?;
        let package = ws.add_crate("metadata-a", "0.1.0", &[])?;
        let manifest_path = package.join("Cargo.toml");
        let mut manifest = std::fs::read_to_string(&manifest_path)?;
        manifest.push_str("\n[package.metadata.example]\nvalue = 1\n");
        std::fs::write(&manifest_path, manifest)?;
        ws.commit("add package metadata")?;

        let manifest = std::fs::read_to_string(&manifest_path)?.replace("value = 1", "value = 2");
        std::fs::write(&manifest_path, manifest)?;
        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(
            output.status.success(),
            "plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(json["surfaces"]["build"]["enabled"], false);
        assert_eq!(json["surfaces"]["test"]["enabled"], false);
        assert_eq!(json["surfaces"]["bench"]["enabled"], false);
        assert_eq!(json["impact"]["direct_crates"], serde_json::json!(["metadata-a"]));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_ignores_root_package_metadata_only_manifest_edits() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("root-metadata", "0.1.0")?;
        let manifest_path = ws.path.join("Cargo.toml");
        let mut manifest = std::fs::read_to_string(&manifest_path)?;
        manifest.push_str("\n[package.metadata.example]\nvalue = 1\n");
        std::fs::write(&manifest_path, manifest)?;
        ws.commit("add root package metadata")?;

        let manifest = std::fs::read_to_string(&manifest_path)?.replace("value = 1", "value = 2");
        std::fs::write(&manifest_path, manifest)?;
        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(
            output.status.success(),
            "root metadata plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(json["surfaces"]["build"]["enabled"], false);
        assert_eq!(json["surfaces"]["test"]["enabled"], false);
        assert!(json["trace"].as_array().is_some_and(|trace| trace.iter().any(|reason| {
            reason["semantic_input"] == "workspace_manifest" && reason["code"] == "SEMANTIC_INPUT_UNCHANGED"
        })));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_ignores_lockfile_formatting_without_resolved_changes() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-lock-formatting")?;
        let consumer = ws.add_crate("lock-consumer", "0.1.0", &[])?;
        ws.add_crate("lock-unrelated", "0.1.0", &[])?;
        let manifest_path = consumer.join("Cargo.toml");
        let mut manifest = std::fs::read_to_string(&manifest_path)?;
        manifest.push_str("anyhow.workspace = true\n");
        std::fs::write(manifest_path, manifest)?;
        generate_lockfile(&ws)?;
        ws.commit("add locked dependency")?;

        let lock_path = ws.path.join("Cargo.lock");
        let lock = std::fs::read_to_string(&lock_path)?;
        std::fs::write(&lock_path, format!("# planner must ignore this comment\n{lock}"))?;

        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(
            output.status.success(),
            "plan failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(json["surfaces"]["build"]["enabled"], false);
        assert_eq!(json["surfaces"]["test"]["enabled"], false);
        assert_eq!(json["surfaces"]["bench"]["enabled"], false);
        assert!(json["trace"].as_array().is_some_and(|trace| {
            trace
                .iter()
                .any(|reason| reason["semantic_input"] == "lockfile" && reason["code"] == "SEMANTIC_INPUT_UNCHANGED")
        }));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_localizes_one_package_lock_update() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-lock-package-localization")?;
        let consumer = ws.add_crate("lock-consumer", "0.1.0", &[])?;
        ws.add_crate("lock-unrelated", "0.1.0", &[])?;
        let manifest_path = consumer.join("Cargo.toml");
        let mut manifest = std::fs::read_to_string(&manifest_path)?;
        manifest.push_str("anyhow.workspace = true\n");
        std::fs::write(manifest_path, manifest)?;
        generate_lockfile(&ws)?;

        let lock_path = ws.path.join("Cargo.lock");
        let valid_lock = std::fs::read_to_string(&lock_path)?;
        let checksum_prefix = "checksum = \"";
        let checksum_start = valid_lock
            .find(checksum_prefix)
            .map(|index| index + checksum_prefix.len())
            .ok_or_else(|| anyhow!("fixture lockfile has no registry checksum"))?;
        let checksum_end = valid_lock
            .get(checksum_start..)
            .ok_or_else(|| anyhow!("registry checksum starts outside the fixture lockfile"))?
            .find('"')
            .map(|index| checksum_start + index)
            .ok_or_else(|| anyhow!("fixture lockfile has an unterminated checksum"))?;
        let mut baseline_lock = valid_lock.clone();
        baseline_lock.replace_range(checksum_start..checksum_end, &"0".repeat(checksum_end - checksum_start));
        std::fs::write(&lock_path, baseline_lock)?;
        ws.commit("record previous registry checksum")?;
        std::fs::write(&lock_path, valid_lock)?;

        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(
            output.status.success(),
            "lock localization plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(
            json["impact"]["build_transitive_crates"],
            serde_json::json!(["lock-consumer"])
        );
        assert_eq!(
            json["surfaces"]["build"]["scope"]["crates"],
            serde_json::json!(["lock-consumer"])
        );
        assert!(
            !json["surfaces"]["build"]["scope"]["crates"]
                .as_array()
                .is_some_and(|packages| packages.iter().any(|package| package == "lock-unrelated"))
        );
        assert!(json["trace"].as_array().is_some_and(|trace| {
            trace
                .iter()
                .any(|reason| reason["semantic_input"] == "lockfile" && reason["code"] == "SEMANTIC_INPUT_PACKAGES")
        }));
        assert!(json["trace"].as_array().is_some_and(|trace| trace.iter().any(|reason| {
            reason["depends_on_package_id"]
                .as_str()
                .is_some_and(|package_id| package_id.contains("anyhow"))
        })));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_partial_workspace_scope_uses_crates_mode() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_cargo_args_preserve_hyphenated_package_tokens() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-exact-cargo-args")?;
        ws.add_crate("core-lib", "0.1.0", &[])?;
        ws.add_crate("consumer-tool", "0.1.0", &[("core-lib", r#"{ path = "../core-lib" }"#)])?;
        ws.add_crate("unrelated-bin", "0.1.0", &[])?;
        ws.commit("add hyphenated packages")?;
        ws.modify_file("core-lib", "src/lib.rs", "pub fn changed() {}\n")?;

        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(
            output.status.success(),
            "plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(
            json["surfaces"]["build"]["scope"]["cargo_args"],
            serde_json::json!(["-p", "consumer-tool", "-p", "core-lib"])
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_deleted_and_renamed_files_keep_deterministic_ownership() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-deleted-renamed-ownership")?;
        let package = ws.add_crate("owned-lib", "0.1.0", &[])?;
        std::fs::write(package.join("src/deleted.rs"), "pub fn deleted() {}\n")?;
        std::fs::write(package.join("src/old.rs"), "pub fn renamed() {}\n")?;
        ws.commit("add owned files")?;

        std::fs::remove_file(package.join("src/deleted.rs"))?;
        std::fs::rename(package.join("src/old.rs"), package.join("src/new.rs"))?;

        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(
            output.status.success(),
            "plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;
        let files = json["files"]
            .as_array()
            .ok_or_else(|| anyhow!("files must be an array"))?;
        assert_eq!(
            files.iter().map(|file| file["path"].as_str()).collect::<Vec<_>>(),
            [
                Some("crates/owned-lib/src/deleted.rs"),
                Some("crates/owned-lib/src/new.rs"),
                Some("crates/owned-lib/src/old.rs")
            ]
        );
        assert!(
            files
                .iter()
                .all(|file| { file["owner_scope"] == "crate" && file["owners"] == serde_json::json!(["owned-lib"]) })
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_resolution_universe_identity_covers_edges_and_package_facts() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-universe-identity")?;
        let identity_a = ws.add_crate("identity-a", "0.1.0", &[])?;
        let identity_b = ws.add_crate("identity-b", "0.1.0", &[])?;
        ws.commit("add identity packages")?;

        let before = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(before.status.success(), "baseline plan failed");
        let before: Value = serde_json::from_slice(&before.stdout)?;

        let manifest_path = identity_b.join("Cargo.toml");
        let mut manifest = std::fs::read_to_string(&manifest_path)?;
        manifest.push_str("identity-a = { path = \"../identity-a\", optional = true }\n");
        std::fs::write(manifest_path, manifest)?;
        generate_lockfile(&ws)?;
        let after = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(
            after.status.success(),
            "changed-universe plan failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&after.stdout),
            String::from_utf8_lossy(&after.stderr),
        );
        let after: Value = serde_json::from_slice(&after.stdout)?;

        assert_ne!(
            before["resolution_universe"]["identity"], after["resolution_universe"]["identity"],
            "a declared optional edge must change the versioned universe identity"
        );

        let manifest_path = identity_a.join("Cargo.toml");
        let mut manifest = std::fs::read_to_string(&manifest_path)?;
        manifest.push_str("\n[lib]\nproc-macro = true\n");
        std::fs::write(manifest_path, manifest)?;
        let package_facts = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(package_facts.status.success(), "package-fact plan failed");
        let package_facts: Value = serde_json::from_slice(&package_facts.stdout)?;
        assert_ne!(
            after["resolution_universe"]["identity"], package_facts["resolution_universe"]["identity"],
            "a proc-macro package fact must change the versioned universe identity"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_scopes_development_dependents_to_test_and_bench() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-development-domain")?;
        ws.add_crate("domain-a", "0.1.0", &[])?;
        let domain_b = ws.add_crate("domain-b", "0.1.0", &[])?;
        ws.add_crate("domain-c", "0.1.0", &[("domain-b", r#"{ path = "../domain-b" }"#)])?;
        std::fs::write(
            domain_b.join("Cargo.toml"),
            r#"[package]
name = "domain-b"
version = "0.1.0"
edition.workspace = true

[dev-dependencies]
domain-a = { path = "../domain-a" }
"#,
        )?;
        ws.commit("add development dependency")?;
        ws.modify_file("domain-a", "src/lib.rs", "pub fn changed() {}\n")?;

        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(
            output.status.success(),
            "plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;

        assert_eq!(json["impact"]["build_transitive_crates"], serde_json::json!([]));
        assert_eq!(
            json["impact"]["development_transitive_crates"],
            serde_json::json!(["domain-b"])
        );
        assert_eq!(
            json["surfaces"]["build"]["scope"]["crates"],
            serde_json::json!(["domain-a"])
        );
        assert_eq!(
            json["surfaces"]["test"]["scope"]["crates"],
            serde_json::json!(["domain-a", "domain-b"])
        );
        assert_eq!(json["surfaces"]["bench"]["scope"]["mode"], serde_json::json!("crates"));
        assert_eq!(
            json["surfaces"]["bench"]["scope"]["crates"],
            serde_json::json!(["domain-b"])
        );
        assert!(json["trace"].as_array().is_some_and(|trace| {
            trace
                .iter()
                .all(|reason| reason["crate"] != "domain-c" && reason["depends_on"] != "domain-b")
        }));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_scopes_build_dependents_to_build_test_and_bench() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-build-domain")?;
        ws.add_crate("build-a", "0.1.0", &[])?;
        let build_b = ws.add_crate("build-b", "0.1.0", &[])?;
        ws.add_crate("build-c", "0.1.0", &[("build-b", r#"{ path = "../build-b" }"#)])?;
        std::fs::write(
            build_b.join("Cargo.toml"),
            r#"[package]
name = "build-b"
version = "0.1.0"
edition.workspace = true

[build-dependencies]
build-a = { path = "../build-a" }
"#,
        )?;
        ws.commit("add build dependency")?;
        ws.modify_file("build-a", "src/lib.rs", "pub fn changed() {}\n")?;

        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(
            output.status.success(),
            "plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;
        let reason = json["trace"]
            .as_array()
            .and_then(|trace| {
                trace
                    .iter()
                    .find(|reason| reason["code"] == "TRANSITIVE_DEPENDS_ON_DIRECT" && reason["edge_kind"] == "build")
            })
            .ok_or_else(|| anyhow!("missing build dependency reason"))?;
        assert_eq!(reason["edge_kind"], "build");
        assert_eq!(reason["host"], true);
        assert_eq!(
            reason["selected_surfaces"],
            serde_json::json!(["bench", "build", "test"])
        );
        assert_eq!(
            json["impact"]["build_transitive_crates"],
            serde_json::json!(["build-b", "build-c"])
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_preserves_proc_macro_host_evidence() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-proc-macro-domain")?;
        let proc_macro = ws.add_crate("derive-core", "0.1.0", &[])?;
        let manifest_path = proc_macro.join("Cargo.toml");
        let mut manifest = std::fs::read_to_string(&manifest_path)?;
        manifest.push_str("\n[lib]\nproc-macro = true\n");
        std::fs::write(manifest_path, manifest)?;
        ws.add_crate(
            "macro-user",
            "0.1.0",
            &[("derive-core", r#"{ path = "../derive-core" }"#)],
        )?;
        ws.commit("add proc macro dependency")?;
        ws.modify_file("derive-core", "src/lib.rs", "use proc_macro::TokenStream;\n")?;

        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(
            output.status.success(),
            "plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;
        let reason = json["trace"]
            .as_array()
            .and_then(|trace| {
                trace
                    .iter()
                    .find(|reason| reason["code"] == "TRANSITIVE_DEPENDS_ON_DIRECT")
            })
            .ok_or_else(|| anyhow!("missing proc-macro dependency reason"))?;
        assert_eq!(reason["proc_macro"], true);
        assert_eq!(reason["host"], true);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_propagates_normal_dependencies_with_exact_edge_evidence() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-normal-domain")?;
        ws.add_crate("normal-a", "0.1.0", &[])?;
        ws.add_crate("normal-b", "0.1.0", &[("normal-a", "{ path = \"../normal-a\" }")])?;
        ws.commit("add normal dependency")?;
        ws.modify_file("normal-a", "src/lib.rs", "pub fn changed() {}\n")?;

        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(
            output.status.success(),
            "plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(
            json["impact"]["build_transitive_crates"],
            serde_json::json!(["normal-b"])
        );
        assert_eq!(json["impact"]["development_transitive_crates"], serde_json::json!([]));

        let reason = json["trace"]
            .as_array()
            .and_then(|trace| {
                trace
                    .iter()
                    .find(|reason| reason["code"] == "TRANSITIVE_DEPENDS_ON_DIRECT")
            })
            .ok_or_else(|| anyhow!("missing semantic impact reason"))?;
        assert_eq!(reason["edge_kind"], "normal");
        assert_eq!(reason["alias"], "normal_a");
        assert_eq!(reason["optional"], false);
        assert_eq!(reason["uses_default_features"], true);
        assert!(reason["package_id"].as_str().is_some_and(|id| id.contains("normal-b")));
        assert!(
            reason["depends_on_package_id"]
                .as_str()
                .is_some_and(|id| id.contains("normal-a"))
        );
        assert_eq!(
            reason["selected_surfaces"],
            serde_json::json!(["bench", "build", "test"])
        );
        assert_eq!(
            json["surfaces"]["bench"]["scope"]["crates"],
            serde_json::json!(["normal-b"])
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_includes_optional_edges_outside_default_features() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-optional-inactive")?;
        ws.add_crate("optional-a", "0.1.0", &[])?;
        let optional_b = ws.add_crate("optional-b", "0.1.0", &[])?;
        std::fs::write(
            optional_b.join("Cargo.toml"),
            r#"[package]
name = "optional-b"
version = "0.1.0"
edition.workspace = true

[features]
default = []
with-a = ["dep:optional-a"]

[dependencies]
optional-a = { path = "../optional-a", optional = true }
"#,
        )?;
        ws.commit("add inactive optional dependency")?;
        ws.modify_file("optional-a", "src/lib.rs", "pub fn changed() {}\n")?;

        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(
            output.status.success(),
            "plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(
            json["impact"]["build_transitive_crates"],
            serde_json::json!(["optional-b"])
        );
        assert_eq!(json["impact"]["development_transitive_crates"], serde_json::json!([]));
        assert_eq!(
            json["surfaces"]["build"]["scope"]["mode"],
            serde_json::json!("workspace")
        );
        let reason = json["trace"]
            .as_array()
            .and_then(|trace| {
                trace
                    .iter()
                    .find(|reason| reason["code"] == "TRANSITIVE_DEPENDS_ON_DIRECT")
            })
            .ok_or_else(|| anyhow!("missing optional dependency reason"))?;
        assert_eq!(reason["optional"], true);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_includes_target_edges_outside_the_host_resolution() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-target-inactive")?;
        ws.add_crate("target-a", "0.1.0", &[])?;
        let target_b = ws.add_crate("target-b", "0.1.0", &[])?;
        std::fs::write(
            target_b.join("Cargo.toml"),
            r#"[package]
name = "target-b"
version = "0.1.0"
edition.workspace = true

[target.'thumbv7em-none-eabihf'.dependencies]
target-a = { path = "../target-a" }
"#,
        )?;
        ws.commit("add target dependency")?;
        ws.modify_file("target-a", "src/lib.rs", "pub fn changed() {}\n")?;

        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(
            output.status.success(),
            "plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(
            json["impact"]["build_transitive_crates"],
            serde_json::json!(["target-b"])
        );
        assert_eq!(
            json["surfaces"]["build"]["scope"]["mode"],
            serde_json::json!("workspace")
        );
        let reason = json["trace"]
            .as_array()
            .and_then(|trace| {
                trace
                    .iter()
                    .find(|reason| reason["code"] == "TRANSITIVE_DEPENDS_ON_DIRECT")
            })
            .ok_or_else(|| anyhow!("missing target dependency reason"))?;
        assert_eq!(reason["target_predicate"], "thumbv7em-none-eabihf");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_uses_effective_cargo_build_target_resolution() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-configured-build-target")?;
        ws.add_crate("target-a", "0.1.0", &[])?;
        let target_b = ws.add_crate("target-b", "0.1.0", &[])?;
        std::fs::write(
            target_b.join("Cargo.toml"),
            r#"[package]
name = "target-b"
version = "0.1.0"
edition.workspace = true

[target.'thumbv7em-none-eabihf'.dependencies]
target-a = { path = "../target-a" }
"#,
        )?;
        std::fs::create_dir_all(ws.path.join(".cargo"))?;
        std::fs::write(
            ws.path.join(".cargo/config.toml"),
            "[build]\ntarget = \"thumbv7em-none-eabihf\"\n",
        )?;
        ws.commit("configure target dependency")?;
        ws.modify_file("target-a", "src/lib.rs", "pub fn changed() {}\n")?;

        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
        assert!(
            output.status.success(),
            "configured target plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(
            json["impact"]["build_transitive_crates"],
            serde_json::json!(["target-b"])
        );
        assert_eq!(
            json["surfaces"]["build"]["scope"]["mode"],
            serde_json::json!("workspace")
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_historical_plan_marks_unfiltered_target_edges_as_conservative() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-target-fallback")?;
        ws.add_crate("target-a", "0.1.0", &[])?;
        let target_b = ws.add_crate("target-b", "0.1.0", &[])?;
        std::fs::write(
            target_b.join("Cargo.toml"),
            r#"[package]
name = "target-b"
version = "0.1.0"
edition.workspace = true

[target.'thumbv7em-none-eabihf'.dependencies]
target-a = { path = "../target-a" }
"#,
        )?;
        let base = ws.commit("add target dependency")?;
        ws.modify_file("target-a", "src/lib.rs", "pub fn changed() {}\n")?;
        let head = ws.commit("change target dependency")?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "plan", "--from", &base, "--to", &head, "--format", "json"],
        )?;
        assert!(
            output.status.success(),
            "plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;
        let reason = json["trace"]
            .as_array()
            .and_then(|trace| {
                trace
                    .iter()
                    .find(|reason| reason["code"] == "TRANSITIVE_DEPENDS_ON_DIRECT")
            })
            .ok_or_else(|| anyhow!("missing target impact reason"))?;
        assert_eq!(reason["target_predicate"], "thumbv7em-none-eabihf");
        assert_eq!(
            reason["fallback_reasons"],
            serde_json::json!(["historical_resolution_unavailable"])
        );

        let text_output = run_cargo_rail(&ws.path, &["rail", "plan", "--from", &base, "--to", &head, "--explain"])?;
        assert!(text_output.status.success(), "text explain should succeed");
        let text = String::from_utf8_lossy(&text_output.stdout);
        assert!(
            text.contains("kind=normal"),
            "edge kind missing from text explain: {text}"
        );
        assert!(
            text.contains("alias=target_a"),
            "alias missing from text explain: {text}"
        );
        assert!(
            text.contains("target=thumbv7em-none-eabihf"),
            "target predicate missing from text explain: {text}"
        );
        assert!(
            !text.contains("target_unfiltered"),
            "declared target edges are not an unfiltered-resolution fallback: {text}"
        );
        assert!(
            text.contains("historical_resolution_unavailable"),
            "historical resolution fallback missing from text explain: {text}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_historical_manifest_diff_falls_back_visibly() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-historical-manifest-fallback")?;
        let package = ws.add_crate("historical-manifest", "0.1.0", &[])?;
        let base = ws.commit("add package")?;
        let manifest_path = package.join("Cargo.toml");
        let mut manifest = std::fs::read_to_string(&manifest_path)?;
        manifest.push_str("\n[features]\nextra = []\n");
        std::fs::write(&manifest_path, manifest)?;
        let head = ws.commit("add historical feature")?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "plan", "--from", &base, "--to", &head, "--format", "json"],
        )?;
        assert!(
            output.status.success(),
            "historical plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(json["surfaces"]["build"]["scope"]["mode"], "workspace");
        assert!(json["trace"].as_array().is_some_and(|trace| trace.iter().any(|reason| {
            reason["semantic_input"] == "manifest"
                && reason["fallback_reasons"] == serde_json::json!(["historical_resolution_unavailable"])
        })));

        let text = run_cargo_rail(&ws.path, &["rail", "plan", "--from", &base, "--to", &head, "--explain"])?;
        assert!(text.status.success(), "historical text plan should succeed");
        assert!(
            String::from_utf8_lossy(&text.stdout).contains("fallback=historical_resolution_unavailable"),
            "historical fallback must be visible in text explain"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_json_deterministic_output() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_github_deterministic_output() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_output_file_overwrites_existing_content() {
    let result: Result<()> = (|| {
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
        assert_eq!(parsed["plan_contract_version"], Value::Number(7.into()));
        assert_eq!(
            content.matches("\"plan_contract_version\"").count(),
            1,
            "output file should contain a single JSON document, not appended documents"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_enabled_surfaces_always_have_trace_reasons() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_docs_change_with_dependents_keeps_build_and_test_off() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_custom_surface_and_github_output() {
    let result: Result<()> = (|| {
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
        let surfaces_json = gh_stdout
            .lines()
            .find_map(|line| line.strip_prefix("surfaces_json="))
            .ok_or_else(|| anyhow!("github output must include surfaces_json"))?;
        let surfaces: Value = serde_json::from_str(surfaces_json)?;
        assert_eq!(surfaces["custom:verify"], true);
        assert!(
            gh_stdout.contains("scope_json="),
            "github output must include scope_json key"
        );
        assert!(
            !gh_stdout.contains("plan_json="),
            "compact github output must omit plan_json"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_custom_surface_is_additive_with_docs_classification() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_custom_surface_is_additive_with_infra_classification() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_configured_infra_pattern_adds_infra_surface() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_file_can_enable_multiple_custom_surfaces() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_runs_without_config_using_defaults() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_no_changes() {
    let result: Result<()> = (|| {
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
            json["impact"]["build_transitive_crates"].as_array().map(Vec::len),
            Some(0),
            "no transitive crates"
        );
        assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(false));
        assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(false));
        assert_eq!(json["surfaces"]["bench"]["enabled"], Value::Bool(false));
        assert_eq!(json["surfaces"]["docs"]["enabled"], Value::Bool(false));
        assert_eq!(json["surfaces"]["infra"]["enabled"], Value::Bool(false));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_invalid_since_ref() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-invalid-ref")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("add crate")?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "plan", "--since", "nonexistent-ref", "--format", "json"],
        )?;
        assert!(!output.status.success(), "plan with invalid ref should fail");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_workspace_cargo_toml_change() {
    let result: Result<()> = (|| {
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

        // Workspace Cargo.toml is classified as toml:workspace.
        assert_eq!(json["files"][0]["kind"], Value::String("toml".to_string()));
        assert_eq!(json["files"][0]["sub_kind"], Value::String("workspace".to_string()));

        // A formatting-only manifest edit does not alter semantic selection.
        assert_eq!(json["surfaces"]["infra"]["enabled"], Value::Bool(true));
        assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(false));
        assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(false));
        assert!(json["trace"].as_array().is_some_and(|trace| trace.iter().any(|reason| {
            reason["code"] == "SEMANTIC_INPUT_UNCHANGED" && reason["semantic_input"] == "workspace_manifest"
        })));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_test_file_no_transitive_surfaces() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_unclassified_crate_owned_file_enables_conservative_build_test() {
    let result: Result<()> = (|| {
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

        let transitive = json["impact"]["build_transitive_crates"]
            .as_array()
            .expect("build_transitive_crates must be an array");
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_unclassified_owner_fallback_can_be_disabled() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-unclassified-aggressive")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("add crate")?;

        std::fs::write(
            ws.path.join(".config/rail.toml"),
            r#"[change-detection]
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_repo_config_files_no_build_test() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_editorconfig_is_repo_config() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_nested_gitignore_is_unclassified() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_confidence_profile_strict_expands_docs_owned_file() {
    let result: Result<()> = (|| {
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

        let transitive = json["impact"]["build_transitive_crates"]
            .as_array()
            .ok_or_else(|| anyhow!("build_transitive_crates should be array"))?;
        assert!(
            transitive.iter().any(|name| name.as_str() == Some("lib-b")),
            "strict mode should seed transitive impact from crate-owned docs changes"
        );
        assert_eq!(json["surfaces"]["build"]["scope"]["mode"], "workspace");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_confidence_profile_fast_disables_transitive_build_test() {
    let result: Result<()> = (|| {
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
            json["impact"]["build_transitive_crates"].as_array().map(Vec::len),
            Some(0),
            "fast profile should not propagate build impact"
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_deprecated_bot_pr_policy_is_warning_backed_and_ignored() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-bot-policy")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("add crate")?;

        std::fs::write(
            ws.path.join(".config/rail.toml"),
            r#"[change-detection]
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
        assert_eq!(json["inputs"]["confidence_profile"], Value::String("fast".to_string()));
        assert_eq!(
            json["inputs"]["confidence_profile_source"],
            Value::String("config".to_string())
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("config migrate"),
            "deprecated provider policy must produce actionable migration guidance"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_github_projections() {
    let result: Result<()> = (|| {
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
            "surface",
            "base_ref",
            "cargo_args",
            "surfaces_json",
            "scope_json",
        ];
        for key in expected_keys {
            assert!(kv.contains_key(key), "missing key: {}", key);
        }

        assert_eq!(kv["base_ref"], "origin/main");
        assert_eq!(kv["cargo_args"], "--workspace");
        let surfaces_json: Value = serde_json::from_str(&kv["surfaces_json"])?;
        assert_eq!(surfaces_json["build"], true);
        assert_eq!(surfaces_json["test"], true);

        let scope_json: Value = serde_json::from_str(&kv["scope_json"])?;
        assert_eq!(scope_json["mode"], serde_json::json!("workspace"));
        assert_eq!(scope_json["crates"], serde_json::json!([]));
        assert_eq!(scope_json["cargo_args"], serde_json::json!(["--workspace"]));
        assert_eq!(scope_json["scope_contract_version"], serde_json::json!(4));
        assert_eq!(scope_json["resolved_head"], serde_json::json!("WORKTREE"));
        assert!(
            !kv.contains_key("plan_json"),
            "compact github output should omit plan_json"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
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
    anyhow::ensure!(
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
    value["inputs"]["snapshot_id"] = Value::String("<SNAPSHOT_ID>".to_string());
    value["inputs"]["config_fingerprint"] = Value::String("<CONFIG_FP>".to_string());
    value["inputs"]["toolchain_fingerprint"] = Value::String("<TOOLCHAIN_FP>".to_string());
    value["scope"]["resolved_base"] = Value::String("origin/main".to_string());
    value["scope"]["resolved_head"] = Value::String("WORKTREE".to_string());
    value["reproducibility"]["cargo_rail_version"] = Value::String("<VERSION>".to_string());
    value["reproducibility"]["config_hash"] = Value::String("<CONFIG_HASH>".to_string());
    if let Some(trace) = value["trace"].as_array_mut() {
        for reason in trace {
            if reason.get("package_id").is_some() {
                let name = reason["crate"].as_str().unwrap_or("unknown");
                reason["package_id"] = Value::String(format!("<PACKAGE_ID:{name}>"));
            }
            if reason.get("depends_on_package_id").is_some() {
                let name = reason["depends_on"].as_str().unwrap_or("unknown");
                reason["depends_on_package_id"] = Value::String(format!("<PACKAGE_ID:{name}>"));
            }
        }
    }
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
        "surface",
        "base_ref",
        "cargo_args",
        "surfaces_json",
        "scope_json",
    ];

    let mut lines = Vec::new();
    for key in ordered_keys {
        match key {
            "scope_json" => lines.push(format!("{}={}", key, serde_json::to_string(&scope_json)?)),
            "surfaces_json" => {
                let surfaces_json: Value = serde_json::from_str(
                    kv.get(key)
                        .ok_or_else(|| anyhow!("missing {} key in github-debug output", key))?,
                )?;
                lines.push(format!("{}={}", key, serde_json::to_string(&surfaces_json)?));
            }
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
        "surface",
        "base_ref",
        "cargo_args",
        "surfaces_json",
        "scope_json",
        "plan_json",
    ];

    let mut lines = Vec::new();
    for key in ordered_keys {
        match key {
            "scope_json" => lines.push(format!("{}={}", key, serde_json::to_string(&scope_json)?)),
            "surfaces_json" => {
                let surfaces_json: Value = serde_json::from_str(
                    kv.get(key)
                        .ok_or_else(|| anyhow!("missing {} key in github-debug output", key))?,
                )?;
                lines.push(format!("{}={}", key, serde_json::to_string(&surfaces_json)?));
            }
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
