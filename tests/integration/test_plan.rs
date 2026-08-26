//! Integration tests for the evidence-backed v8 planner contract.

use std::collections::{BTreeSet, HashMap};
use std::process::Command;

use anyhow::{Context as _, Result, anyhow, ensure};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::helpers::{TestWorkspace, git, run_cargo_rail};

const PLAN_V8_SCHEMA: &str = include_str!("../../schemas/plan-v8.schema.json");
const PLAN_VARIANTS_V1_SCHEMA: &str = include_str!("../../schemas/plan-variants-v1.schema.json");
const PLANNING_EVIDENCE_V1_SCHEMA: &str = include_str!("../../schemas/planning-evidence-v1.schema.json");

const CARGO_WORK: [&str; 6] = [
    "cargo.build",
    "cargo.clippy",
    "cargo.doc",
    "cargo.doctest",
    "cargo.package",
    "cargo.test",
];

fn plan(ws: &TestWorkspace, args: &[&str]) -> Result<Value> {
    let mut command = vec!["rail", "plan"];
    command.extend_from_slice(args);
    command.push("--json");
    let output = run_cargo_rail(&ws.path, &command)?;
    ensure!(
        output.status.success(),
        "plan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn sign_planning_evidence(mut manifest: Value) -> Result<Value> {
    manifest["identity"] = Value::String(String::new());
    fn canonicalize(value: Value) -> Value {
        match value {
            Value::Object(object) => {
                let mut entries = object.into_iter().collect::<Vec<_>>();
                entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
                Value::Object(
                    entries
                        .into_iter()
                        .map(|(key, value)| (key, canonicalize(value)))
                        .collect(),
                )
            }
            Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
            other => other,
        }
    }
    let encoded = serde_json::to_vec(&canonicalize(manifest.clone()))?;
    let digest = Sha256::digest(encoded);
    manifest["identity"] = Value::String(format!(
        "planning-evidence-v1:sha256:{}",
        digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>()
    ));
    Ok(manifest)
}

fn complete_evidence(plan: &Value, inputs: HashMap<&str, Vec<Value>>) -> Result<Value> {
    complete_evidence_with_base_model(plan, inputs, serde_json::json!({"packages": [], "edges": []}))
}

fn complete_evidence_with_base_model(
    plan: &Value,
    inputs: HashMap<&str, Vec<Value>>,
    base_model: Value,
) -> Result<Value> {
    let work = CARGO_WORK
        .into_iter()
        .map(|id| {
            (
                id.to_string(),
                serde_json::json!({
                    "complete": true,
                    "bypasses": [],
                    "inputs": inputs.get(id).cloned().unwrap_or_default(),
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    sign_planning_evidence(serde_json::json!({
        "planning_evidence_version": 1,
        "identity": "",
        "provider": {
            "identity": "cargo-rail-synthetic-test-provider-v1",
            "capabilities": [
                "build_script_reads",
                "compiler_reads",
                "proc_macro_reads",
                "process_domain",
                "rustc_dep_info",
                "rustdoc_dep_info"
            ]
        },
        "source_base": plan["inputs"]["base"],
        "cargo_identity": plan["inputs"]["cargo"],
        "cargo_configuration_identity": plan["inputs"]["configuration"],
        "toolchain_identity": plan["inputs"]["toolchain"],
        "target_identity": plan["inputs"]["target"],
        "platform": plan["inputs"]["platform"],
        "environment": [],
        "base_model": base_model,
        "work": work,
    }))
}

fn git_input_identity(ws: &TestWorkspace, revision: &str, path: &str) -> Result<String> {
    let output = git(&ws.path, &["ls-tree", revision, "--", path])?;
    let record = String::from_utf8(output.stdout)?;
    let mut fields = record.split_whitespace();
    let mode = fields.next().context("tree entry has no mode")?;
    ensure!(fields.next() == Some("blob"), "tree entry is not a blob: {record}");
    let object_id = fields.next().context("tree entry has no object ID")?;
    Ok(format!("git:{mode}:{object_id}"))
}

fn write_evidence(ws: &TestWorkspace, name: &str, evidence: &Value) -> Result<String> {
    let path = ws.path.join("target").join(name);
    std::fs::create_dir_all(path.parent().context("evidence path has no parent")?)?;
    std::fs::write(&path, serde_json::to_vec_pretty(evidence)?)?;
    Ok(path.to_str().context("non-UTF-8 evidence path")?.to_string())
}

fn generate_lockfile(ws: &TestWorkspace) -> Result<()> {
    let output = Command::new("cargo")
        .current_dir(&ws.path)
        .args(["generate-lockfile", "--offline"])
        .output()?;
    ensure!(
        output.status.success(),
        "cargo generate-lockfile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn test_plan_schema_command_matches_published_schema() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-schema-command")?;
        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--schema"])?;
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), PLAN_V8_SCHEMA);
        assert!(output.stderr.is_empty());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_v8_is_the_canonical_global_json_contract() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-v8-canonical-json")?;
        ws.add_crate("canonical", "0.1.0", &[])?;
        ws.commit("establish workspace")?;
        let plan = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(plan["plan_contract_version"], 8);
        assert!(plan.get("schema_version").is_none());
        let schema: Value = serde_json::from_str(PLAN_V8_SCHEMA)?;
        let validator = jsonschema::validator_for(&schema).map_err(|error| anyhow!("invalid schema: {error}"))?;
        let errors = validator
            .iter_errors(&plan)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        assert!(errors.is_empty(), "v8 plan failed schema: {errors:#?}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_surface_work_requires_explicit_enablement() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-surface-opt-in")?;
        ws.add_crate("surface-opt-in", "0.1.0", &[])?;
        ws.commit("establish workspace")?;
        ws.modify_file("surface-opt-in", "src/lib.rs", "pub fn changed() {}\n")?;
        assert_eq!(plan(&ws, &["--since", "HEAD"])?["work"]["surface"]["state"], "skipped");

        std::fs::create_dir_all(ws.path.join(".config"))?;
        std::fs::write(ws.path.join(".config/rail.toml"), "[surface]\nenabled = true\n")?;
        assert_eq!(plan(&ws, &["--since", "HEAD"])?["work"]["surface"]["state"], "required");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_bounds_cold_and_complete_evidence_incident() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-semver-policy-incident")?;
        ws.add_crate("incident", "0.1.0", &[])?;
        let config = |semver: &str| {
            format!(
                r#"[release]
semver_check = "{semver}"

[surface]
enabled = true

[plan.work.compatibility]
scope = "variants"
paths = ["tests/compatibility/**"]

[plan.work.release-archives]
scope = "variants"
paths = ["distribution/**"]
"#
            )
        };
        std::fs::write(ws.path.join(".config/rail.toml"), config("warn"))?;
        std::fs::write(ws.path.join("CHANGELOG.md"), "# Changelog\n")?;
        ws.commit("establish incident baseline")?;
        git(&ws.path, &["branch", "origin/main"])?;
        std::fs::write(ws.path.join(".config/rail.toml"), config("off"))?;
        std::fs::write(
            ws.path.join("CHANGELOG.md"),
            "# Changelog\n\n- Explain release policy.\n",
        )?;

        let cold = plan(&ws, &["--since", "origin/main"])?;
        assert_eq!(cold["work"]["release.semver"]["cause"], "changed_input");
        for id in CARGO_WORK {
            assert_eq!(cold["work"][id]["cause"], "incomplete_evidence", "{id}");
        }
        for id in ["compatibility", "release-archives", "surface"] {
            assert_eq!(cold["work"][id]["state"], "skipped", "{id}");
        }

        let evidence = complete_evidence(&cold, HashMap::new())?;
        let evidence_path = write_evidence(&ws, "incident-evidence.json", &evidence)?;
        let warm = plan(&ws, &["--since", "origin/main", "--evidence", &evidence_path])?;
        for id in CARGO_WORK {
            assert_eq!(warm["work"][id]["state"], "skipped", "{id}");
        }
        assert_eq!(warm["required"], serde_json::json!(["release.semver"]));
        assert_eq!(warm["inputs"]["evidence"], serde_json::json!([evidence["identity"]]));

        let schema: Value = serde_json::from_str(PLANNING_EVIDENCE_V1_SCHEMA)?;
        let validator = jsonschema::validator_for(&schema).map_err(|error| anyhow!("invalid schema: {error}"))?;
        assert!(validator.is_valid(&evidence));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_normalizes_retired_policy_only_in_historical_configuration() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-historical-retired-policy")?;
        ws.add_crate("historical-policy", "0.1.0", &[])?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[change-detection]\nconfidence_profile = \"strict\"\n",
        )?;
        ws.commit("establish retired planner policy")?;
        std::fs::write(ws.path.join(".config/rail.toml"), "")?;

        let planned = plan(&ws, &["--since", "HEAD"])?;
        assert!(planned["changes"]["config"].as_array().is_some_and(Vec::is_empty));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_observed_include_input_selects_exact_unit_and_rejects_stale_evidence() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-observed-include")?;
        let package = ws.add_crate("observed", "0.1.0", &[])?;
        std::fs::write(
            package.join("src/lib.rs"),
            "pub const GUIDE: &str = include_str!(\"guide.md\");\n",
        )?;
        std::fs::write(package.join("src/guide.md"), "before\n")?;
        ws.commit("establish observed input")?;
        let cold = plan(&ws, &["--since", "HEAD"])?;
        let input = serde_json::json!({
            "path": "crates/observed/src/guide.md",
            "identity": git_input_identity(&ws, "HEAD", "crates/observed/src/guide.md")?,
            "package": "observed@0.1.0#path:crates/observed",
            "target": "observed"
        });
        let evidence = complete_evidence_with_base_model(
            &cold,
            HashMap::from([("cargo.build", vec![input])]),
            serde_json::json!({
                "packages": [{
                    "key": "observed@0.1.0#path:crates/observed",
                    "name": "observed",
                    "root": "crates/observed",
                    "targets": [{"name": "observed", "kind": ["lib"], "src_path": "crates/observed/src/lib.rs"}]
                }],
                "edges": []
            }),
        )?;
        let evidence_path = write_evidence(&ws, "include-evidence.json", &evidence)?;
        std::fs::write(package.join("src/guide.md"), "after\n")?;
        let selected = plan(&ws, &["--since", "HEAD", "--evidence", &evidence_path])?;
        assert_eq!(selected["work"]["cargo.build"]["cause"], "changed_input");
        assert_eq!(
            selected["work"]["cargo.build"]["scope"]["selection"]["packages"][0]["name"],
            "observed"
        );
        assert_eq!(selected["work"]["cargo.test"]["state"], "skipped");

        let mut stale = evidence.clone();
        stale["source_base"] = Value::String("0".repeat(40));
        let stale = sign_planning_evidence(stale)?;
        let stale_path = write_evidence(&ws, "stale-evidence.json", &stale)?;
        let rejected = plan(&ws, &["--since", "HEAD", "--evidence", &stale_path])?;
        assert_eq!(rejected["work"]["cargo.test"]["cause"], "incomplete_evidence");
        assert!(rejected["inputs"]["evidence"].as_array().is_some_and(Vec::is_empty));

        let mut forged = evidence;
        forged["work"]["cargo.build"]["inputs"][0]["identity"] =
            Value::String(format!("git:100644:{}", "0".repeat(40)));
        let forged = sign_planning_evidence(forged)?;
        let forged_path = write_evidence(&ws, "forged-input-evidence.json", &forged)?;
        let rejected = plan(&ws, &["--since", "HEAD", "--evidence", &forged_path])?;
        assert_eq!(rejected["work"]["cargo.build"]["cause"], "incomplete_evidence");
        let evidence_id = rejected["work"]["cargo.build"]["evidence"][0]
            .as_str()
            .context("fallback evidence ID missing")?;
        assert_eq!(
            rejected["evidence"][evidence_id]["code"],
            "planning_evidence_input_identity_invalid"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_does_not_treat_package_containment_as_compiler_membership() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-containment-is-not-membership")?;
        let package = ws.add_crate("contained", "0.1.0", &[])?;
        std::fs::write(package.join("src/orphan.rs"), "pub fn not_a_module() {}\n")?;
        ws.commit("establish unreferenced package file")?;

        std::fs::write(package.join("src/orphan.rs"), "pub fn still_not_a_module() {}\n")?;
        let cold = plan(&ws, &["--since", "HEAD"])?;
        for id in [
            "cargo.build",
            "cargo.clippy",
            "cargo.doc",
            "cargo.doctest",
            "cargo.test",
        ] {
            assert_eq!(cold["work"][id]["cause"], "incomplete_evidence", "{id}");
            assert_eq!(
                cold["work"][id]["scope"]["selection"]["packages"][0]["name"], "contained",
                "{id}"
            );
        }
        assert_eq!(cold["work"]["cargo.package"]["cause"], "changed_input");

        let evidence = complete_evidence(&cold, HashMap::new())?;
        let evidence_path = write_evidence(&ws, "containment-evidence.json", &evidence)?;
        let warm = plan(&ws, &["--since", "HEAD", "--evidence", &evidence_path])?;
        for id in [
            "cargo.build",
            "cargo.clippy",
            "cargo.doc",
            "cargo.doctest",
            "cargo.test",
        ] {
            assert_eq!(warm["work"][id]["state"], "skipped", "{id}");
        }
        assert_eq!(warm["work"]["cargo.package"]["cause"], "changed_input");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_all_is_monotonic_and_variant_fallback_is_explicit() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-all-monotonic")?;
        ws.add_crate("all-case", "0.1.0", &[])?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[plan.work.compatibility]\nscope = 'variants'\npaths = ['tests/compatibility/**']\n",
        )?;
        ws.commit("establish work catalog")?;
        let normal = plan(&ws, &["--since", "HEAD"])?;
        let all = plan(&ws, &["--since", "HEAD", "--all"])?;
        let required = |value: &Value| {
            value["required"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<BTreeSet<_>>()
        };
        assert!(required(&normal).is_subset(&required(&all)));
        assert!(all["work"].as_object().is_some_and(|work| {
            work.values()
                .all(|decision| decision["state"] == "required" && decision["cause"] == "forced_all")
        }));
        assert_eq!(all["work"]["compatibility"]["scope"]["selection"]["kind"], "all");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_variant_catalogs_validate_against_published_schema() {
    let result: Result<()> = (|| {
        let schema: Value = serde_json::from_str(PLAN_VARIANTS_V1_SCHEMA)?;
        let validator = jsonschema::validator_for(&schema).map_err(|error| anyhow!("invalid schema: {error}"))?;
        for path in [
            "distribution/compatibility-plan-variants.json",
            "distribution/release-archive-plan-variants.json",
        ] {
            let catalog: Value = serde_json::from_slice(&std::fs::read(path)?)?;
            let errors = validator
                .iter_errors(&catalog)
                .map(|error| error.to_string())
                .collect::<Vec<_>>();
            assert!(errors.is_empty(), "{path} failed schema: {errors:#?}");
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_variant_catalog_identity_is_order_and_format_independent() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-variant-identity")?;
        ws.add_crate("variant-identity", "0.1.0", &[])?;
        std::fs::create_dir_all(ws.path.join("distribution"))?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[plan.work.compatibility]\nscope = 'variants'\npaths = ['tests/compatibility/**']\nvariant_catalog = 'distribution/variants.json'\n",
        )?;
        let left = serde_json::json!({
            "variant_catalog_version": 1,
            "work": "compatibility",
            "variants": [
                {"id": "linux", "dimensions": {"runner": "ubuntu-latest", "family": "compatibility"}, "paths": ["src/**", "Cargo.toml"]},
                {"id": "windows", "dimensions": {"family": "compatibility", "runner": "windows-latest"}, "paths": ["Cargo.toml"]}
            ]
        });
        std::fs::write(
            ws.path.join("distribution/variants.json"),
            serde_json::to_vec_pretty(&left)?,
        )?;
        ws.commit("establish variant catalog")?;
        let first = plan(&ws, &["--since", "HEAD"])?;

        let right = serde_json::json!({
            "work": "compatibility",
            "variants": [
                {"paths": ["Cargo.toml"], "dimensions": {"runner": "windows-latest", "family": "compatibility"}, "id": "windows"},
                {"paths": ["Cargo.toml", "src/**"], "id": "linux", "dimensions": {"family": "compatibility", "runner": "ubuntu-latest"}}
            ],
            "variant_catalog_version": 1
        });
        std::fs::write(ws.path.join("distribution/variants.json"), serde_json::to_vec(&right)?)?;
        let reordered = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(reordered["inputs"]["catalog"], first["inputs"]["catalog"]);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_variant_catalog_selects_exact_rows_for_changed_cargo_work() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-exact-variant-selection")?;
        ws.add_crate("exact-variant", "0.1.0", &[])?;
        std::fs::create_dir_all(ws.path.join("distribution"))?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[plan.work.compatibility]\nscope = 'variants'\ncargo = ['cargo.build']\nvariant_catalog = 'distribution/variants.json'\n",
        )?;
        std::fs::write(
            ws.path.join("distribution/variants.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "variant_catalog_version": 1,
                "work": "compatibility",
                "variants": [
                    {"id": "build", "dimensions": {"family": "compatibility", "runner": "ubuntu-latest"}, "cargo": ["cargo.build"]},
                    {"id": "filesystem", "dimensions": {"family": "filesystem", "runner": "ubuntu-latest"}, "paths": ["scripts/filesystem/**"]}
                ]
            }))?,
        )?;
        ws.commit("establish exact variant inputs")?;
        ws.modify_file("exact-variant", "src/lib.rs", "pub fn changed() {}\n")?;

        let planned = plan(&ws, &["--since", "HEAD"])?;
        let selection = &planned["work"]["compatibility"]["scope"]["selection"];
        assert_eq!(selection["kind"], "selected");
        assert_eq!(selection["variants"].as_array().map(Vec::len), Some(1));
        assert_eq!(selection["variants"][0]["id"], "build");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_declared_cargo_work_inherits_subscribed_cargo_scope() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-declared-cargo-scope")?;
        ws.add_crate("scope-a", "0.1.0", &[])?;
        ws.add_crate("scope-b", "0.1.0", &[("scope-a", r#"{ path = "../scope-a" }"#)])?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[plan.work.miri]\nscope = 'cargo'\ncargo = ['cargo.test']\n",
        )?;
        ws.commit("establish declared Cargo work")?;
        ws.modify_file("scope-a", "src/lib.rs", "pub fn changed() {}\n")?;

        let planned = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(planned["work"]["miri"]["state"], "required");
        assert_eq!(planned["work"]["miri"]["cause"], "changed_input");
        assert_eq!(
            planned["work"]["miri"]["scope"]["selection"], planned["work"]["cargo.test"]["scope"]["selection"],
            "Cargo-scoped declared work must reuse the subscribed decision's exact selector"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_propagates_cargo_domains_with_portable_selectors() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-cargo-domains")?;
        ws.add_crate("domain-a", "0.1.0", &[])?;
        ws.add_crate("domain-b", "0.1.0", &[("domain-a", r#"{ path = "../domain-a" }"#)])?;
        ws.commit("establish dependency graph")?;
        ws.modify_file("domain-a", "src/lib.rs", "pub fn changed() {}\n")?;
        let plan = plan(&ws, &["--since", "HEAD"])?;
        for work in [
            "cargo.build",
            "cargo.clippy",
            "cargo.doc",
            "cargo.doctest",
            "cargo.test",
        ] {
            let names = plan["work"][work]["scope"]["selection"]["packages"]
                .as_array()
                .context("package selectors missing")?
                .iter()
                .filter_map(|selector| selector["name"].as_str())
                .collect::<BTreeSet<_>>();
            assert_eq!(names, BTreeSet::from(["domain-a", "domain-b"]), "{work}");
        }
        assert!(
            plan["work"]["cargo.build"]["scope"]["selection"]["packages"]
                .as_array()
                .is_some_and(|packages| packages.iter().all(|package| package["key"]
                    .as_str()
                    .is_some_and(|key| !key.contains(ws.path.to_string_lossy().as_ref()))))
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_integration_target_and_manifest_noop_are_exact() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-target-and-manifest")?;
        let package = ws.add_crate("target-case", "0.1.0", &[])?;
        generate_lockfile(&ws)?;
        ws.commit("establish package")?;
        std::fs::create_dir_all(package.join("tests"))?;
        std::fs::write(package.join("tests/contract.rs"), "#[test]\nfn contract() {}\n")?;
        let target = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(target["work"]["cargo.build"]["state"], "skipped");
        assert_eq!(target["work"]["cargo.test"]["state"], "required");
        assert!(
            target["work"]["cargo.test"]["scope"]["selection"]["targets"]
                .as_array()
                .is_some_and(|targets| targets.iter().any(|target| target["name"] == "contract"))
        );

        git(&ws.path, &["clean", "-fd"])?;
        let manifest = std::fs::read_to_string(package.join("Cargo.toml"))?;
        std::fs::write(package.join("Cargo.toml"), format!("# formatting only\n{manifest}"))?;
        let formatting = plan(&ws, &["--since", "HEAD"])?;
        for work in CARGO_WORK.into_iter().chain(["dependency-policy"]) {
            assert_eq!(formatting["work"][work]["state"], "skipped", "{work}");
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_object_pair_is_checkout_independent_and_release_inputs_remain_distinct() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-history-and-release")?;
        let package = ws.add_crate("release-case", "0.1.0", &[])?;
        generate_lockfile(&ws)?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[plan.work.release-archives]\nscope = 'repository'\npaths = ['Cargo.toml', 'Cargo.lock', '.changes/**']\n",
        )?;
        let base = ws.commit("establish release baseline")?;
        std::fs::write(package.join("src/lib.rs"), "pub fn changed() {}\n")?;
        let head = ws.commit("change source")?;
        std::fs::write(package.join("src/lib.rs"), "uncommitted noise\n")?;
        let historical = plan(&ws, &["--from", &base, "--to", &head])?;
        assert_eq!(historical["inputs"]["head_commit"], head);
        assert_eq!(
            historical["work"]["cargo.build"]["scope"]["selection"]["kind"],
            "packages"
        );
        assert_eq!(historical["work"]["release-archives"]["state"], "skipped");

        git(&ws.path, &["reset", "--hard", "HEAD"])?;
        let root_manifest = std::fs::read_to_string(ws.path.join("Cargo.toml"))?;
        std::fs::write(
            ws.path.join("Cargo.toml"),
            format!("{root_manifest}\n[workspace.metadata.release-case]\nversion = 1\n"),
        )?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        std::fs::write(ws.path.join(".changes/release.md"), "release\n")?;
        let release = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(release["work"]["release-archives"]["state"], "required");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_object_pair_preserves_rename_and_copy_relations() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-object-relations")?;
        let package = ws.add_crate("relations", "0.1.0", &[])?;
        std::fs::write(package.join("src/former.rs"), "pub fn relation() {}\n")?;
        let base = ws.commit("establish relation source")?;
        git(
            &ws.path,
            &[
                "mv",
                "crates/relations/src/former.rs",
                "crates/relations/src/renamed.rs",
            ],
        )?;
        std::fs::write(package.join("src/copied.rs"), "pub fn relation() {}\n")?;
        let head = ws.commit("rename and copy source")?;

        let planned = plan(&ws, &["--from", &base, "--to", &head])?;
        let files = planned["changes"]["files"].as_array().context("file changes missing")?;
        let relation = |path: &str| {
            files
                .iter()
                .find(|change| change["path"] == path)
                .and_then(|change| change["relation"].as_str())
        };
        assert_eq!(
            relation("crates/relations/src/former.rs"),
            Some("renamed_to:crates/relations/src/renamed.rs")
        );
        assert_eq!(
            relation("crates/relations/src/renamed.rs"),
            Some("renamed_from:crates/relations/src/former.rs")
        );
        assert_eq!(
            relation("crates/relations/src/copied.rs"),
            Some("copied_from:crates/relations/src/former.rs")
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_object_pair_reads_member_manifests_from_objects() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-object-manifest-authority")?;
        let package = ws.add_crate("manifest-authority", "0.1.0", &[])?;
        let base = ws.commit("establish inherited workspace package fields")?;
        let root_manifest = std::fs::read_to_string(ws.path.join("Cargo.toml"))?;
        std::fs::write(
            ws.path.join("Cargo.toml"),
            root_manifest.replace("license = \"MIT\"", "license = \"Apache-2.0\""),
        )?;
        let head = ws.commit("change inherited workspace license")?;
        let expected = plan(&ws, &["--from", &base, "--to", &head])?;

        let member_manifest = std::fs::read_to_string(package.join("Cargo.toml"))?;
        std::fs::write(
            package.join("Cargo.toml"),
            member_manifest.replace("license.workspace = true", "license = \"MIT\""),
        )?;
        let with_checkout_noise = plan(&ws, &["--from", &base, "--to", &head])?;

        assert_eq!(with_checkout_noise, expected);
        assert_eq!(
            expected["work"]["cargo.build"]["scope"]["selection"]["packages"][0]["name"],
            "manifest-authority"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_portable_base_model_bounds_removed_member_impact() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-portable-base-model")?;
        ws.add_crate("base-a", "0.1.0", &[])?;
        let dependent = ws.add_crate("base-b", "0.1.0", &[("base-a", r#"{ path = "../base-a" }"#)])?;
        let base = ws.commit("establish base dependency")?;

        let root_manifest = std::fs::read_to_string(ws.path.join("Cargo.toml"))?;
        std::fs::write(
            ws.path.join("Cargo.toml"),
            root_manifest.replace("members = [\"crates/*\"]", "members = [\"crates/base-b\"]"),
        )?;
        let dependent_manifest = std::fs::read_to_string(dependent.join("Cargo.toml"))?;
        std::fs::write(
            dependent.join("Cargo.toml"),
            dependent_manifest.replace("base-a = { path = \"../base-a\" }\n", ""),
        )?;
        let head = ws.commit("remove base workspace member")?;

        let cold = plan(&ws, &["--from", &base, "--to", &head])?;
        assert_eq!(cold["work"]["cargo.build"]["scope"]["selection"]["kind"], "workspace");

        let base_model = serde_json::json!({
            "packages": [
                {
                    "key": "base-a@0.1.0#path:crates/base-a",
                    "name": "base-a",
                    "root": "crates/base-a",
                    "targets": [{"name": "base_a", "kind": ["lib"], "src_path": "crates/base-a/src/lib.rs"}]
                },
                {
                    "key": "base-b@0.1.0#path:crates/base-b",
                    "name": "base-b",
                    "root": "crates/base-b",
                    "targets": [{"name": "base_b", "kind": ["lib"], "src_path": "crates/base-b/src/lib.rs"}]
                }
            ],
            "edges": [{
                "dependency": "base-a@0.1.0#path:crates/base-a",
                "dependent": "base-b@0.1.0#path:crates/base-b",
                "domain": "build"
            }]
        });
        let evidence = complete_evidence_with_base_model(&cold, HashMap::new(), base_model)?;
        let evidence_path = write_evidence(&ws, "base-model-evidence.json", &evidence)?;
        let bounded = plan(&ws, &["--from", &base, "--to", &head, "--evidence", &evidence_path])?;
        for id in [
            "cargo.build",
            "cargo.clippy",
            "cargo.doc",
            "cargo.doctest",
            "cargo.test",
        ] {
            let selection = &bounded["work"][id]["scope"]["selection"];
            assert_eq!(selection["kind"], "packages", "{id}");
            assert_eq!(selection["packages"].as_array().map(Vec::len), Some(1), "{id}");
            assert_eq!(selection["packages"][0]["name"], "base-b", "{id}");
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}
