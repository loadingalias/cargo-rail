//! Integration tests for the evidence-backed v9 planner contract.

use std::collections::{BTreeSet, HashMap};
use std::io::Write as _;
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, anyhow, ensure};
use rscrypto::Sha256;
use serde_json::Value;

use crate::helpers::{TestWorkspace, cargo_rail_command, git, run_cargo_rail, run_cargo_rail_with_env};

const PLAN_V9_SCHEMA: &str = include_str!("../../schemas/plan-v9.schema.json");
const PLAN_VARIANTS_V1_SCHEMA: &str = include_str!("../../schemas/plan-variants-v1.schema.json");
const PLAN_VARIANTS_V2_SCHEMA: &str = include_str!("../../schemas/plan-variants-v2.schema.json");
const PLANNING_EVIDENCE_V1_SCHEMA: &str = include_str!("../../schemas/planning-evidence-v1.schema.json");

const CARGO_WORK: [&str; 6] = [
    "cargo.build",
    "cargo.clippy",
    "cargo.doc",
    "cargo.doctest",
    "cargo.package",
    "cargo.test",
];

#[test]
fn equivalent_configuration_spellings_keep_policy_but_change_source_binding() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("demo", "0.1.0")?;
        std::fs::create_dir_all(ws.path.join(".config"))?;
        let config = ws.path.join(".config/rail.toml");
        std::fs::write(&config, "[unify]\nmsrv = false\n[release]\npush = true\n")?;
        ws.commit("predecessor policy")?;
        let before = plan(&ws, &["--since", "HEAD"])?;
        let saved = write_saved_plan(&ws, "old-policy.json", &before)?;
        std::fs::write(
            &config,
            "[unify]\nmsrv_policy = { mode = 'disabled' }\n[release]\nremote_effects = 'push'\n",
        )?;
        let after = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(after["changes"]["config"], serde_json::json!([]));
        assert_ne!(before["identity"], after["identity"]);
        let verification = verify_saved_plan(&ws, &saved)?;
        assert!(
            !verification.status.success(),
            "saved plan accepted different config bytes: {verification:?}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn historical_configuration_uses_historical_split_and_transitive_host_paths() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("historical-config-paths")?;
        ws.add_crate("old-host", "0.1.0", &[])?;
        ws.add_crate("remaining", "0.1.0", &[])?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[unify]\npin_transitives = true\ntransitive_host = 'crates/old-host'\n[crates.old-host.split]\nremote = '../old-host'\nbranch = 'main'\nmode = 'single'\npaths = [{ crate = 'crates/old-host' }]\n",
        )?;
        let base = ws.commit("historical workspace")?;
        std::fs::remove_dir_all(ws.path.join("crates/old-host"))?;
        std::fs::write(ws.path.join(".config/rail.toml"), "")?;
        let head = ws.commit("remove former host")?;
        let value = plan(&ws, &["--since", &base])?;
        assert_eq!(value["plan_contract_version"], 9);
        let historical = plan(&ws, &["--from", &base, "--to", &head])?;
        assert_eq!(historical["plan_contract_version"], 9);
        assert!(!ws.path.join("crates/old-host").exists());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn historical_configuration_never_interprets_symlink_targets_as_manifest_bytes() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("historical-linked-manifest")?;
        ws.add_crate("demo", "0.1.0", &[])?;
        let manifest = ws.path.join("crates/demo/Cargo.toml");
        let original = std::fs::read(&manifest)?;
        let config = ws.path.join(".config/rail.toml");
        std::fs::write(
            &config,
            "[crates.demo.split]\nremote = '../demo'\nbranch = 'main'\nmode = 'single'\npaths = [{ crate = 'crates/demo' }]\n",
        )?;
        std::fs::remove_file(&manifest)?;
        std::os::unix::fs::symlink("[package]\nname = 'demo'\n", &manifest)?;
        let base = ws.commit("historical linked manifest")?;
        std::fs::remove_file(&manifest)?;
        std::fs::write(&manifest, original)?;
        std::fs::write(&config, "")?;
        ws.commit("regular current manifest")?;
        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", &base])?;
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("must be a regular Git file"), "{error}");
        assert!(error.contains(&base), "{error}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

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

fn write_saved_plan(ws: &TestWorkspace, name: &str, plan: &Value) -> Result<String> {
    let path = ws.path.join("target").join(name);
    std::fs::create_dir_all(path.parent().context("saved plan path has no parent")?)?;
    std::fs::write(&path, serde_json::to_vec_pretty(plan)?)?;
    Ok(path.to_str().context("saved plan path is not UTF-8")?.to_string())
}

fn verify_saved_plan(ws: &TestWorkspace, path: &str) -> Result<std::process::Output> {
    run_cargo_rail(&ws.path, &["rail", "plan", "--verify", path])
}

fn verify_saved_plan_stdin(ws: &TestWorkspace, plan: &[u8]) -> Result<std::process::Output> {
    let mut child = cargo_rail_command(&ws.path)?
        .args(["rail", "plan", "--verify", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .context("saved-plan verifier has no stdin")?
        .write_all(plan)?;
    Ok(child.wait_with_output()?)
}

#[test]
fn test_plan_identity_ignores_equivalent_empty_cargo_home_locations() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-cargo-home-equivalence")?;
        ws.add_crate("portable", "0.1.0", &[])?;
        ws.commit("establish Cargo home equivalence fixture")?;
        let first_home = tempfile::tempdir()?;
        let second_home = tempfile::tempdir()?;

        let first = run_cargo_rail_with_env(
            &ws.path,
            &["rail", "plan", "--since", "HEAD", "--json"],
            &[(
                "CARGO_HOME",
                first_home.path().to_str().context("first Cargo home is not UTF-8")?,
            )],
        )?;
        ensure!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
        let second = run_cargo_rail_with_env(
            &ws.path,
            &["rail", "plan", "--since", "HEAD", "--json"],
            &[(
                "CARGO_HOME",
                second_home.path().to_str().context("second Cargo home is not UTF-8")?,
            )],
        )?;
        ensure!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
        let first: Value = serde_json::from_slice(&first.stdout)?;
        let second: Value = serde_json::from_slice(&second.stdout)?;

        assert_eq!(first["inputs"]["configuration"], second["inputs"]["configuration"]);
        assert_eq!(first["identity"], second["identity"]);
        Ok(())
    })();
    super::helpers::finish_test(result);
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
    let digest = Sha256::digest(&encoded);
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
        assert_eq!(String::from_utf8_lossy(&output.stdout), PLAN_V9_SCHEMA);
        assert!(output.stderr.is_empty());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_published_plan_variants_v1_schema_remains_exactly_available() {
    let schema: Value = serde_json::from_str(PLAN_VARIANTS_V1_SCHEMA).expect("valid historical schema");
    jsonschema::validator_for(&schema).expect("valid historical JSON Schema");
    assert_eq!(
        Sha256::digest(PLAN_VARIANTS_V1_SCHEMA.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "e25351a0a1d6872a3ce83ccc589faa0b724cabdeecd87da86908d7466ad8903f"
    );
}

#[test]
fn test_plan_v9_is_the_canonical_global_json_contract() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-v9-canonical-json")?;
        ws.add_crate("canonical", "0.1.0", &[])?;
        ws.commit("establish workspace")?;
        let plan = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(plan["plan_contract_version"], 9);
        assert!(plan.get("schema_version").is_none());
        let schema: Value = serde_json::from_str(PLAN_V9_SCHEMA)?;
        let validator = jsonschema::validator_for(&schema).map_err(|error| anyhow!("invalid schema: {error}"))?;
        let errors = validator
            .iter_errors(&plan)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        assert!(errors.is_empty(), "v9 plan failed schema: {errors:#?}");
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
fn test_plan_decodes_exact_v0_25_configuration_from_git_history() {
    let result: Result<()> = (|| {
        const TAGGED_CONFIG: &[u8] = include_bytes!("../fixtures/config/v0.25.0/rail.toml");

        let ws = TestWorkspace::new_named("plan-v0-25-config")?;
        ws.add_crate("historical-config", "0.1.0", &[])?;
        std::fs::write(ws.path.join(".config/rail.toml"), TAGGED_CONFIG)?;
        ws.commit("record exact v0.25 configuration")?;

        std::fs::write(ws.path.join(".config/rail.toml"), "")?;
        let planned = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(planned["plan_contract_version"], 9);
        assert!(
            planned["changes"]["files"]
                .as_array()
                .is_some_and(|changed| changed.iter().any(|entry| entry["path"] == ".config/rail.toml"))
        );
        let rendered = serde_json::to_string(&planned)?;
        assert!(rendered.contains("require_change_files"));
        assert!(rendered.contains("unconventional_commits"));

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
paths = ["deliverables/**"]

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
            "[plan.work.compatibility]\nscope = 'variants'\npaths = ['deliverables/**']\n",
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
fn test_plan_variant_catalog_identity_is_order_and_format_independent() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-variant-identity")?;
        ws.add_crate("variant-identity", "0.1.0", &[])?;
        std::fs::create_dir_all(ws.path.join("distribution"))?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[plan.work.compatibility]\nscope = 'variants'\npaths = ['deliverables/**']\nvariant_catalog = 'distribution/variants.json'\n",
        )?;
        let left = serde_json::json!({
            "variant_catalog_version": 2,
            "work": "compatibility",
            "variants": [
                {"id": "linux", "dimensions": {"runner": "ubuntu-latest", "family": "compatibility"}, "external_paths": ["src/**", "Cargo.toml"]},
                {"id": "windows", "dimensions": {"family": "compatibility", "runner": "windows-latest"}, "external_paths": ["Cargo.toml"]}
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
                {"external_paths": ["Cargo.toml"], "dimensions": {"runner": "windows-latest", "family": "compatibility"}, "id": "windows"},
                {"external_paths": ["Cargo.toml", "src/**"], "id": "linux", "dimensions": {"family": "compatibility", "runner": "ubuntu-latest"}}
            ],
            "variant_catalog_version": 2
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
            "[plan.work.compatibility]\nscope = 'variants'\nvariant_catalog = 'distribution/variants.json'\n",
        )?;
        std::fs::write(
            ws.path.join("distribution/variants.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "variant_catalog_version": 2,
                "work": "compatibility",
                "variants": [
                    {"id": "build", "dimensions": {"family": "compatibility", "runner": "ubuntu-latest"}, "cargo_roots": [{"package": "exact-variant"}]},
                    {"id": "filesystem", "dimensions": {"family": "filesystem", "runner": "ubuntu-latest"}, "external_paths": ["scripts/filesystem/**"]}
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
fn test_plan_variant_catalog_widens_only_for_unattributed_required_inputs() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-variant-input-attribution")?;
        ws.add_crate("variant-attribution", "0.1.0", &[])?;
        for directory in ["ci/shared", "ci/linux", "ci/windows", "docs"] {
            std::fs::create_dir_all(ws.path.join(directory))?;
        }
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[plan.work.ci-suite]\nscope = 'variants'\npaths = ['ci/shared/**']\nvariant_catalog = 'distribution/variants.json'\n",
        )?;
        std::fs::create_dir_all(ws.path.join("distribution"))?;
        std::fs::write(
            ws.path.join("distribution/variants.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "variant_catalog_version": 2,
                "work": "ci-suite",
                "variants": [
                    {"id": "linux", "dimensions": {"runner": "ubuntu-latest"}, "external_paths": ["ci/linux/**"]},
                    {"id": "windows", "dimensions": {"runner": "windows-latest"}, "external_paths": ["ci/windows/**"]}
                ]
            }))?,
        )?;
        for path in [
            "ci/shared/run.sh",
            "ci/linux/run.sh",
            "ci/windows/run.ps1",
            "docs/unrelated.md",
        ] {
            std::fs::write(ws.path.join(path), "before\n")?;
        }
        ws.commit("establish variant input attribution")?;

        let restore = |paths: &[&str]| -> Result<()> {
            for path in paths {
                std::fs::write(ws.path.join(path), "before\n")?;
            }
            Ok(())
        };
        let selected_ids = |plan: &Value| {
            plan["work"]["ci-suite"]["scope"]["selection"]["variants"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|variant| variant["id"].as_str().map(str::to_string))
                .collect::<Vec<_>>()
        };

        std::fs::write(ws.path.join("ci/shared/run.sh"), "shared change\n")?;
        let shared_only = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(shared_only["work"]["ci-suite"]["scope"]["selection"]["kind"], "all");
        restore(&["ci/shared/run.sh"])?;

        std::fs::write(ws.path.join("ci/linux/run.sh"), "linux change\n")?;
        let variant_only = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(selected_ids(&variant_only), ["linux"]);
        restore(&["ci/linux/run.sh"])?;

        std::fs::write(ws.path.join("ci/shared/run.sh"), "shared change\n")?;
        std::fs::write(ws.path.join("ci/linux/run.sh"), "linux change\n")?;
        let mixed = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(mixed["work"]["ci-suite"]["scope"]["selection"]["kind"], "all");
        restore(&["ci/shared/run.sh", "ci/linux/run.sh"])?;

        std::fs::write(ws.path.join("ci/linux/run.sh"), "linux change\n")?;
        std::fs::write(ws.path.join("ci/windows/run.ps1"), "windows change\n")?;
        let multiple = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(selected_ids(&multiple), ["linux", "windows"]);
        restore(&["ci/linux/run.sh", "ci/windows/run.ps1"])?;

        std::fs::write(ws.path.join("ci/linux/run.sh"), "linux change\n")?;
        std::fs::write(ws.path.join("docs/unrelated.md"), "unrelated change\n")?;
        let unrelated = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(selected_ids(&unrelated), ["linux"]);
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
fn test_declared_cargo_work_propagates_incomplete_subscribed_scope() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-declared-incomplete-cargo-scope")?;
        ws.add_crate("scope", "0.1.0", &[])?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[plan.work.miri]\nscope = 'cargo'\ncargo = ['cargo.test']\n",
        )?;
        ws.commit("establish declared Cargo subscription")?;
        std::fs::write(
            ws.path.join(".config/nextest.toml"),
            "[profile.default]\nfail-fast = true\n",
        )?;

        let planned = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(planned["work"]["cargo.test"]["cause"], "incomplete_evidence");
        assert_eq!(planned["work"]["miri"]["state"], "required");
        assert_eq!(planned["work"]["miri"]["cause"], "incomplete_evidence");
        assert_eq!(
            planned["work"]["miri"]["scope"]["selection"],
            planned["work"]["cargo.test"]["scope"]["selection"]
        );
        assert_eq!(planned["work"]["miri"]["scope"]["selection"]["kind"], "workspace");
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
            let attribution = plan["attribution"][work]["selections"]
                .as_array()
                .context("scope attribution missing")?;
            let direct = attribution
                .iter()
                .find(|item| item["subject"].as_str().is_some_and(|key| key.starts_with("domain-a@")))
                .context("direct package missing")?;
            let dependent = attribution
                .iter()
                .find(|item| item["subject"].as_str().is_some_and(|key| key.starts_with("domain-b@")))
                .context("dependent package missing")?;
            assert_eq!(direct["relation"], "direct");
            assert_eq!(dependent["relation"], "dependency");
            assert_eq!(dependent["origin"], direct["subject"]);
            let schema: Value = serde_json::from_str(PLAN_V9_SCHEMA)?;
            assert!(
                jsonschema::validator_for(&schema)
                    .map_err(|error| anyhow!("invalid schema: {error}"))?
                    .is_valid(&plan)
            );
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
fn test_plan_cargo_selectors_exclude_non_workspace_dependency_packages() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-non-workspace-dependency")?;
        let root_manifest = std::fs::read_to_string(ws.path.join("Cargo.toml"))?;
        std::fs::write(
            ws.path.join("Cargo.toml"),
            root_manifest.replace(
                "members = [\"crates/*\"]",
                "members = [\"crates/*\"]\nexclude = [\"vendor/external\"]",
            ),
        )?;
        let external = ws.path.join("vendor/external");
        std::fs::create_dir_all(external.join("src"))?;
        std::fs::write(
            external.join("Cargo.toml"),
            "[package]\nname = \"external\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )?;
        std::fs::write(external.join("src/lib.rs"), "pub fn external() {}\n")?;
        ws.add_crate(
            "consumer",
            "0.1.0",
            &[("external", r#"{ path = "../../vendor/external" }"#)],
        )?;
        generate_lockfile(&ws)?;
        ws.commit("establish non-workspace dependency")?;

        let external_manifest = std::fs::read_to_string(external.join("Cargo.toml"))?;
        std::fs::write(
            external.join("Cargo.toml"),
            external_manifest.replace("version = \"0.1.0\"", "version = \"0.2.0\""),
        )?;
        generate_lockfile(&ws)?;
        git(&ws.path, &["add", "vendor/external/Cargo.toml"])?;
        git(&ws.path, &["commit", "-m", "update non-workspace dependency manifest"])?;

        let plan = plan(&ws, &["--since", "HEAD"])?;
        for work in [
            "cargo.build",
            "cargo.clippy",
            "cargo.doc",
            "cargo.doctest",
            "cargo.test",
        ] {
            let packages = plan["work"][work]["scope"]["selection"]["packages"]
                .as_array()
                .with_context(|| format!("{work} package selectors missing"))?;
            let names = packages
                .iter()
                .filter_map(|package| package["name"].as_str())
                .collect::<Vec<_>>();
            assert_eq!(names, ["consumer"], "{work}");
        }
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
fn test_plan_object_pair_uses_only_to_tree_authority() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-object-complete-head-authority")?;
        ws.add_crate("base-member", "0.1.0", &[])?;
        let base = ws.commit("establish object planning base")?;

        ws.add_crate("head-member", "0.1.0", &[])?;
        std::fs::create_dir_all(ws.path.join("distribution"))?;
        std::fs::create_dir_all(ws.path.join(".cargo"))?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[plan.work.historical]\nscope = 'variants'\nvariant_catalog = 'distribution/historical.json'\n",
        )?;
        std::fs::write(
            ws.path.join("distribution/historical.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "variant_catalog_version": 2,
                "work": "historical",
                "variants": [{
                    "id": "head",
                    "dimensions": {"runner": "head-runner"},
                    "cargo_roots": [{"package": "head-member"}]
                }]
            }))?,
        )?;
        std::fs::write(
            ws.path.join(".cargo/config.toml"),
            "[build]\ntarget-dir = 'target/head'\n",
        )?;
        let head = ws.commit("establish complete object planning head")?;

        let expected = plan(&ws, &["--from", &base, "--to", &head])?;
        assert_eq!(expected["inputs"]["head"], head);
        assert_eq!(expected["work"]["historical"]["state"], "required");
        assert_eq!(expected["work"]["historical"]["scope"]["selection"]["kind"], "all");
        let selected_packages = expected["work"]["cargo.test"]["scope"]["selection"]["packages"]
            .as_array()
            .context("historical Cargo package selection missing")?;
        assert!(selected_packages.iter().any(|package| package["name"] == "head-member"));

        let root_manifest = std::fs::read_to_string(ws.path.join("Cargo.toml"))?;
        std::fs::write(
            ws.path.join("Cargo.toml"),
            root_manifest.replace("members = [\"crates/*\"]", "members = [\"crates/base-member\"]"),
        )?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[plan.work.live]\nscope = 'repository'\npaths = ['live/**']\n",
        )?;
        std::fs::write(ws.path.join("distribution/historical.json"), b"not json")?;
        std::fs::write(
            ws.path.join(".cargo/config.toml"),
            "[build]\ntarget-dir = 'target/live'\n",
        )?;

        let with_conflicting_checkout = plan(&ws, &["--from", &base, "--to", &head])?;
        assert_eq!(with_conflicting_checkout, expected);

        let clone_root = tempfile::TempDir::new()?;
        let clone = clone_root.path().join("clone");
        let output = Command::new("git")
            .args(["clone", "--quiet", "--no-local"])
            .arg(&ws.path)
            .arg(&clone)
            .output()?;
        ensure!(
            output.status.success(),
            "clone failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let cloned = run_cargo_rail(&clone, &["rail", "plan", "--from", &base, "--to", &head, "--json"])?;
        ensure!(
            cloned.status.success(),
            "cloned historical plan failed: {}",
            String::from_utf8_lossy(&cloned.stderr)
        );
        let cloned: Value = serde_json::from_slice(&cloned.stdout)?;
        assert_eq!(cloned, expected);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_saved_worktree_plan_rejects_every_git_drift_layer() {
    let result: Result<()> = (|| {
        for drift in ["unstaged", "staged", "untracked", "deleted", "renamed"] {
            let ws = TestWorkspace::new_named(&format!("plan-verify-{drift}"))?;
            let package = ws.add_crate("verify", "0.1.0", &[])?;
            generate_lockfile(&ws)?;
            ws.commit("establish saved-plan verification fixture")?;
            let saved = plan(&ws, &["--since", "HEAD"])?;
            let path = write_saved_plan(&ws, "saved-plan.json", &saved)?;
            let unchanged = verify_saved_plan(&ws, &path)?;
            ensure!(
                unchanged.status.success(),
                "unchanged {drift} fixture failed verification: {}",
                String::from_utf8_lossy(&unchanged.stderr)
            );

            match drift {
                "unstaged" => std::fs::write(package.join("src/lib.rs"), "pub fn unstaged() {}\n")?,
                "staged" => {
                    std::fs::write(package.join("src/lib.rs"), "pub fn staged() {}\n")?;
                    git(&ws.path, &["add", "crates/verify/src/lib.rs"])?;
                }
                "untracked" => std::fs::write(package.join("src/untracked.rs"), "pub fn untracked() {}\n")?,
                "deleted" => std::fs::remove_file(package.join("src/lib.rs"))?,
                "renamed" => std::fs::rename(package.join("src/lib.rs"), package.join("src/renamed.rs"))?,
                other => return Err(anyhow!("unknown saved-plan drift fixture '{other}'")),
            }
            let rejected = verify_saved_plan(&ws, &path)?;
            assert_eq!(rejected.status.code(), Some(2), "{drift} drift was accepted");
            assert!(rejected.stdout.is_empty(), "{drift} verification emitted stdout");
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_saved_plan_from_stdin_preserves_checkout_verification() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-verify-stdin")?;
        let package = ws.add_crate("verify", "0.1.0", &[])?;
        generate_lockfile(&ws)?;
        ws.commit("establish stdin saved-plan verification fixture")?;
        let saved = serde_json::to_vec(&plan(&ws, &["--since", "HEAD"])?)?;

        let unchanged = verify_saved_plan_stdin(&ws, &saved)?;
        ensure!(
            unchanged.status.success(),
            "unchanged stdin plan failed verification: {}",
            String::from_utf8_lossy(&unchanged.stderr)
        );
        ensure!(unchanged.stdout.is_empty(), "stdin verification emitted stdout");

        std::fs::write(package.join("src/lib.rs"), "pub fn drift() {}\n")?;
        let rejected = verify_saved_plan_stdin(&ws, &saved)?;
        assert_eq!(rejected.status.code(), Some(2));
        assert!(rejected.stdout.is_empty());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_saved_worktree_plan_rejects_executable_mode_drift() {
    use std::os::unix::fs::PermissionsExt as _;

    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-verify-executable-mode")?;
        let package = ws.add_crate("verify", "0.1.0", &[])?;
        generate_lockfile(&ws)?;
        ws.commit("establish executable-mode fixture")?;
        let saved = plan(&ws, &["--since", "HEAD"])?;
        let path = write_saved_plan(&ws, "saved-plan.json", &saved)?;
        let source = package.join("src/lib.rs");
        let mut permissions = std::fs::metadata(&source)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(source, permissions)?;

        let rejected = verify_saved_plan(&ws, &path)?;
        assert_eq!(rejected.status.code(), Some(2));
        assert!(rejected.stdout.is_empty());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_saved_object_plan_requires_clean_exact_head_and_ignores_generated_roots() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-verify-object")?;
        let package = ws.add_crate("verify", "0.1.0", &[])?;
        generate_lockfile(&ws)?;
        let base = ws.commit("establish object verification base")?;
        std::fs::write(package.join("src/lib.rs"), "pub fn head() {}\n")?;
        let head = ws.commit("establish object verification head")?;
        let saved = plan(&ws, &["--from", &base, "--to", &head])?;
        let path = write_saved_plan(&ws, "saved-object-plan.json", &saved)?;

        std::fs::create_dir_all(ws.path.join("target/generated"))?;
        std::fs::write(ws.path.join("target/generated/ignored"), b"generated")?;
        let unchanged = verify_saved_plan(&ws, &path)?;
        ensure!(
            unchanged.status.success(),
            "clean object plan failed: {}",
            String::from_utf8_lossy(&unchanged.stderr)
        );

        std::fs::write(ws.path.join("execution-drift.txt"), b"drift")?;
        let dirty = verify_saved_plan(&ws, &path)?;
        assert_eq!(dirty.status.code(), Some(2));
        assert!(dirty.stdout.is_empty());
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
        ws.add_crate("base-c", "0.1.0", &[])?;
        std::fs::create_dir_all(ws.path.join("distribution"))?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[plan.work.deliverables]\nscope = 'variants'\nvariant_catalog = 'distribution/deliverables.json'\n",
        )?;
        std::fs::write(
            ws.path.join("distribution/deliverables.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "variant_catalog_version": 2,
                "work": "deliverables",
                "variants": [
                    {"id": "affected", "dimensions": {}, "cargo_roots": [{"package": "base-b"}]},
                    {"id": "unrelated", "dimensions": {}, "cargo_roots": [{"package": "base-c"}]}
                ]
            }))?,
        )?;
        let base = ws.commit("establish base dependency")?;

        let root_manifest = std::fs::read_to_string(ws.path.join("Cargo.toml"))?;
        std::fs::write(
            ws.path.join("Cargo.toml"),
            root_manifest.replace(
                "members = [\"crates/*\"]",
                "members = [\"crates/base-b\", \"crates/base-c\"]",
            ),
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
        let variants = bounded["work"]["deliverables"]["scope"]["selection"]["variants"]
            .as_array()
            .context("deliverable variants missing")?
            .iter()
            .filter_map(|variant| variant["id"].as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(variants, BTreeSet::from(["affected", "unrelated"]));
        let evidence_id = bounded["work"]["deliverables"]["evidence"][0]
            .as_str()
            .context("deliverable evidence ID missing")?;
        assert!(
            bounded["evidence"][evidence_id]["input"]
                .as_str()
                .is_some_and(|input| input.contains("cargo:workspace->variant:affected"))
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_variant_catalog_v2_rejects_work_level_cargo_subscriptions() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-variant-v2-cargo-subscription")?;
        ws.add_crate("demo", "0.1.0", &[])?;
        std::fs::create_dir_all(ws.path.join("distribution"))?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[plan.work.deliverables]\nscope = 'variants'\ncargo = ['cargo.build']\nvariant_catalog = 'distribution/deliverables.json'\n",
        )?;
        std::fs::write(
            ws.path.join("distribution/deliverables.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "variant_catalog_version": 2,
                "work": "deliverables",
                "variants": [{
                    "id": "demo",
                    "dimensions": {},
                    "external_paths": ["crates/demo/**"]
                }]
            }))?,
        )?;
        ws.commit("establish invalid v2 Cargo subscription")?;

        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--json"])?;
        assert_eq!(output.status.code(), Some(2));
        let error: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(
            error["message"],
            "variant catalog 'distribution/deliverables.json' requires plan.work.deliverables.cargo to be empty; variant Cargo impact derives only from typed cargo_roots"
        );
        assert!(error.get("plan_contract_version").is_none());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_variant_catalog_v2_resolves_cargo_roots_fail_closed() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-variant-v2-root-validation")?;
        let package = ws.add_crate("demo", "0.1.0", &[])?;
        std::fs::write(package.join("src/main.rs"), "fn main() {}\n")?;
        std::fs::create_dir_all(ws.path.join("distribution"))?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[plan.work.deliverables]\nscope = 'variants'\nvariant_catalog = 'distribution/deliverables.json'\n",
        )?;
        let catalog_path = ws.path.join("distribution/deliverables.json");
        let write_catalog = |roots: Value| -> Result<()> {
            std::fs::write(
                &catalog_path,
                serde_json::to_vec_pretty(&serde_json::json!({
                    "variant_catalog_version": 2,
                    "work": "deliverables",
                    "variants": [{"id": "demo", "dimensions": {}, "cargo_roots": roots}]
                }))?,
            )?;
            Ok(())
        };
        write_catalog(serde_json::json!([{"package": "missing"}]))?;
        ws.commit("establish invalid catalog")?;
        let unknown_package = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--json"])?;
        assert_eq!(unknown_package.status.code(), Some(2));
        let unknown_package: Value = serde_json::from_slice(&unknown_package.stdout)?;
        assert!(
            unknown_package["message"]
                .as_str()
                .is_some_and(|message| message.contains("unknown workspace package 'missing'"))
        );

        write_catalog(serde_json::json!([{
            "package": "demo",
            "target": {"name": "missing", "kind": "bin"}
        }]))?;
        let unknown_target = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--json"])?;
        assert_eq!(unknown_target.status.code(), Some(2));
        let unknown_target: Value = serde_json::from_slice(&unknown_target.stdout)?;
        assert!(
            unknown_target["message"]
                .as_str()
                .is_some_and(|message| message.contains("unknown target 'bin:missing'"))
        );

        write_catalog(serde_json::json!([{"package": "demo"}, {"package": "demo"}]))?;
        let duplicate = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--json"])?;
        assert_eq!(duplicate.status.code(), Some(2));
        let duplicate: Value = serde_json::from_slice(&duplicate.stdout)?;
        assert!(
            duplicate["message"]
                .as_str()
                .is_some_and(|message| message.contains("duplicate Cargo roots"))
        );

        write_catalog(serde_json::json!([{"package": "demo", "features": ["missing"]}]))?;
        let unknown_feature = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--json"])?;
        assert_eq!(unknown_feature.status.code(), Some(2));
        let unknown_feature: Value = serde_json::from_slice(&unknown_feature.stdout)?;
        assert!(
            unknown_feature["message"]
                .as_str()
                .is_some_and(|message| message.contains("does not declare feature 'missing'"))
        );

        write_catalog(serde_json::json!([{"package": "demo", "features": ["one", "one"]}]))?;
        let duplicate_feature = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--json"])?;
        assert_eq!(duplicate_feature.status.code(), Some(2));
        let duplicate_feature: Value = serde_json::from_slice(&duplicate_feature.stdout)?;
        assert!(
            duplicate_feature["message"]
                .as_str()
                .is_some_and(|message| message.contains("duplicate selector 'one'"))
        );

        write_catalog(serde_json::json!([{
            "package": "demo",
            "manifest": "fuzz/demo/Cargo.toml"
        }]))?;
        let unregistered_manifest = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--json"])?;
        assert_eq!(unregistered_manifest.status.code(), Some(2));
        let unregistered_manifest: Value = serde_json::from_slice(&unregistered_manifest.stdout)?;
        assert!(
            unregistered_manifest["message"]
                .as_str()
                .is_some_and(|message| message.contains("is not registered in release.auxiliary_cargo_manifests"))
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_variant_feature_roots_select_exact_source_profiles_and_widen_unknowns() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-feature-root-profiles")?;
        let package = ws.add_crate("demo", "0.1.0", &[])?;
        let manifest = std::fs::read_to_string(package.join("Cargo.toml"))?;
        std::fs::write(
            package.join("Cargo.toml"),
            format!("{manifest}\n[features]\ndefault = []\nrsa = []\nsha2 = []\nsignatures = [\"rsa\"]\n"),
        )?;
        std::fs::write(
            package.join("src/lib.rs"),
            "#[cfg(feature = \"rsa\")]\npub mod rsa;\n#[cfg(feature = \"sha2\")]\npub mod sha2;\n",
        )?;
        std::fs::write(package.join("src/rsa.rs"), "pub fn value() -> usize { 1 }\n")?;
        std::fs::write(package.join("src/sha2.rs"), "pub fn value() -> usize { 2 }\n")?;
        std::fs::create_dir_all(ws.path.join("distribution"))?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[plan.work.profiles]\nscope = 'variants'\npaths = ['crates/demo/**']\nvariant_catalog = 'distribution/profiles.json'\n",
        )?;
        std::fs::write(
            ws.path.join("distribution/profiles.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "variant_catalog_version": 2,
                "work": "profiles",
                "variants": [
                    {"id": "hashes", "dimensions": {}, "cargo_roots": [{"package": "demo", "features": ["sha2"]}]},
                    {"id": "minimal", "dimensions": {}, "cargo_roots": [{"package": "demo", "features": []}]},
                    {"id": "signatures", "dimensions": {}, "cargo_roots": [{"package": "demo", "features": ["signatures"]}]}
                ]
            }))?,
        )?;
        ws.commit("establish exact feature profiles")?;

        ws.modify_file("demo", "src/rsa.rs", "pub fn value() -> usize { 3 }\n")?;
        let exact = plan(&ws, &["--since", "HEAD"])?;
        let selection = &exact["work"]["profiles"]["scope"]["selection"];
        assert_eq!(selection["kind"], "selected");
        assert_eq!(
            selection["variants"]
                .as_array()
                .context("feature variants missing")?
                .iter()
                .filter_map(|variant| variant["id"].as_str())
                .collect::<Vec<_>>(),
            ["signatures"]
        );

        ws.commit("change exact feature source")?;
        std::fs::write(package.join("src/generated.rs"), "pub fn generated() {}\n")?;
        let unknown = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(unknown["work"]["profiles"]["scope"]["selection"]["kind"], "all");

        ws.commit("add unattributed Rust source")?;
        let manifest = std::fs::read_to_string(package.join("Cargo.toml"))?;
        std::fs::write(package.join("Cargo.toml"), format!("{manifest}\n# policy change\n"))?;
        let manifest = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(manifest["work"]["profiles"]["scope"]["selection"]["kind"], "all");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_variant_auxiliary_cargo_roots_follow_registered_feature_resolutions() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-auxiliary-roots")?;
        let package = ws.add_crate("demo", "0.1.0", &[])?;
        let manifest = std::fs::read_to_string(package.join("Cargo.toml"))?;
        std::fs::write(
            package.join("Cargo.toml"),
            format!("{manifest}\n[features]\ndefault = []\nrsa = []\nsha2 = []\n"),
        )?;
        std::fs::write(
            package.join("src/lib.rs"),
            "#[cfg(feature = \"rsa\")]\npub mod rsa;\n#[cfg(feature = \"sha2\")]\npub mod sha2;\n",
        )?;
        std::fs::write(package.join("src/rsa.rs"), "pub fn value() -> usize { 1 }\n")?;
        std::fs::write(package.join("src/sha2.rs"), "pub fn value() -> usize { 2 }\n")?;

        for (name, feature) in [("fuzz-rsa", "rsa"), ("fuzz-sha2", "sha2")] {
            let root = ws.path.join("fuzz-packages").join(name);
            std::fs::create_dir_all(root.join("src"))?;
            std::fs::write(
                root.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ndemo = {{ path = \"../../crates/demo\", default-features = false, features = [\"{feature}\"] }}\n\n[workspace]\n"
                ),
            )?;
            std::fs::write(root.join("src/lib.rs"), "pub fn harness() {}\n")?;
            let output = Command::new("cargo")
                .args(["generate-lockfile", "--manifest-path"])
                .arg(root.join("Cargo.toml"))
                .output()?;
            ensure!(
                output.status.success(),
                "auxiliary lockfile generation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        std::fs::create_dir_all(ws.path.join("distribution"))?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[release]\nauxiliary_cargo_manifests = ['fuzz-packages/fuzz-rsa/Cargo.toml', 'fuzz-packages/fuzz-sha2/Cargo.toml']\n\n[plan.work.assurance]\nscope = 'variants'\npaths = ['crates/demo/**', 'fuzz-packages/**']\nvariant_catalog = 'distribution/assurance.json'\n",
        )?;
        std::fs::write(
            ws.path.join("distribution/assurance.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "variant_catalog_version": 2,
                "work": "assurance",
                "variants": [
                    {"id": "rsa", "dimensions": {}, "cargo_roots": [{"manifest": "fuzz-packages/fuzz-rsa/Cargo.toml", "package": "fuzz-rsa"}]},
                    {"id": "sha2", "dimensions": {}, "cargo_roots": [{"manifest": "fuzz-packages/fuzz-sha2/Cargo.toml", "package": "fuzz-sha2"}]}
                ]
            }))?,
        )?;
        ws.commit("establish auxiliary Cargo roots")?;

        ws.modify_file("demo", "src/rsa.rs", "pub fn value() -> usize { 3 }\n")?;
        let product = plan(&ws, &["--since", "HEAD"])?;
        let selected = &product["work"]["assurance"]["scope"]["selection"];
        assert_eq!(selected["kind"], "selected");
        assert_eq!(selected["variants"][0]["id"], "rsa");

        ws.commit("change RSA product source")?;
        std::fs::write(
            ws.path.join("fuzz-packages/fuzz-rsa/src/lib.rs"),
            "pub fn harness() { let _ = 1; }\n",
        )?;
        let harness = plan(&ws, &["--since", "HEAD"])?;
        let selected = &harness["work"]["assurance"]["scope"]["selection"];
        assert_eq!(selected["kind"], "selected");
        assert_eq!(selected["variants"][0]["id"], "rsa");

        ws.commit("change scoped auxiliary harness")?;
        let lock = ws.path.join("fuzz-packages/fuzz-rsa/Cargo.lock");
        let lock_contents = std::fs::read_to_string(&lock)?;
        std::fs::write(&lock, format!("{lock_contents}\n"))?;
        let root_lock = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(root_lock["work"]["assurance"]["scope"]["selection"]["kind"], "all");

        ws.commit("change auxiliary root lock")?;
        let auxiliary_manifest = ws.path.join("fuzz-packages/fuzz-rsa/Cargo.toml");
        let manifest = std::fs::read_to_string(&auxiliary_manifest)?;
        std::fs::write(&auxiliary_manifest, format!("{manifest}\n# manifest policy\n"))?;
        let manifest = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(manifest["work"]["assurance"]["scope"]["selection"]["kind"], "all");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_variant_evidence_names_only_seeds_reaching_each_row() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-variant-origin-provenance")?;
        ws.add_crate("affected-seed", "0.1.0", &[])?;
        ws.add_crate(
            "affected-root",
            "0.1.0",
            &[("affected-seed", r#"{ path = "../affected-seed" }"#)],
        )?;
        ws.add_crate("disjoint-seed", "0.1.0", &[])?;
        ws.add_crate("unrelated-root", "0.1.0", &[])?;
        std::fs::create_dir_all(ws.path.join("distribution"))?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[plan.work.deliverables]\nscope = 'variants'\nvariant_catalog = 'distribution/deliverables.json'\n",
        )?;
        std::fs::write(
            ws.path.join("distribution/deliverables.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "variant_catalog_version": 2,
                "work": "deliverables",
                "variants": [
                    {"id": "affected", "dimensions": {}, "cargo_roots": [{"package": "affected-root"}]},
                    {"id": "unrelated", "dimensions": {}, "cargo_roots": [{"package": "unrelated-root"}]}
                ]
            }))?,
        )?;
        ws.commit("establish variant provenance fixture")?;

        ws.modify_file("affected-seed", "src/lib.rs", "pub fn affected() {}\n")?;
        ws.modify_file("disjoint-seed", "src/lib.rs", "pub fn disjoint() {}\n")?;
        let planned = plan(&ws, &["--since", "HEAD"])?;
        let variants = planned["work"]["deliverables"]["scope"]["selection"]["variants"]
            .as_array()
            .context("deliverable variants missing")?
            .iter()
            .filter_map(|variant| variant["id"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(variants, ["affected"]);
        let evidence_id = planned["work"]["deliverables"]["evidence"][0]
            .as_str()
            .context("deliverable evidence ID missing")?;
        assert_eq!(
            planned["evidence"][evidence_id]["input"].as_str(),
            Some("cargo:affected-seed@0.1.0#path:crates/affected-seed->variant:affected")
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_variant_evidence_names_removed_historical_seed() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-variant-historical-origin")?;
        let removed = ws.add_crate("removed-seed", "0.1.0", &[])?;
        let affected = ws.add_crate(
            "affected-root",
            "0.1.0",
            &[("removed-seed", r#"{ path = "../removed-seed" }"#)],
        )?;
        ws.add_crate("unrelated-root", "0.1.0", &[])?;
        std::fs::create_dir_all(ws.path.join("distribution"))?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[plan.work.deliverables]\nscope = 'variants'\nvariant_catalog = 'distribution/deliverables.json'\n",
        )?;
        std::fs::write(
            ws.path.join("distribution/deliverables.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "variant_catalog_version": 2,
                "work": "deliverables",
                "variants": [
                    {"id": "affected", "dimensions": {}, "cargo_roots": [{"package": "affected-root"}]},
                    {"id": "unrelated", "dimensions": {}, "cargo_roots": [{"package": "unrelated-root"}]}
                ]
            }))?,
        )?;
        ws.commit("establish historical variant provenance fixture")?;

        std::fs::remove_dir_all(removed)?;
        let manifest = std::fs::read_to_string(affected.join("Cargo.toml"))?;
        std::fs::write(
            affected.join("Cargo.toml"),
            manifest.replace("removed-seed = { path = \"../removed-seed\" }\n", ""),
        )?;
        let cold = plan(&ws, &["--since", "HEAD"])?;
        let base_model = serde_json::json!({
            "packages": [
                {
                    "key": "affected-root@0.1.0#path:crates/affected-root",
                    "name": "affected-root",
                    "root": "crates/affected-root",
                    "targets": [{"name": "affected_root", "kind": ["lib"], "src_path": "crates/affected-root/src/lib.rs"}]
                },
                {
                    "key": "removed-seed@0.1.0#path:crates/removed-seed",
                    "name": "removed-seed",
                    "root": "crates/removed-seed",
                    "targets": [{"name": "removed_seed", "kind": ["lib"], "src_path": "crates/removed-seed/src/lib.rs"}]
                }
            ],
            "edges": [{
                "dependency": "removed-seed@0.1.0#path:crates/removed-seed",
                "dependent": "affected-root@0.1.0#path:crates/affected-root",
                "domain": "build"
            }]
        });
        let evidence = complete_evidence_with_base_model(&cold, HashMap::new(), base_model)?;
        let evidence_path = write_evidence(&ws, "historical-origin-evidence.json", &evidence)?;
        let bounded = plan(&ws, &["--since", "HEAD", "--evidence", &evidence_path])?;
        let variants = bounded["work"]["deliverables"]["scope"]["selection"]["variants"]
            .as_array()
            .context("deliverable variants missing")?
            .iter()
            .filter_map(|variant| variant["id"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(variants, ["affected"]);
        let evidence_id = bounded["work"]["deliverables"]["evidence"][0]
            .as_str()
            .context("deliverable evidence ID missing")?;
        assert_eq!(
            bounded["evidence"][evidence_id]["input"].as_str(),
            Some(
                "cargo:affected-root@0.1.0#path:crates/affected-root->variant:affected,cargo:removed-seed@0.1.0#path:crates/removed-seed->variant:affected"
            )
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_variant_catalog_v2_models_iggy_deliverables_exactly() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-iggy-deliverables")?;
        ws.add_crate("common", "0.1.0", &[])?;
        ws.add_crate("server", "0.1.0", &[("common", r#"{ path = "../common" }"#)])?;
        ws.add_crate("mcp", "0.1.0", &[("common", r#"{ path = "../common" }"#)])?;
        ws.add_crate("dashboard-frontend", "0.1.0", &[])?;
        ws.add_crate(
            "dashboard-server",
            "0.1.0",
            &[("dashboard-frontend", r#"{ path = "../dashboard-frontend" }"#)],
        )?;
        ws.add_crate("connectors", "0.1.0", &[])?;
        ws.add_crate("connector-plugin", "0.1.0", &[])?;
        std::fs::create_dir_all(ws.path.join("distribution"))?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[plan.work.deliverables]\nscope = 'variants'\nvariant_catalog = 'distribution/deliverables.json'\n",
        )?;

        let common_paths = [
            ".dockerignore",
            "LICENSE",
            "NOTICE",
            "scripts/verification/third-party-licenses.sh",
        ];
        let row = |id: &str, roots: &[&str], extra: &[&str], rust: bool| {
            let mut paths = common_paths.to_vec();
            if rust {
                paths.extend(["about.toml", "about.hbs"]);
            }
            paths.extend_from_slice(extra);
            serde_json::json!({
                "id": id,
                "dimensions": {"image": id},
                "cargo_roots": roots.iter().map(|package| serde_json::json!({"package": package})).collect::<Vec<_>>(),
                "external_paths": paths
            })
        };
        let catalog = serde_json::json!({
            "variant_catalog_version": 2,
            "work": "deliverables",
            "variants": [
                row("server", &["server"], &["core/server/Dockerfile", "web/**", "scripts/verification/render-node-licenses.mjs"], true),
                row("mcp", &["mcp"], &["core/mcp/Dockerfile"], true),
                row("dashboard", &["dashboard-server", "dashboard-frontend"], &["core/dashboard/Dockerfile"], true),
                row("connectors", &["connectors"], &["core/connectors/Dockerfile"], true),
                row("web", &[], &["web/**", "scripts/verification/render-node-licenses.mjs"], false)
            ]
        });
        let schema: Value = serde_json::from_str(PLAN_VARIANTS_V2_SCHEMA)?;
        let validator = jsonschema::validator_for(&schema).map_err(|error| anyhow!("invalid schema: {error}"))?;
        let errors = validator
            .iter_errors(&catalog)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        assert!(errors.is_empty(), "v2 catalog failed schema: {errors:#?}");
        std::fs::write(
            ws.path.join("distribution/deliverables.json"),
            serde_json::to_vec_pretty(&catalog)?,
        )?;

        for relative in common_paths.into_iter().chain([
            "about.toml",
            "about.hbs",
            "core/server/Dockerfile",
            "core/mcp/Dockerfile",
            "core/dashboard/Dockerfile",
            "core/connectors/Dockerfile",
            "web/Dockerfile",
            "web/app.ts",
            "scripts/verification/render-node-licenses.mjs",
            "foreign/node/package-lock.json",
            "docs/guide.md",
        ]) {
            let path = ws.path.join(relative);
            std::fs::create_dir_all(path.parent().context("fixture path has no parent")?)?;
            std::fs::write(path, "before\n")?;
        }
        ws.commit("establish Iggy deliverable model")?;

        let cases = [
            ("crates/server/src/lib.rs", vec!["server"]),
            ("crates/common/src/lib.rs", vec!["mcp", "server"]),
            ("web/app.ts", vec!["server", "web"]),
            ("crates/dashboard-frontend/src/lib.rs", vec!["dashboard"]),
            ("crates/connector-plugin/src/lib.rs", vec![]),
            ("foreign/node/package-lock.json", vec![]),
            ("docs/guide.md", vec![]),
            ("core/mcp/Dockerfile", vec!["mcp"]),
            (".dockerignore", vec!["connectors", "dashboard", "mcp", "server", "web"]),
            ("LICENSE", vec!["connectors", "dashboard", "mcp", "server", "web"]),
            (
                "scripts/verification/third-party-licenses.sh",
                vec!["connectors", "dashboard", "mcp", "server", "web"],
            ),
            ("about.hbs", vec!["connectors", "dashboard", "mcp", "server"]),
            ("scripts/verification/render-node-licenses.mjs", vec!["server", "web"]),
            ("Cargo.toml", vec!["connectors", "dashboard", "mcp", "server"]),
        ];
        for (relative, expected) in cases {
            let path = ws.path.join(relative);
            let original = std::fs::read(&path)?;
            let mut changed = original.clone();
            if relative == "Cargo.toml" {
                changed.extend_from_slice(b"\n[plan-test]\nchanged = true\n");
            } else {
                changed.extend_from_slice(b"\n# changed\n");
            }
            std::fs::write(&path, changed)?;
            let planned = plan(&ws, &["--since", "HEAD"])?;
            if expected.is_empty() {
                assert_eq!(planned["work"]["deliverables"]["state"], "skipped", "{relative}");
            } else {
                let actual = planned["work"]["deliverables"]["scope"]["selection"]["variants"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|variant| variant["id"].as_str())
                    .collect::<Vec<_>>();
                assert_eq!(actual, expected, "{relative}: {planned}");
            }
            std::fs::write(path, original)?;
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_emits_runtime_artifacts_as_distinct_cargo_work() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-runtime-artifacts")?;
        for package in [
            "integration",
            "server",
            "cli",
            "mcp",
            "connectors",
            "connector-plugin",
            "unit",
        ] {
            let root = ws.add_crate(package, "0.1.0", &[])?;
            if !matches!(package, "integration" | "unit") {
                std::fs::write(root.join("src/main.rs"), "fn main() {}\n")?;
            }
            if package == "server" {
                std::fs::write(root.join("src/internal.rs"), "pub fn unchanged() {}\n")?;
            }
        }
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            r#"[plan.work.runtime-artifacts]
scope = "cargo"
cargo_prerequisites = [
  { source_work = "cargo.test", when = [{ package = "integration" }], require = [
    { package = "server", target = { name = "server", kind = "bin" } },
    { package = "cli", target = { name = "cli", kind = "bin" } },
    { package = "mcp", target = { name = "mcp", kind = "bin" } },
    { package = "connectors", target = { name = "connectors", kind = "bin" } },
    { package = "connector-plugin", target = { name = "connector-plugin", kind = "bin" } },
  ] },
]
"#,
        )?;
        ws.commit("establish runtime artifact edges")?;

        let package_names = |planned: &Value, work: &str| {
            planned["work"][work]["scope"]["selection"]["packages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|package| package["name"].as_str().map(str::to_string))
                .collect::<BTreeSet<_>>()
        };

        let integration_path = ws.path.join("crates/integration/src/lib.rs");
        let integration_original = std::fs::read(&integration_path)?;
        std::fs::write(&integration_path, "pub fn changed() {}\n")?;
        let integration = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(
            package_names(&integration, "cargo.test"),
            BTreeSet::from(["integration".to_string()])
        );
        assert_eq!(
            package_names(&integration, "runtime-artifacts"),
            BTreeSet::from([
                "cli".to_string(),
                "connector-plugin".to_string(),
                "connectors".to_string(),
                "mcp".to_string(),
                "server".to_string(),
            ])
        );
        assert_eq!(
            integration["work"]["runtime-artifacts"]["scope"]["selection"]["targets"]
                .as_array()
                .map(Vec::len),
            Some(5)
        );
        std::fs::write(integration_path, integration_original)?;

        let server_path = ws.path.join("crates/server/src/internal.rs");
        let server_original = std::fs::read(&server_path)?;
        std::fs::write(&server_path, "pub fn changed() {}\n")?;
        let cold_artifact = plan(&ws, &["--since", "HEAD"])?;
        let evidence = complete_evidence(&cold_artifact, HashMap::new())?;
        let evidence_path = write_evidence(&ws, "runtime-artifact-evidence.json", &evidence)?;
        let artifact = plan(&ws, &["--since", "HEAD", "--evidence", &evidence_path])?;
        assert_eq!(
            package_names(&artifact, "cargo.test"),
            BTreeSet::from(["integration".to_string(), "server".to_string()]),
            "runtime artifact changes must propagate back to their declared test execution root"
        );
        assert!(!package_names(&artifact, "cargo.test").contains("cli"));
        std::fs::write(server_path, server_original)?;

        ws.modify_file("unit", "src/lib.rs", "pub fn changed() {}\n")?;
        let unit = plan(&ws, &["--since", "HEAD"])?;
        assert_eq!(package_names(&unit, "cargo.test"), BTreeSet::from(["unit".to_string()]));
        assert_eq!(unit["work"]["runtime-artifacts"]["state"], "skipped");
        Ok(())
    })();
    super::helpers::finish_test(result);
}
