//! Named-work local and CI consumer contract tests.

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::{Context as _, Result};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::process::Command;

const COMMIT_WORKFLOW: &str = include_str!("../../.github/workflows/commit.yaml");
const COMPATIBILITY_WORKFLOW: &str = include_str!("../../.github/workflows/compatibility.yaml");
const ARCHIVE_WORKFLOW: &str = include_str!("../../.github/workflows/release-archives.yaml");
const RELEASE_WORKFLOW: &str = include_str!("../../.github/workflows/release.yaml");
const CACHE_ACTION: &str = include_str!("../../.github/actions/cache/action.yaml");
const SETUP_ACTION: &str = include_str!("../../.github/actions/setup/action.yaml");
const RAIL_CONFIG: &str = include_str!("../../.config/rail.toml");
const CARGO_SCRIPT: &str = include_str!("../../scripts/cargo/run.sh");
const VERIFY_SCRIPT: &str = include_str!("../../scripts/plan/verify.sh");
const PLAN_BUNDLE_V1_SCHEMA: &str = include_str!("../../schemas/plan-bundle-v1.schema.json");

fn reader() -> Command {
    reader_in(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
}

fn reader_in(workspace: &std::path::Path) -> Command {
    let mut command = Command::new(if cfg!(windows) { "python" } else { "python3" });
    command
        .current_dir(workspace)
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/plan/read.py"));
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
        let workspace = TestWorkspace::new_named("plan-reader-stdout")?;
        workspace.add_crate("reader", "0.1.0", &[])?;
        workspace.commit("establish plan reader stdout fixture")?;
        let output = reader_in(&workspace.path)
            .env("CARGO_RAIL_BIN", env!("CARGO_BIN_EXE_cargo-rail"))
            .env("RAIL_SINCE", "HEAD")
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
fn test_portable_plan_bundle_validates_integrity_contract_identity_and_checkout_before_execution() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("portable-plan-bundle")?;
        let package = ws.add_crate("reader", "0.1.0", &[])?;
        let base = ws.commit("establish portable plan base")?;
        std::fs::write(package.join("src/lib.rs"), "pub fn head() {}\n")?;
        let head = ws.commit("establish portable plan head")?;
        let target = ws.path.join("target/portable-plan");
        std::fs::create_dir_all(&target)?;
        let source_plan = target.join("source-plan.json");
        let planned = reader_at(&ws.path)
            .env("RAIL_SINCE", &base)
            .env("RAIL_OBJECT_HEAD", &head)
            .args(["create"])
            .arg(&source_plan)
            .output()?;
        assert!(planned.status.success(), "{}", String::from_utf8_lossy(&planned.stderr));
        let bundled = reader_in(&ws.path)
            .args(["bundle"])
            .arg(&source_plan)
            .arg(&target)
            .args(["--producer-version", "0.26.0"])
            .output()?;
        assert!(bundled.status.success(), "{}", String::from_utf8_lossy(&bundled.stderr));

        let manifest_path = target.join("plan-bundle-v1.json");
        let plan_path = target.join("plan-v8.json");
        let reader_path = target.join("plan-read.py");
        let manifest: Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
        let schema: Value = serde_json::from_str(PLAN_BUNDLE_V1_SCHEMA)?;
        let validator = jsonschema::validator_for(&schema)?;
        assert!(
            validator.iter_errors(&manifest).next().is_none(),
            "portable plan bundle failed its published schema"
        );
        assert_eq!(manifest["plan_bundle_version"], 1);
        assert_eq!(manifest["contract"]["version"], 8);
        assert_eq!(manifest["producer"]["version"], "0.26.0");
        assert_eq!(manifest["platform_limits"]["architectures"][0], "any");
        let payload_bytes = manifest["files"]
            .as_array()
            .context("bundle files missing")?
            .iter()
            .filter_map(|file| file["size"].as_u64())
            .sum::<u64>();
        assert!(payload_bytes < 1024 * 1024, "portable verifier bundle is not compact");

        let verify = || -> Result<std::process::Output> {
            Ok(Command::new(if cfg!(windows) { "python" } else { "python3" })
                .current_dir(&ws.path)
                .arg(&reader_path)
                .args(["verify-bundle"])
                .arg(&manifest_path)
                .output()?)
        };
        let accepted = verify()?;
        assert!(
            accepted.status.success(),
            "{}",
            String::from_utf8_lossy(&accepted.stderr)
        );
        assert!(accepted.stdout.is_empty());

        std::fs::write(ws.path.join("dirty.txt"), "dirty\n")?;
        let dirty = verify()?;
        assert_eq!(dirty.status.code(), Some(2));
        std::fs::remove_file(ws.path.join("dirty.txt"))?;

        let original_manifest = std::fs::read(&manifest_path)?;
        let original_plan = std::fs::read(&plan_path)?;
        let mut incompatible: Value = serde_json::from_slice(&original_manifest)?;
        incompatible["contract"]["version"] = 9.into();
        std::fs::write(&manifest_path, serde_json::to_vec_pretty(&incompatible)?)?;
        assert_eq!(verify()?.status.code(), Some(2));
        std::fs::write(&manifest_path, &original_manifest)?;

        let bind_plan_file = || -> Result<()> {
            let bytes = std::fs::read(&plan_path)?;
            let mut manifest: Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
            let file = manifest["files"]
                .as_array_mut()
                .context("bundle files missing")?
                .iter_mut()
                .find(|file| file["role"] == "execution_plan")
                .context("execution plan role missing")?;
            file["size"] = (bytes.len() as u64).into();
            file["sha256"] = Value::String(
                Sha256::digest(&bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            );
            std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
            Ok(())
        };
        std::fs::write(&plan_path, b"{")?;
        bind_plan_file()?;
        assert_eq!(verify()?.status.code(), Some(2));

        std::fs::write(&plan_path, &original_plan)?;
        std::fs::write(&manifest_path, &original_manifest)?;
        let mut wrong_identity: Value = serde_json::from_slice(&original_plan)?;
        let rejected_identity = format!("plan-v8:sha256:{}", "0".repeat(64));
        wrong_identity["identity"] = Value::String(rejected_identity.clone());
        std::fs::write(&plan_path, serde_json::to_vec(&wrong_identity)?)?;
        let mut manifest: Value = serde_json::from_slice(&original_manifest)?;
        manifest["plan_identity"] = Value::String(rejected_identity);
        std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
        bind_plan_file()?;
        assert_eq!(verify()?.status.code(), Some(2));

        std::fs::write(&plan_path, &original_plan)?;
        std::fs::write(&manifest_path, &original_manifest)?;
        std::fs::write(package.join("src/lib.rs"), "pub fn later_commit() {}\n")?;
        ws.commit("move beyond portable plan head")?;
        assert_eq!(verify()?.status.code(), Some(2));
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
        let workspace = TestWorkspace::new_named("source-reader-bootstrap")?;
        workspace.add_crate("reader", "0.1.0", &[])?;
        workspace.commit("establish source reader fixture")?;
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("saved-plan.json");
        let bootstrap_target = directory.path().join("bootstrap-target");
        let planned = run_cargo_rail(&workspace.path, &["rail", "plan", "--since", "HEAD", "--json"])?;
        assert!(planned.status.success(), "{}", String::from_utf8_lossy(&planned.stderr));
        std::fs::write(&path, planned.stdout)?;

        let verified = Command::new(if cfg!(windows) { "python" } else { "python3" })
            .current_dir(&workspace.path)
            .env_remove("CARGO_RAIL_BIN")
            .env("RAIL_BOOTSTRAP_TARGET_DIR", bootstrap_target)
            .arg(repository.join("scripts/plan/read.py"))
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
        let workspace = TestWorkspace::new_named("installed-reader-selection")?;
        workspace.add_crate("reader", "0.1.0", &[])?;
        workspace.commit("establish installed reader fixture")?;
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

        let output = reader_in(&workspace.path)
            .env_remove("CARGO_RAIL_BIN")
            .env_remove("RAIL_BOOTSTRAP_TARGET_DIR")
            .env("RAIL_SINCE", "HEAD")
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
    assert!(VERIFY_SCRIPT.contains("plan-bundle-v1.json"));
    assert!(VERIFY_SCRIPT.contains("verify-bundle"));
}

#[test]
fn test_ci_dogfoods_released_cache_and_reuses_exact_source_authority() {
    assert!(CACHE_ACTION.contains("loadingalias/cargo-rail-action/cache@"));
    assert!(CACHE_ACTION.contains("# v8.2.0"));
    assert!(CACHE_ACTION.contains("version: 0.25.0"));
    assert!(CACHE_ACTION.contains("root-portability: remap"));
    assert!(!CACHE_ACTION.contains("scripts/cache/setup.sh --max-size 10GiB"));
    assert!(CACHE_ACTION.contains("strict-probe: \"true\""));
    assert!(COMMIT_WORKFLOW.contains("target/debug/cargo-rail"));
    assert!(COMMIT_WORKFLOW.contains("RAIL_OBJECT_HEAD: ${{ github.sha }}"));
    assert!(COMMIT_WORKFLOW.contains("target/plan-read.py"));
    assert!(COMMIT_WORKFLOW.contains("target/plan-bundle-v1.json"));
    assert!(COMMIT_WORKFLOW.contains("scripts/plan/verify.sh target/plan-v8.json"));
    assert!(COMMIT_WORKFLOW.contains("stage: ${{ github.event_name == 'push'"));
    assert!(RAIL_CONFIG.contains(
        "[plan.work.\"docs.generated\"]\nscope = \"repository\"\npaths = [\n  \"scripts/docs/**\",\n  \"scripts/ci/install-tools.sh\""
    ));
}

#[test]
fn test_ci_scopes_remote_cache_credentials_to_setup_and_compiler_steps() {
    let commit_setup = r#"          native-cache-access-key-id: ${{ env.CARGO_RAIL_CACHE_URL != '' && secrets.CARGO_RAIL_R2_ACCESS_KEY_ID || '' }}
          native-cache-secret-access-key: ${{ env.CARGO_RAIL_CACHE_URL != '' && secrets.CARGO_RAIL_R2_SECRET_ACCESS_KEY || '' }}"#;
    assert_eq!(COMMIT_WORKFLOW.matches(commit_setup).count(), 4);

    let reusable_setup = r#"          remote-access-key-id: ${{ inputs.native-cache-url != '' && secrets.r2_access_key_id || '' }}
          remote-secret-access-key: ${{ inputs.native-cache-url != '' && secrets.r2_secret_access_key || '' }}"#;
    assert_eq!(COMPATIBILITY_WORKFLOW.matches(reusable_setup).count(), 1);
    assert_eq!(ARCHIVE_WORKFLOW.matches(reusable_setup).count(), 1);
    let riscv_setup = r#"          remote-access-key-id: ${{ matrix.compatibility.target != 'riscv64gc-unknown-linux-gnu' && inputs.native-cache-url != '' && secrets.r2_access_key_id || '' }}
          remote-secret-access-key: ${{ matrix.compatibility.target != 'riscv64gc-unknown-linux-gnu' && inputs.native-cache-url != '' && secrets.r2_secret_access_key || '' }}"#;
    assert_eq!(COMPATIBILITY_WORKFLOW.matches(riscv_setup).count(), 1);

    let credential_environment = r#"      env:
        AWS_ACCESS_KEY_ID: ${{ inputs.remote-access-key-id }}
        AWS_SECRET_ACCESS_KEY: ${{ inputs.remote-secret-access-key }}
        AWS_EC2_METADATA_DISABLED: "true"
      uses: loadingalias/cargo-rail-action/cache@"#;
    assert!(CACHE_ACTION.contains(credential_environment));
    assert_eq!(CACHE_ACTION.matches("AWS_ACCESS_KEY_ID:").count(), 1);
    assert_eq!(CACHE_ACTION.matches("AWS_SECRET_ACCESS_KEY:").count(), 1);
    assert!(!SETUP_ACTION.contains("AWS_ACCESS_KEY_ID"));
    assert!(!SETUP_ACTION.contains("AWS_SECRET_ACCESS_KEY"));

    let commit_compiler =
        "AWS_ACCESS_KEY_ID: ${{ env.CARGO_RAIL_CACHE_URL != '' && secrets.CARGO_RAIL_R2_ACCESS_KEY_ID || '' }}";
    assert_eq!(COMMIT_WORKFLOW.matches(commit_compiler).count(), 5);
    let reusable_compiler = "AWS_ACCESS_KEY_ID: ${{ inputs.native-cache-url != '' && secrets.r2_access_key_id || '' }}";
    assert_eq!(COMPATIBILITY_WORKFLOW.matches(reusable_compiler).count(), 6);
    assert_eq!(ARCHIVE_WORKFLOW.matches(reusable_compiler).count(), 0);
    let riscv_bootstrap = "AWS_ACCESS_KEY_ID: ${{ matrix.compatibility.target != 'riscv64gc-unknown-linux-gnu' && inputs.native-cache-url != '' && secrets.r2_access_key_id || '' }}";
    assert_eq!(COMPATIBILITY_WORKFLOW.matches(riscv_bootstrap).count(), 1);
    assert_eq!(
        COMPATIBILITY_WORKFLOW
            .matches("AWS_ACCESS_KEY_ID: ${{ secrets.r2_access_key_id }}")
            .count(),
        1
    );
    assert!(COMPATIBILITY_WORKFLOW.contains("- name: Enable source-built RISC-V compiler cache"));
    assert!(
        COMPATIBILITY_WORKFLOW.contains(
            "if: matrix.compatibility.target == 'riscv64gc-unknown-linux-gnu' && inputs.native-cache-url != ''"
        )
    );
    assert!(
        COMPATIBILITY_WORKFLOW.contains("PATH=\"$PWD/target/debug:$PATH\" scripts/cache/setup.sh --max-size 10GiB")
    );
    let archive_compiler = "AWS_ACCESS_KEY_ID: ${{ !endsWith(matrix.target, '-unknown-linux-gnu') && inputs.native-cache-url != '' && secrets.r2_access_key_id || '' }}";
    assert_eq!(ARCHIVE_WORKFLOW.matches(archive_compiler).count(), 2);
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
        assert!(source.contains("remote-access-key-id:"));
        assert!(source.contains("remote-secret-access-key:"));
    }
    assert!(CACHE_ACTION.contains("r2://*)"));
    assert!(CACHE_ACTION.contains("inputs.remote-access-key-id == ''"));
    assert!(CACHE_ACTION.contains("inputs.remote-secret-access-key == ''"));
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

#[test]
fn test_release_tag_gate_requires_the_current_publication_contract() {
    for fragment in [
        "trailers:key=Rail-Release-Contract,valueonly)' \"$tag_sha\")\" = 1",
        "trailers:key=Rail-Release-Mode,valueonly)' \"$tag_sha\")\" = run",
        "trailers:key=Rail-Release-Publish,valueonly)' \"$tag_sha\")\" = true",
        "trailers:key=Rail-Release-Publish-Registry,valueonly)' \"$tag_sha\")\" = crates-io",
        "trailers:key=Rail-Release-Crate,valueonly)' \"$tag_sha\")\" = \"cargo-rail@${RELEASE_TAG#v}\"",
        "trailers:key=Rail-Release-Crate-Publish,valueonly)' \"$tag_sha\")\" = cargo-rail=true",
        "trailers:key=Rail-Release-Tag,valueonly)' \"$tag_sha\")\" = true",
        "trailers:key=Rail-Release-Tag-Name,valueonly)' \"$tag_sha\")\" = \"cargo-rail=$RELEASE_TAG\"",
    ] {
        assert!(
            RELEASE_WORKFLOW.contains(fragment),
            "missing release trailer gate: {fragment}"
        );
        assert_eq!(
            RELEASE_WORKFLOW.matches(fragment).count(),
            1,
            "release trailer gate must be exact and singular: {fragment}"
        );
    }
}
