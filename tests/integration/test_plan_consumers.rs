//! Named-work local and CI consumer contract tests.

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::process::Command;

const COMMIT_WORKFLOW: &str = include_str!("../../.github/workflows/commit.yaml");
const COMPATIBILITY_WORKFLOW: &str = include_str!("../../.github/workflows/compatibility.yaml");
const ARCHIVE_WORKFLOW: &str = include_str!("../../.github/workflows/release-archives.yaml");
const RELEASE_WORKFLOW: &str = include_str!("../../.github/workflows/release.yaml");
const CACHE_ACTION: &str = include_str!("../../.github/actions/cache/action.yaml");
const SETUP_ACTION: &str = include_str!("../../.github/actions/setup/action.yaml");
const CARGO_SCRIPT: &str = include_str!("../../scripts/cargo/run.sh");
const VERIFY_SCRIPT: &str = include_str!("../../scripts/plan/verify.sh");

fn reader() -> Command {
    let mut command = Command::new(if cfg!(windows) { "python" } else { "python3" });
    command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("scripts/plan/read.py");
    command
}

fn reader_at(workspace: &std::path::Path) -> Command {
    let mut command = Command::new(if cfg!(windows) { "python" } else { "python3" });
    command
        .current_dir(workspace)
        .env("CARGO_RAIL_BIN", env!("CARGO_BIN_EXE_cargo-rail"))
        .env("RUSTC_WRAPPER", "")
        .env("CARGO_BUILD_RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "")
        .env("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER", "")
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/plan/read.py"));
    command
}

fn fixture() -> Value {
    sign_plan(serde_json::json!({
        "plan_contract_version": 8,
        "identity": format!("plan-v8:sha256:{}", "0".repeat(64)),
        "inputs": {
            "base": "base",
            "head": "WORKTREE",
            "head_commit": "0".repeat(40),
            "capture": null,
            "cargo": format!("resolution-universe-v1:sha256:{}", "1".repeat(64)),
            "configuration": format!("cargo-configuration-v1:sha256:{}", "1".repeat(64)),
            "toolchain": "toolchain-v1",
            "target": format!("planning-target-v1:sha256:{}", "1".repeat(64)),
            "platform": "test-platform",
            "catalog": format!("work-catalog-v1:sha256:{}", "1".repeat(64)),
            "evidence": [],
            "override": "none"
        },
        "changes": {"files": [], "cargo": [], "config": []},
        "work": {
            "cargo.build": {"state": "skipped", "evidence": [format!("evidence:sha256:{}", "2".repeat(64))]},
            "cargo.test": {
                "state": "required",
                "cause": "changed_input",
                "scope": {
                    "kind": "cargo",
                    "selection": {
                        "kind": "packages",
                        "packages": [{"key": "demo@0.1.0#path:demo", "name": "demo", "cargo_spec": "demo;echo-not-a-shell"}],
                        "cargo_args": ["-p", "demo;echo-not-a-shell"],
                        "targets": [{"package": "demo@0.1.0#path:demo", "name": "contract", "kind": ["test"]}]
                    }
                },
                "evidence": [format!("evidence:sha256:{}", "3".repeat(64))]
            },
            "compatibility": {
                "state": "required",
                "cause": "changed_input",
                "scope": {
                    "kind": "variants",
                    "selection": {
                        "kind": "selected",
                        "variants": [{"id": "linux", "dimensions": {"family": "compatibility", "runner": "ubuntu-latest"}}],
                        "evidence": format!("evidence:sha256:{}", "4".repeat(64))
                    }
                },
                "evidence": [format!("evidence:sha256:{}", "4".repeat(64))]
            }
        },
        "required": ["cargo.test", "compatibility"],
        "evidence": {
            (format!("evidence:sha256:{}", "2".repeat(64))): {"code": "skip", "subject": "cargo.build", "description": "skip", "input": null, "complete": true},
            (format!("evidence:sha256:{}", "3".repeat(64))): {"code": "change", "subject": "cargo.test", "description": "change", "input": null, "complete": true},
            (format!("evidence:sha256:{}", "4".repeat(64))): {"code": "change", "subject": "compatibility", "description": "change", "input": null, "complete": true}
        }
    }))
}

fn sign_plan(mut plan: Value) -> Value {
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
    let records = plan["evidence"].as_object().expect("fixture evidence").clone();
    let mut replacements = std::collections::BTreeMap::new();
    let mut signed_records = serde_json::Map::new();
    for (reference, record) in records {
        let portable = serde_json::json!({
            "code": record["code"],
            "subject": record["subject"],
            "input": record["input"],
            "complete": record["complete"],
        });
        let encoded = serde_json::to_vec(&canonicalize(portable)).expect("canonical evidence fixture");
        let signed = format!(
            "evidence:sha256:{}",
            Sha256::digest(encoded)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        replacements.insert(reference, signed.clone());
        signed_records.insert(signed, record);
    }
    plan["evidence"] = Value::Object(signed_records);
    for decision in plan["work"].as_object_mut().expect("fixture work").values_mut() {
        if let Some(references) = decision["evidence"].as_array_mut() {
            for reference in references {
                if let Some(replacement) = reference.as_str().and_then(|value| replacements.get(value)) {
                    *reference = Value::String(replacement.clone());
                }
            }
        }
        if let Some(reference) = decision["scope"]["selection"]["evidence"].as_str()
            && let Some(replacement) = replacements.get(reference)
        {
            decision["scope"]["selection"]["evidence"] = Value::String(replacement.clone());
        }
    }
    let mut evidence = plan["evidence"]
        .as_object()
        .expect("fixture evidence")
        .keys()
        .cloned()
        .map(Value::String)
        .collect::<Vec<_>>();
    evidence.sort_unstable_by(|left, right| left.as_str().cmp(&right.as_str()));
    let portable = serde_json::json!({
        "plan_contract_version": plan["plan_contract_version"],
        "inputs": plan["inputs"],
        "changes": plan["changes"],
        "work": plan["work"],
        "required": plan["required"],
        "evidence": evidence,
    });
    let encoded = serde_json::to_vec(&canonicalize(portable)).expect("canonical fixture");
    plan["identity"] = Value::String(format!(
        "plan-v8:sha256:{}",
        Sha256::digest(encoded)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ));
    plan
}

#[test]
fn test_plan_reader_preserves_argv_and_lowers_targets_and_variants() {
    let result: Result<()> = (|| {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("plan.json");
        std::fs::write(&path, serde_json::to_vec(&fixture())?)?;

        let output = reader()
            .args(["cargo-args", path.to_str().unwrap(), "cargo.test"])
            .output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(output.stdout, b"-p\0demo;echo-not-a-shell\0");

        let output = reader()
            .args(["cargo-scope", path.to_str().unwrap(), "cargo.test"])
            .output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(output.stdout, b"packages\n");

        let output = reader()
            .args(["package-names", path.to_str().unwrap(), "cargo.test"])
            .output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(output.stdout, b"demo\0");

        let output = reader()
            .args(["cargo-scope", path.to_str().unwrap(), "cargo.build"])
            .output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(output.stdout, b"skipped\n");
        let output = reader()
            .args(["package-names", path.to_str().unwrap(), "cargo.build"])
            .output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.stdout.is_empty());

        let output = reader()
            .args(["target-args", path.to_str().unwrap(), "cargo.test"])
            .output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(output.stdout, b"--test\0contract\0");

        let output = reader()
            .args([
                "matrix",
                path.to_str().unwrap(),
                "compatibility",
                "--family",
                "compatibility",
            ])
            .output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout)?,
            serde_json::json!({"include": [{"compatibility": {"id": "linux", "runner": "ubuntu-latest"}}]})
        );

        let output = reader()
            .args([
                "matrix",
                path.to_str().unwrap(),
                "compatibility",
                "--family",
                "filesystem",
            ])
            .output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout)?,
            serde_json::json!({"include": []})
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_reader_distinguishes_workspace_and_rejects_ambiguous_package_names() {
    let result: Result<()> = (|| {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("plan.json");
        let mut workspace = fixture();
        workspace["work"]["cargo.test"]["scope"]["selection"] = serde_json::json!({
            "kind": "workspace",
            "cargo_args": [],
            "targets": []
        });
        std::fs::write(&path, serde_json::to_vec(&sign_plan(workspace))?)?;
        let output = reader()
            .args(["cargo-scope", path.to_str().unwrap(), "cargo.test"])
            .output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(output.stdout, b"workspace\n");
        let output = reader()
            .args(["package-names", path.to_str().unwrap(), "cargo.test"])
            .output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.stdout.is_empty());

        let mut punctuation = fixture();
        punctuation["work"]["cargo.test"]["scope"]["selection"]["packages"][0]["name"] = "demo-name_123".into();
        std::fs::write(&path, serde_json::to_vec(&sign_plan(punctuation))?)?;
        let output = reader()
            .args(["package-names", path.to_str().unwrap(), "cargo.test"])
            .output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(output.stdout, b"demo-name_123\0");

        let mut ambiguous = fixture();
        ambiguous["work"]["cargo.test"]["scope"]["selection"]["packages"] = serde_json::json!([
            {"key": "demo@0.1.0#path:a", "name": "demo", "cargo_spec": "demo@0.1.0"},
            {"key": "demo@0.2.0#path:b", "name": "demo", "cargo_spec": "demo@0.2.0"}
        ]);
        ambiguous["work"]["cargo.test"]["scope"]["selection"]["cargo_args"] =
            serde_json::json!(["-p", "demo@0.1.0", "-p", "demo@0.2.0"]);
        ambiguous["work"]["cargo.test"]["scope"]["selection"]["targets"] = serde_json::json!([]);
        std::fs::write(&path, serde_json::to_vec(&sign_plan(ambiguous))?)?;
        let output = reader()
            .args(["package-names", path.to_str().unwrap(), "cargo.test"])
            .output()?;
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("package names are ambiguous"));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_reader_rejects_contract_or_projection_drift() {
    let result: Result<()> = (|| {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("plan.json");
        let mut plan = fixture();
        plan["plan_contract_version"] = 7.into();
        let plan = sign_plan(plan);
        std::fs::write(&path, serde_json::to_vec(&plan)?)?;
        assert_eq!(
            reader().args(["validate", path.to_str().unwrap()]).status()?.code(),
            Some(2)
        );

        let mut plan = fixture();
        plan["required"] = serde_json::json!([]);
        std::fs::write(&path, serde_json::to_vec(&plan)?)?;
        assert_eq!(
            reader().args(["validate", path.to_str().unwrap()]).status()?.code(),
            Some(2)
        );

        let mut plan = fixture();
        plan["inputs"]["unknown"] = true.into();
        std::fs::write(&path, serde_json::to_vec(&plan)?)?;
        assert_eq!(
            reader().args(["validate", path.to_str().unwrap()]).status()?.code(),
            Some(2)
        );

        let mut plan = fixture();
        plan["work"]["cargo.test"]["scope"]["selection"]["unexpected"] = true.into();
        std::fs::write(&path, serde_json::to_vec(&plan)?)?;
        assert_eq!(
            reader().args(["validate", path.to_str().unwrap()]).status()?.code(),
            Some(2)
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_reader_creates_one_valid_plan_on_stdout() {
    let result: Result<()> = (|| {
        let output = reader()
            .env("CARGO_RAIL_BIN", env!("CARGO_BIN_EXE_cargo-rail"))
            .args(["create", "-", "--all"])
            .output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let plan: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(plan["plan_contract_version"], 8);
        assert_eq!(plan["inputs"]["override"], "all");
        assert!(plan["required"].as_array().is_some_and(|required| !required.is_empty()));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_reader_delegates_final_authority_verification_to_cargo_rail() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-reader-authority")?;
        let package = ws.add_crate("reader", "0.1.0", &[])?;
        ws.commit("establish plan reader authority")?;
        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--json"])?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let path = ws.path.join("target/saved-plan.json");
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, output.stdout)?;

        let unchanged = reader_at(&ws.path).args(["verify-checkout"]).arg(&path).output()?;
        assert!(
            unchanged.status.success(),
            "{}",
            String::from_utf8_lossy(&unchanged.stderr)
        );
        assert!(unchanged.stdout.is_empty());

        std::fs::write(package.join("src/lib.rs"), "pub fn drift() {}\n")?;
        let rejected = reader_at(&ws.path).args(["verify-checkout"]).arg(&path).output()?;
        assert_eq!(rejected.status.code(), Some(2));
        assert!(rejected.stdout.is_empty());
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("cargo-rail rejected current checkout binding"));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_reader_accepts_matching_checkout_from_another_planning_host() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-reader-cross-host")?;
        ws.add_crate("reader", "0.1.0", &[])?;
        ws.commit("establish cross-host plan authority")?;
        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--json"])?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let mut plan: Value = serde_json::from_slice(&output.stdout)?;
        plan["inputs"]["cargo"] = Value::String(format!("resolution-universe-v1:sha256:{}", "a".repeat(64)));
        plan["inputs"]["configuration"] = Value::String(format!("cargo-configuration-v1:sha256:{}", "b".repeat(64)));
        plan["inputs"]["toolchain"] = Value::String("foreign-toolchain".to_string());
        plan["inputs"]["target"] = Value::String(format!("planning-target-v1:sha256:{}", "c".repeat(64)));
        plan["inputs"]["platform"] = Value::String("windows-x86_64".to_string());
        let plan = sign_plan(plan);
        let path = ws.path.join("target/cross-host-plan.json");
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, serde_json::to_vec(&plan)?)?;

        let verified = reader_at(&ws.path).args(["verify-checkout"]).arg(&path).output()?;
        assert!(
            verified.status.success(),
            "{}",
            String::from_utf8_lossy(&verified.stderr)
        );
        assert!(verified.stdout.is_empty());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_source_reader_bootstrap_preserves_direct_binary_authority() {
    let result: Result<()> = (|| {
        let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("saved-plan.json");
        let bootstrap_target = directory.path().join("bootstrap-target");
        let planned = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
            .current_dir(repository)
            .args(["rail", "plan", "--since", "HEAD", "--json"])
            .output()?;
        assert!(planned.status.success(), "{}", String::from_utf8_lossy(&planned.stderr));
        std::fs::write(&path, planned.stdout)?;

        let verified = reader()
            .env_remove("CARGO_RAIL_BIN")
            .env("RAIL_BOOTSTRAP_TARGET_DIR", bootstrap_target)
            .args(["verify-checkout"])
            .arg(&path)
            .output()?;
        assert!(
            verified.status.success(),
            "{}",
            String::from_utf8_lossy(&verified.stderr)
        );
        assert!(verified.stdout.is_empty());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_repository_reader_prefers_the_installed_release_without_an_explicit_bootstrap() {
    use std::os::unix::fs::PermissionsExt as _;

    let result: Result<()> = (|| {
        let directory = tempfile::tempdir()?;
        let bin = directory.path().join("bin");
        std::fs::create_dir(&bin)?;
        let launcher = bin.join("cargo-rail");
        let marker = directory.path().join("selected-installed-release");
        let plan = directory.path().join("plan.json");
        std::fs::write(
            &launcher,
            "#!/bin/sh\n: > \"$CARGO_RAIL_READER_MARKER\"\nexec \"$CARGO_RAIL_READER_DELEGATE\" \"$@\"\n",
        )?;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700))?;
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
        )?;

        let output = reader()
            .env_remove("CARGO_RAIL_BIN")
            .env_remove("RAIL_BOOTSTRAP_TARGET_DIR")
            .env("PATH", path)
            .env("CARGO_RAIL_READER_MARKER", &marker)
            .env("CARGO_RAIL_READER_DELEGATE", env!("CARGO_BIN_EXE_cargo-rail"))
            .args(["create", plan.to_str().unwrap(), "--all"])
            .output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(marker.is_file(), "reader did not select cargo-rail from PATH");
        assert_eq!(load_plan_for_test(&plan)?["plan_contract_version"], 8);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
fn load_plan_for_test(path: &std::path::Path) -> Result<Value> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

#[test]
fn test_plan_reader_emits_plain_all_variant_sentinel() {
    let result: Result<()> = (|| {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("plan.json");
        let mut plan = fixture();
        let evidence = plan["work"]["compatibility"]["evidence"][0].clone();
        plan["work"]["compatibility"]["scope"]["selection"] = serde_json::json!({
            "kind": "all",
            "evidence": evidence
        });
        let plan = sign_plan(plan);
        std::fs::write(&path, serde_json::to_vec(&plan)?)?;

        let output = reader()
            .args(["matrix", path.to_str().unwrap(), "compatibility"])
            .output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(output.stdout, b"all\n");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_commit_workflow_uses_named_work_and_planner_matrices_only() {
    assert!(!COMMIT_WORKFLOW.contains("cargo-rail-action"));
    assert!(!COMMIT_WORKFLOW.contains("outputs.build"));
    for work in [
        "action-pins",
        "compatibility",
        "release-archives",
        "surface",
        "docs.generated",
    ] {
        assert!(COMMIT_WORKFLOW.contains(work), "missing named work gate {work}");
    }
    assert!(COMMIT_WORKFLOW.contains("cargo-rail-plan-v8"));
    assert!(COMPATIBILITY_WORKFLOW.contains("inputs.compatibility-matrix"));
    assert!(COMPATIBILITY_WORKFLOW.contains("inputs.filesystem-matrix"));
    assert!(ARCHIVE_WORKFLOW.contains("inputs.selected-matrix"));
    assert!(ARCHIVE_WORKFLOW.contains("scripts/ci/smoke-release-tar.sh"));
    assert!(COMPATIBILITY_WORKFLOW.contains("scripts/plan/verify.sh"));
    assert!(ARCHIVE_WORKFLOW.contains("scripts/plan/verify.sh"));
}

#[test]
fn test_commit_workflow_consumes_every_builtin_cargo_execution_decision() {
    for work in ["cargo.build", "cargo.test", "cargo.doctest"] {
        assert!(
            COMMIT_WORKFLOW.contains(&format!("required-work), '{work}')")),
            "Commit workflow does not route required {work}"
        );
    }
    assert!(COMMIT_WORKFLOW.contains("just build"));
    assert!(COMMIT_WORKFLOW.contains("just test"));
    assert!(CARGO_SCRIPT.contains("run_cargo_work cargo.build"));
    for work in ["cargo.test", "cargo.doctest"] {
        assert!(CARGO_SCRIPT.contains(&format!("run_cargo_work {work}")));
    }
    assert!(CARGO_SCRIPT.contains("scripts/plan/verify.sh \"$plan_file\""));
    assert!(VERIFY_SCRIPT.contains("GITHUB_ACTIONS"));
    assert!(VERIFY_SCRIPT.contains("target/debug/cargo-rail"));
}

#[test]
fn test_ci_dogfoods_released_cache_and_reuses_exact_source_authority() {
    assert!(CACHE_ACTION.contains("loadingalias/cargo-rail-action/cache@"));
    assert!(CACHE_ACTION.contains("# v8.1.0"));
    assert!(CACHE_ACTION.contains("version: 0.24.0"));
    assert!(CACHE_ACTION.contains("root-portability: remap"));
    assert!(!CACHE_ACTION.contains("scripts/cache/setup.sh --max-size 10GiB"));
    assert!(!CACHE_ACTION.contains("strict-probe: \"true\""));
    assert!(COMMIT_WORKFLOW.contains("target/debug/cargo-rail"));
    assert!(COMMIT_WORKFLOW.contains("scripts/plan/verify.sh target/plan-v8.json"));
    assert!(COMMIT_WORKFLOW.contains("stage: ${{ github.event_name == 'push'"));
}

#[test]
fn test_ci_uses_one_r2_authority_with_bounded_credentials() {
    for source in [COMMIT_WORKFLOW, COMPATIBILITY_WORKFLOW, ARCHIVE_WORKFLOW] {
        assert!(!source.contains("CACHE_QUALIFICATION_AWS_"));
        assert!(!source.contains("configure-aws-credentials"));
        assert!(!source.contains("native-cache-role:"));
        assert!(!source.contains("native-cache-region:"));
        assert!(!source.contains("native-cache-account:"));
    }

    assert!(COMMIT_WORKFLOW.contains("vars.CARGO_RAIL_CACHE_REMOTE"));
    assert!(COMMIT_WORKFLOW.contains("secrets.CARGO_RAIL_R2_ACCESS_KEY_ID"));
    assert!(COMMIT_WORKFLOW.contains("secrets.CARGO_RAIL_R2_SECRET_ACCESS_KEY"));
    assert!(COMMIT_WORKFLOW.contains("github.event_name == 'push'"));
    assert!(RELEASE_WORKFLOW.contains("native-cache-url: ${{ vars.CARGO_RAIL_CACHE_REMOTE }}"));
    assert!(RELEASE_WORKFLOW.contains("r2_access_key_id: ${{ secrets.CARGO_RAIL_R2_ACCESS_KEY_ID }}"));
    assert!(RELEASE_WORKFLOW.contains("r2_secret_access_key: ${{ secrets.CARGO_RAIL_R2_SECRET_ACCESS_KEY }}"));

    for source in [COMPATIBILITY_WORKFLOW, ARCHIVE_WORKFLOW] {
        assert!(source.contains("secrets:\n      r2_access_key_id:"));
        assert!(source.contains("r2_secret_access_key:"));
        assert!(source.contains("remote-credentials-ready:"));
    }
    assert!(CACHE_ACTION.contains("r2://*)"));
    assert!(CACHE_ACTION.contains("CARGO_RAIL_CACHE_CREDENTIALS_READY"));
    assert!(!CACHE_ACTION.contains("AWS_ACCESS_KEY_ID"));
    assert!(!CACHE_ACTION.contains("AWS_SECRET_ACCESS_KEY"));
    assert!(!SETUP_ACTION.contains("AWS_ACCESS_KEY_ID"));
    assert!(!SETUP_ACTION.contains("AWS_SECRET_ACCESS_KEY"));
}

#[test]
fn test_release_reuses_exact_sha_commit_archives_with_recovery_fallback() {
    for fragment in [
        "gh run list",
        "actions/runs/$run_id/artifacts",
        "run-id: ${{ needs.verify-release.outputs.commit_run_id }}",
        "rebuilding every archive for recovery",
    ] {
        assert!(
            RELEASE_WORKFLOW.contains(fragment),
            "missing release handoff: {fragment}"
        );
    }
    assert!(!RELEASE_WORKFLOW.contains("cargo nextest run"));
    assert!(!RELEASE_WORKFLOW.contains("Verify Clippy"));
}
