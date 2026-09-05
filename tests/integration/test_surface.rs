//! Front-door contracts for complete Rust source-surface analysis.

use std::fs;

use crate::helpers::{NestedWorkspace, TestWorkspace, run_cargo_rail, run_cargo_rail_with_env};
use anyhow::{Result, anyhow};
use rscrypto::Sha256;

const SURFACE_V1_SCHEMA: &str = include_str!("../../schemas/surface-v1.schema.json");
const SURFACE_V2_SCHEMA: &str = include_str!("../../schemas/surface-v2.schema.json");
const SURFACE_V3_SCHEMA: &str = include_str!("../../schemas/surface-v3.schema.json");

#[test]
fn compiler_observation_process_reports_its_private_protocol() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cargo-rail-compiler-observation"))
        .arg(cargo_rail::compiler::invocation::OBSERVATION_PROTOCOL_ARGUMENT)
        .output()
        .expect("run compiler observation protocol probe");

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8 protocol version").trim(),
        cargo_rail::compiler::invocation::OBSERVATION_PROTOCOL_VERSION.to_string()
    );
}

#[test]
fn surface_schema_is_pre_context_and_matches_the_published_contract() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_named("surface-schema-command")?;
        let output = run_cargo_rail(&workspace.path, &["rail", "surface", "--schema"])?;

        assert!(
            output.status.success(),
            "surface --schema should not load workspace state"
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), SURFACE_V3_SCHEMA);
        assert!(output.stderr.is_empty(), "schema output must keep stderr empty");
        let schema: serde_json::Value = serde_json::from_str(SURFACE_V3_SCHEMA)?;
        jsonschema::validator_for(&schema).map_err(|error| anyhow::anyhow!("invalid surface schema: {error}"))?;
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn historical_surface_schemas_remain_exactly_available() {
    for (schema, digest) in [
        (
            SURFACE_V1_SCHEMA,
            "3e74e75a833ee7cf1c7d504bd1541ee5311d2a070967e322552e1a9a43535145",
        ),
        (
            SURFACE_V2_SCHEMA,
            "101316fd270606ce5b8d9b1563c0a24cfa4648e61a68c109317b82b4dd3572dd",
        ),
    ] {
        let parsed: serde_json::Value = serde_json::from_str(schema).expect("valid historical schema");
        jsonschema::validator_for(&parsed).expect("valid historical JSON Schema");
        assert_eq!(
            Sha256::digest(schema.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            digest
        );
    }
}

#[test]
fn source_built_surface_fails_before_workspace_acquisition() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_named("surface-source-installation")?;
        fs::write(workspace.path.join("Cargo.toml"), "this is not Cargo metadata")?;

        for operation in ["--check", "--prepare"] {
            let output = run_cargo_rail(&workspace.path, &["rail", "surface", operation])?;

            assert_eq!(output.status.code(), Some(2));
            let stderr = String::from_utf8(output.stderr)?;
            assert!(stderr.contains("surface is unavailable in this source-built cargo-rail installation"));
            assert!(stderr.contains("cargo install does not provide surface"));
            assert!(
                !stderr.contains("metadata"),
                "workspace acquisition must not run for {operation}: {stderr}"
            );
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Typed target authority and wrapper observations must share the captured Git
/// source root even when Cargo owns a workspace below it.
#[test]
#[ignore = "requires the exact rustc-dev companion authority embedded by the protocol harness"]
fn surface_inspects_a_workspace_nested_below_its_git_root() {
    let result: Result<()> = (|| {
        let workspace = NestedWorkspace::new("rust")?;
        let package = workspace.add_crate("nested-surface-app", "0.1.0")?;
        fs::write(
            package.join("src/main.rs"),
            r#"fn main() {
  live();
}

pub fn live() {}
pub fn dead_public() {}
"#,
        )?;
        fs::write(
            workspace.workspace_root.join(".config/rail.toml"),
            "[surface]\nenabled = true\n",
        )?;
        fs::write(
            workspace.workspace_root.join("rust-toolchain.toml"),
            include_str!("../../rust-toolchain.toml"),
        )?;
        workspace.commit("Add nested surface fixture")?;

        let output = run_cargo_rail(&workspace.workspace_root, &["rail", "surface", "--format", "json"])?;
        assert!(
            output.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(report["mode"], "inspect");
        assert_eq!(report["config"]["enabled"], true);
        assert!(
            report["metrics"]["acquisition"]["compiler_invocations"]
                .as_u64()
                .is_some_and(|count| count > 0),
            "nested workspace analysis must acquire typed compiler facts"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Complete surface analysis reads authenticated typed compiler facts, so this
/// front-door contract only holds for a cargo-rail built with an embedded
/// driver authority. `just check-compiler-driver` provisions that
/// build; an ordinary source build has no producer authority to exercise.
#[test]
#[ignore = "requires the exact rustc-dev companion authority embedded by the protocol harness"]
fn surface_check_collects_production_tests_and_doctests_once() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_named("surface-complete-views")?;
        let package = workspace.add_crate("surface-app", "0.1.0", &[])?;
        let manifest = fs::read_to_string(package.join("Cargo.toml"))?.replace(
            "authors.workspace = true\n",
            "authors.workspace = true\npublish = false\n",
        );
        fs::write(package.join("Cargo.toml"), manifest)?;
        fs::write(
            package.join("src/macros.rs"),
            r#"macro_rules! define_public_unit {
  (
    $(#[$meta:meta])*
    $vis:vis struct $name:ident;
  ) => {
    $(#[$meta])*
    $vis struct $name;
  };
}
"#,
        )?;
        fs::write(
            package.join("src/lib.rs"),
            r#"//! Library API.

#[macro_use]
mod macros;

/// Return the fixture value.
///
/// ```
/// assert_eq!(surface_app::exported(), 7);
/// ```
///
/// ```compile_fail,E0308
/// let _: usize = "this doctest must fail to compile";
/// ```
pub fn exported() -> usize { 7 }

define_public_unit! {
  #[non_exhaustive]
  pub struct MacroGeneratedPublic;
}
"#,
        )?;
        fs::write(
            package.join("src/main.rs"),
            r#"fn main() {
  internal_public();
  assert_eq!(surface_app::exported(), 7);
}

pub fn internal_public() {}

pub fn dead_public() {}

#[cfg(test)]
pub fn test_only_public() {}

#[cfg(test)]
mod tests {
  #[test]
  fn registers_test_only_use() {
    super::test_only_public();
  }
}
"#,
        )?;
        fs::write(
            workspace.path.join(".config/rail.toml"),
            r#"[surface]
consumer_scope = "workspace"
crate_visibility = "allow"

[[surface.product]]
package = "surface-app"
bin = "surface-app"
reason = "fixture product"
"#,
        )?;
        fs::write(
            workspace.path.join("rust-toolchain.toml"),
            include_str!("../../rust-toolchain.toml"),
        )?;
        workspace.commit("Add complete surface fixture")?;

        let output = run_cargo_rail(&workspace.path, &["rail", "surface", "--check", "--format", "json"])?;
        assert_eq!(
            output.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "JSON mode must keep compiler progress off stderr"
        );

        let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let schema: serde_json::Value = serde_json::from_str(SURFACE_V3_SCHEMA)?;
        let validator =
            jsonschema::validator_for(&schema).map_err(|error| anyhow!("invalid surface schema: {error}"))?;
        let errors = validator
            .iter_errors(&report)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        assert!(errors.is_empty(), "surface report failed its schema: {errors:#?}");
        assert_eq!(report["metrics"]["acquisition"]["analysis_views"], 2);
        assert_eq!(report["metrics"]["acquisition"]["cargo_views_executed"], 2);
        assert_eq!(report["metrics"]["graph"]["traversals"], 3);
        assert_eq!(
            report["retention"]["fragment_item_observations"],
            report["completeness"]["items"]
        );
        assert_eq!(report["retention"]["merged_items"], report["metrics"]["graph"]["nodes"]);
        assert_eq!(
            report["retention"]["conservative_observations"],
            report["completeness"]["retentions"]
        );
        assert!(
            report["completeness"]["retention_reasons"]["generated-registration"]
                .as_u64()
                .is_some_and(|count| count > 0),
            "generated test-harness registration must remain named conservative evidence"
        );
        let generated_registration = report["retention"]["reasons"]
            .as_array()
            .and_then(|reasons| reasons.iter().find(|reason| reason["code"] == "generated-registration"))
            .ok_or_else(|| anyhow!("generated-registration retention detail is missing"))?;
        assert_eq!(
            generated_registration["observations"],
            report["completeness"]["retention_reasons"]["generated-registration"]
        );
        assert!(
            generated_registration["observations"].as_u64().unwrap()
                >= generated_registration["unique_items"].as_u64().unwrap()
        );
        assert!(
            generated_registration["predicate"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(
            generated_registration["representatives"]
                .as_array()
                .is_some_and(|examples| examples.len() <= 3)
        );
        assert!(
            report["retention"]["counterfactual"].is_null(),
            "normal Surface analysis must not pay for counterfactual graph traversals"
        );

        let findings = report["findings"]
            .as_array()
            .ok_or_else(|| anyhow!("surface findings are not an array"))?;
        let finding = |name: &str| findings.iter().find(|finding| finding["name"] == name);
        assert_eq!(
            finding("internal_public").map(|value| &value["kind"]),
            Some(&serde_json::json!("unnecessary-public"))
        );
        assert_eq!(
            finding("dead_public").map(|value| &value["kind"]),
            Some(&serde_json::json!("dead-public"))
        );
        assert_eq!(
            finding("test_only_public").and_then(|value| value["non_production_live"].as_bool()),
            Some(true)
        );
        assert!(
            finding("exported").is_none(),
            "cross-crate product API must remain public"
        );

        let repeated_check = run_cargo_rail(&workspace.path, &["rail", "surface", "--check", "--format", "json"])?;
        assert_eq!(
            repeated_check.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&repeated_check.stdout),
            String::from_utf8_lossy(&repeated_check.stderr)
        );
        let mut repeated_report: serde_json::Value = serde_json::from_slice(&repeated_check.stdout)?;
        let repeated_acquisition = &repeated_report["metrics"]["acquisition"];
        assert_eq!(
            repeated_acquisition["cargo_views_executed"], 0,
            "repeated acquisition metrics: {repeated_acquisition:#}"
        );
        assert_eq!(repeated_acquisition["compiler_invocations"], 0);
        assert_eq!(repeated_acquisition["fact_cache_hits"], 2);
        assert_eq!(repeated_acquisition["fact_cache_misses"], 0);
        let mut cold_report = report.clone();
        for normalized in [&mut cold_report, &mut repeated_report] {
            normalized["metrics"] = serde_json::Value::Null;
            normalized["cache"] = serde_json::Value::Null;
            normalized["fragments"] = serde_json::Value::Null;
        }
        assert_eq!(
            cold_report, repeated_report,
            "exact complete fact reuse must preserve the normalized Surface result"
        );

        let inspected = run_cargo_rail(&workspace.path, &["rail", "surface", "--format", "json"])?;
        assert!(
            inspected.status.success(),
            "inspection must report findings without turning them into a failing gate: stdout={} stderr={}",
            String::from_utf8_lossy(&inspected.stdout),
            String::from_utf8_lossy(&inspected.stderr)
        );
        let inspected_report: serde_json::Value = serde_json::from_slice(&inspected.stdout)?;
        assert_eq!(inspected_report["mode"], "inspect");
        assert_eq!(inspected_report["result"], "findings");
        assert_eq!(inspected_report["exit_code"], 0);
        assert!(
            inspected_report["findings"]
                .as_array()
                .is_some_and(|findings| !findings.is_empty())
        );

        let fixed = run_cargo_rail(
            &workspace.path,
            &["rail", "surface", "--fix", "--backup", "--format", "json"],
        )?;
        assert_eq!(
            fixed.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&fixed.stderr)
        );
        assert!(fixed.stderr.is_empty(), "JSON fix mode must keep stderr empty");
        let fixed_report: serde_json::Value = serde_json::from_slice(&fixed.stdout)?;
        let fixed_errors = validator
            .iter_errors(&fixed_report)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        assert!(
            fixed_errors.is_empty(),
            "surface fix report failed its schema: {fixed_errors:#?}"
        );
        let mut malformed_plan = fixed_report.clone();
        malformed_plan["mutation"]["plan"] = serde_json::json!({});
        assert!(
            validator.iter_errors(&malformed_plan).next().is_some(),
            "surface schema must own the exact nested mutation plan"
        );
        assert_eq!(fixed_report["mutation"]["phase"], "applied");
        assert_eq!(
            fixed_report["mutation"]["plan"]["actions"].as_array().map(Vec::len),
            Some(1)
        );
        let receipt = fixed_report["mutation"]["receipt"]
            .as_str()
            .ok_or_else(|| anyhow!("surface fix should publish a receipt"))?;
        assert!(workspace.path.join(receipt).is_file(), "surface receipt should exist");
        assert!(
            fixed_report["mutation"]["backup"].as_str().is_some(),
            "explicit backup should be reported"
        );

        let fixed_source = fs::read_to_string(package.join("src/main.rs"))?;
        assert!(fixed_source.contains("fn internal_public() {}"));
        assert!(fixed_source.contains("fn test_only_public() {}"));
        assert!(fixed_source.contains("pub fn dead_public() {}"));

        let repeated = run_cargo_rail(
            &workspace.path,
            &["rail", "surface", "--fix", "--dry-run", "--format", "json"],
        )?;
        assert_eq!(
            repeated.status.code(),
            Some(1),
            "dead-public remains report-only: {}",
            String::from_utf8_lossy(&repeated.stderr)
        );
        let repeated_report: serde_json::Value = serde_json::from_slice(&repeated.stdout)?;
        let repeated_errors = validator
            .iter_errors(&repeated_report)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        assert!(
            repeated_errors.is_empty(),
            "repeated surface fix report failed its schema: {repeated_errors:#?}"
        );
        assert_eq!(
            repeated_report["mutation"]["plan"]["actions"].as_array().map(Vec::len),
            Some(0),
            "one successful fix must leave no further visibility mutation"
        );
        let repeated_acquisition = &repeated_report["metrics"]["acquisition"];
        assert_eq!(
            repeated_acquisition["cargo_views_executed"], 0,
            "an unchanged warm run must not invoke Cargo: {repeated_acquisition}"
        );
        assert_eq!(
            repeated_acquisition["compiler_invocations"], 0,
            "an unchanged warm run must not invoke a compiler: {repeated_acquisition}"
        );
        assert_eq!(
            repeated_acquisition["fact_cache_hits"], 2,
            "both unchanged analysis views must reuse complete facts: {repeated_acquisition}"
        );
        assert_eq!(repeated_acquisition["fact_cache_misses"], 0);
        assert_eq!(repeated_acquisition["fact_cache_store_failures"], 0);
        assert_eq!(repeated_acquisition["fact_cache_bypass_reasons"], serde_json::json!({}));

        let github = run_cargo_rail(&workspace.path, &["rail", "surface", "--check", "--format", "github"])?;
        assert_eq!(github.status.code(), Some(1));
        assert!(github.stderr.is_empty(), "GitHub mode must keep progress off stderr");
        let github_output = String::from_utf8(github.stdout)?;
        assert!(github_output.contains("surface=true\n"));
        let embedded = github_output
            .lines()
            .find_map(|line| line.strip_prefix("surface_report_json="))
            .ok_or_else(|| anyhow!("GitHub output should embed the surface contract"))?;
        let embedded_report: serde_json::Value = serde_json::from_str(embedded)?;
        let embedded_errors = validator
            .iter_errors(&embedded_report)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        assert!(
            embedded_errors.is_empty(),
            "GitHub surface contract failed its schema: {embedded_errors:#?}"
        );

        let response_file = workspace.path.join("rustc.args");
        fs::write(&response_file, "--cfg\ncargo_rail_response_file\n")?;
        fs::create_dir_all(workspace.path.join(".cargo"))?;
        fs::write(
            workspace.path.join(".cargo/config.toml"),
            format!("[build]\nrustflags = ['@{}']\n", response_file.display()),
        )?;
        let configured_response = run_cargo_rail(&workspace.path, &["rail", "surface", "--check", "--format", "json"])?;
        assert_eq!(
            configured_response.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&configured_response.stdout),
            String::from_utf8_lossy(&configured_response.stderr)
        );
        let configured_report: serde_json::Value = serde_json::from_slice(&configured_response.stdout)?;
        let configured_acquisition = &configured_report["metrics"]["acquisition"];
        assert_eq!(configured_acquisition["cargo_views_executed"], 2);
        assert_eq!(configured_acquisition["fact_cache_hits"], 0);
        assert_eq!(
            configured_acquisition["fact_cache_bypass_reasons"]["response_file_configuration_unmodeled"], 2,
            "user-selected response-file bytes must remain fail-closed: {configured_acquisition}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
#[ignore = "requires the exact rustc-dev companion authority embedded by the protocol harness"]
fn surface_partial_acquisition_is_machine_resumable_and_reuses_exact_completed_views() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_named("surface-acquisition-resume")?;
        let package = workspace.add_crate("surface-resume-app", "0.1.0", &[])?;
        let manifest = fs::read_to_string(package.join("Cargo.toml"))?;
        let features = (0..12)
            .map(|index| format!("feature-{index} = []"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(
            package.join("Cargo.toml"),
            format!("{manifest}\n[features]\n{features}\n"),
        )?;
        fs::write(
            package.join("src/main.rs"),
            "fn main() { live(); }\npub fn live() {}\npub fn dead_public() {}\n",
        )?;
        let profiles = (0..12)
            .map(|index| {
                format!("[[surface.feature-profile]]\nname = \"profile-{index}\"\nfeatures = [\"feature-{index}\"]\n")
            })
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(
            workspace.path.join(".config/rail.toml"),
            format!(
                r#"[surface]
consumer_scope = "workspace"
doctest_coverage = "disabled"

[[surface.product]]
package = "surface-resume-app"
bin = "surface-resume-app"
reason = "resume fixture product"

{profiles}
"#
            ),
        )?;
        fs::write(
            workspace.path.join("rust-toolchain.toml"),
            include_str!("../../rust-toolchain.toml"),
        )?;
        workspace.commit("Add Surface resume fixture")?;

        let failed = run_cargo_rail_with_env(
            &workspace.path,
            &["rail", "surface", "--format", "json"],
            &[
                ("CARGO_BUILD_JOBS", "1"),
                ("CARGO_RAIL_SURFACE_FAIL_ACQUISITION_VIEW", "10"),
            ],
        )?;
        assert_eq!(failed.status.code(), Some(2));
        assert!(
            failed.stderr.is_empty(),
            "JSON failure must keep stderr empty: {}",
            String::from_utf8_lossy(&failed.stderr)
        );
        let failure: serde_json::Value = serde_json::from_slice(&failed.stdout)?;
        assert!(failure["help"].as_str().is_some_and(|help| help.contains("--resume")));

        let journal_directory = workspace.path.join("target/cargo-rail/surface-acquisitions-v2");
        let journals = fs::read_dir(&journal_directory)?.collect::<Result<Vec<_>, _>>()?;
        assert_eq!(journals.len(), 1);
        let journal = journals[0].path();
        let document: serde_json::Value = serde_json::from_slice(&fs::read(&journal)?)?;
        let header = &document["header"];
        assert_eq!(header["surface_acquisition_contract_version"], 2);
        assert_eq!(header["view_count"], 12);
        assert_eq!(header["concurrency"], 1);
        assert_eq!(header["products"][0]["package"], "surface-resume-app");
        let failed_view = document["views"]
            .as_array()
            .ok_or_else(|| anyhow!("v2 journal has no view array"))?
            .iter()
            .find(|record| record["durable"]["state"] == "failed")
            .ok_or_else(|| anyhow!("partial journal has no failed view"))?;
        assert!(
            failed_view["ordinal"].is_null(),
            "new v2 journals must not emit legacy ordinals"
        );
        assert_eq!(failed_view["view_index"], 9);
        assert_eq!(failed_view["target_triple"], "default");
        assert_eq!(failed_view["command_class"], "cargo-check-all-targets");
        assert_eq!(
            failed_view["selected_products"][0]["cargo_target"],
            "surface-resume-app"
        );
        let partial = &document["summary"];
        assert_eq!(partial["state"], "partial");
        assert_eq!(partial["completed"], 9);
        assert_eq!(partial["failed"], 1);
        assert_eq!(partial["pending"], 2);
        assert_eq!(partial["running"], 0);

        let journal_argument = journal
            .strip_prefix(&workspace.path)?
            .to_str()
            .ok_or_else(|| anyhow!("journal path is not UTF-8"))?;
        let resumed = run_cargo_rail_with_env(
            &workspace.path,
            &["rail", "surface", "--resume", journal_argument, "--format", "json"],
            &[("CARGO_BUILD_JOBS", "1")],
        )?;
        assert!(
            resumed.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&resumed.stdout),
            String::from_utf8_lossy(&resumed.stderr)
        );
        let mut resumed_report: serde_json::Value = serde_json::from_slice(&resumed.stdout)?;
        assert_eq!(resumed_report["metrics"]["acquisition"]["analysis_views"], 12);
        assert_eq!(resumed_report["metrics"]["acquisition"]["fact_cache_hits"], 9);
        assert_eq!(resumed_report["metrics"]["acquisition"]["cargo_views_executed"], 3);

        fs::remove_dir_all(workspace.path.join("target/cargo-rail-test-cache"))?;
        let cold = run_cargo_rail_with_env(
            &workspace.path,
            &["rail", "surface", "--format", "json"],
            &[("CARGO_BUILD_JOBS", "1")],
        )?;
        assert!(cold.status.success());
        let mut cold_report: serde_json::Value = serde_json::from_slice(&cold.stdout)?;
        for report in [&mut resumed_report, &mut cold_report] {
            report["metrics"] = serde_json::Value::Null;
            report["cache"] = serde_json::Value::Null;
            report["fragments"] = serde_json::Value::Null;
        }
        assert_eq!(
            resumed_report, cold_report,
            "resume and cold analysis must produce the same complete policy report"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
#[ignore = "requires the exact rustc-dev companion authority embedded by the protocol harness"]
fn surface_keeps_std_and_alloc_roots_package_local_across_resume_and_reuse() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = TestWorkspace::new_named("surface-package-feature-isolation")?;
        workspace.add_feature_isolation_crates()?;
        fs::write(
            workspace.path.join(".config/rail.toml"),
            r#"[surface]
consumer_scope = "workspace"
doctest_coverage = "disabled"

[[surface.product]]
package = "std-root"
lib = "std_root"
reason = "std feature root"

[[surface.product]]
package = "alloc-root"
lib = "alloc_root"
reason = "alloc-only feature root"

[[surface.feature-profile]]
name = "isolation"
features = ["isolate"]
"#,
        )?;
        fs::write(
            workspace.path.join("rust-toolchain.toml"),
            include_str!("../../rust-toolchain.toml"),
        )?;
        workspace.commit("Add package-local Surface feature roots")?;

        let trace_directory = workspace.path.join("target/cargo-rail-cargo-argv");
        fs::create_dir_all(&trace_directory)?;
        let cargo_recorder = workspace.path.join("target/cargo-rail-cargo-recorder");
        fs::write(
            &cargo_recorder,
            r#"#!/bin/sh
set -eu
trace="$(mktemp "$CARGO_RAIL_TEST_CARGO_LOG/argv.XXXXXX")"
printf '%s\0' "$@" > "$trace"
exec cargo "$@"
"#,
        )?;
        let mut permissions = fs::metadata(&cargo_recorder)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&cargo_recorder, permissions)?;
        let cargo_recorder = cargo_recorder
            .to_str()
            .ok_or_else(|| anyhow!("Cargo recorder path is not UTF-8"))?;
        let trace_directory_value = trace_directory
            .to_str()
            .ok_or_else(|| anyhow!("Cargo trace path is not UTF-8"))?;

        let failed = run_cargo_rail_with_env(
            &workspace.path,
            &["rail", "surface", "--format", "json"],
            &[
                ("CARGO", cargo_recorder),
                ("CARGO_BUILD_JOBS", "1"),
                ("CARGO_RAIL_TEST_CARGO_LOG", trace_directory_value),
                ("CARGO_RAIL_SURFACE_FAIL_ACQUISITION_VIEW", "2"),
            ],
        )?;
        assert_eq!(failed.status.code(), Some(2));

        let journal_directory = workspace.path.join("target/cargo-rail/surface-acquisitions-v2");
        let journals = fs::read_dir(&journal_directory)?.collect::<Result<Vec<_>, _>>()?;
        assert_eq!(journals.len(), 1);
        let journal = journals[0].path();
        let partial: serde_json::Value = serde_json::from_slice(&fs::read(&journal)?)?;
        let views = partial["views"].as_array().ok_or_else(|| anyhow!("journal views"))?;
        assert_eq!(views.len(), 3);
        let mut identities = std::collections::BTreeMap::new();
        for view in views {
            let packages = view["packages"]
                .as_array()
                .ok_or_else(|| anyhow!("journal package authority"))?;
            assert_eq!(packages.len(), 1, "journal view must bind one package: {view:#}");
            let package = packages[0]
                .as_str()
                .ok_or_else(|| anyhow!("journal package is not text"))?;
            assert!(
                identities
                    .insert(package, view["view_identity"].as_str().expect("view identity"))
                    .is_none(),
                "one configured Surface profile should produce one view per package"
            );
        }
        assert_eq!(
            identities.keys().copied().collect::<Vec<_>>(),
            vec!["alloc-root", "shared", "std-root"]
        );
        assert_eq!(
            identities
                .values()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            3,
            "package authority must bind otherwise equal target and feature selections"
        );

        let journal_argument = journal
            .strip_prefix(&workspace.path)?
            .to_str()
            .ok_or_else(|| anyhow!("journal path is not UTF-8"))?;
        let resumed = run_cargo_rail_with_env(
            &workspace.path,
            &["rail", "surface", "--resume", journal_argument, "--format", "json"],
            &[
                ("CARGO", cargo_recorder),
                ("CARGO_BUILD_JOBS", "1"),
                ("CARGO_RAIL_TEST_CARGO_LOG", trace_directory_value),
            ],
        )?;
        assert!(
            resumed.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&resumed.stdout),
            String::from_utf8_lossy(&resumed.stderr)
        );
        let resumed_report: serde_json::Value = serde_json::from_slice(&resumed.stdout)?;
        let acquisition = &resumed_report["metrics"]["acquisition"];
        assert_eq!(acquisition["analysis_views"], 3);
        assert_eq!(acquisition["fact_cache_hits"], 1);
        assert_eq!(acquisition["cargo_views_executed"], 2);

        let completed: serde_json::Value = serde_json::from_slice(&fs::read(&journal)?)?;
        assert_eq!(completed["summary"]["state"], "complete");
        let evidence = completed["views"]
            .as_array()
            .expect("completed views")
            .iter()
            .map(|view| view["durable"]["evidence"].as_str().expect("completed evidence"))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(evidence.len(), 3, "resume evidence must remain package-bound");

        let recorded_acquisitions = || -> Result<Vec<Vec<String>>> {
            let mut invocations = fs::read_dir(&trace_directory)?
                .map(|entry| {
                    let bytes = fs::read(entry?.path())?;
                    bytes
                        .split(|byte| *byte == 0)
                        .filter(|argument| !argument.is_empty())
                        .map(|argument| String::from_utf8(argument.to_vec()).map_err(Into::into))
                        .collect::<Result<Vec<_>>>()
                })
                .collect::<Result<Vec<_>>>()?;
            invocations.retain(|arguments| {
                arguments.first().is_some_and(|argument| argument == "check")
                    && arguments.iter().any(|argument| argument == "--all-targets")
                    && arguments.iter().any(|argument| argument == "--message-format=json")
            });
            Ok(invocations)
        };
        let acquisitions = recorded_acquisitions()?;
        assert_eq!(
            acquisitions.len(),
            3,
            "each package should execute exactly once: {acquisitions:#?}"
        );
        let mut acquired_packages = std::collections::BTreeSet::new();
        for arguments in &acquisitions {
            let package_positions = arguments
                .iter()
                .enumerate()
                .filter_map(|(index, argument)| (argument == "--package").then_some(index))
                .collect::<Vec<_>>();
            assert_eq!(
                package_positions.len(),
                1,
                "one package selector per Cargo view: {arguments:?}"
            );
            let package = arguments
                .get(package_positions[0] + 1)
                .ok_or_else(|| anyhow!("Cargo package selector has no value"))?;
            assert!(acquired_packages.insert(package.as_str()));
            let feature_values = arguments
                .windows(2)
                .filter_map(|pair| (pair[0] == "--features").then_some(pair[1].as_str()))
                .collect::<Vec<_>>();
            assert_eq!(feature_values, vec![format!("{package}/isolate")]);
        }
        assert_eq!(
            acquired_packages,
            std::collections::BTreeSet::from(["alloc-root", "shared", "std-root"])
        );

        let warm = run_cargo_rail_with_env(
            &workspace.path,
            &["rail", "surface", "--format", "json"],
            &[
                ("CARGO", cargo_recorder),
                ("CARGO_BUILD_JOBS", "1"),
                ("CARGO_RAIL_TEST_CARGO_LOG", trace_directory_value),
            ],
        )?;
        assert!(warm.status.success());
        let warm_report: serde_json::Value = serde_json::from_slice(&warm.stdout)?;
        let warm_acquisition = &warm_report["metrics"]["acquisition"];
        assert_eq!(warm_acquisition["cargo_views_executed"], 0);
        assert_eq!(warm_acquisition["compiler_invocations"], 0);
        assert_eq!(warm_acquisition["fact_cache_hits"], 3);
        assert_eq!(recorded_acquisitions()?.len(), 3, "warm Surface must not invoke Cargo");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
#[ignore = "requires the exact rustc-dev companion authority embedded by the protocol harness"]
fn surface_fix_failure_matrix_restores_every_written_source() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_named("surface-failure-matrix")?;
        let package = workspace.add_crate("surface-fault-app", "0.1.0", &[])?;
        let manifest = fs::read_to_string(package.join("Cargo.toml"))?.replace(
            "authors.workspace = true\n",
            "authors.workspace = true\npublish = false\n",
        );
        fs::write(package.join("Cargo.toml"), manifest)?;
        fs::remove_file(package.join("src/lib.rs"))?;
        let main_source = r#"mod support;

fn main() {
  first();
  support::second();
}

pub fn first() {}
"#;
        let support_source = "pub fn second() {}\n";
        fs::write(package.join("src/main.rs"), main_source)?;
        fs::write(package.join("src/support.rs"), support_source)?;
        fs::write(
            workspace.path.join(".config/rail.toml"),
            r#"[surface]
consumer_scope = "workspace"

[[surface.product]]
package = "surface-fault-app"
bin = "surface-fault-app"
reason = "failure-matrix product"
"#,
        )?;
        fs::write(
            workspace.path.join("rust-toolchain.toml"),
            include_str!("../../rust-toolchain.toml"),
        )?;
        workspace.commit("Add surface failure fixture")?;

        for point in [
            "first-write",
            "partial-write",
            "post-write-validation",
            "recompilation",
            "receipt-write",
        ] {
            let output = run_cargo_rail_with_env(
                &workspace.path,
                &["rail", "surface", "--fix"],
                &[("CARGO_RAIL_SURFACE_FAIL_AT", point)],
            )?;
            assert_eq!(
                output.status.code(),
                Some(2),
                "{point}: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(&format!("injected surface failure at {point}")),
                "{point}: unexpected stderr={}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(fs::read_to_string(package.join("src/main.rs"))?, main_source, "{point}");
            assert_eq!(
                fs::read_to_string(package.join("src/support.rs"))?,
                support_source,
                "{point}"
            );
        }

        fs::write(
            workspace.path.join(".config/rail.toml"),
            r#"[surface]
consumer_scope = "workspace"

[[surface.product]]
package = "surface-fault-app"
bin = "surface-fault-app"
reason = "failure-matrix product"

[[surface.override]]
lint = "unnecessary-public"
package = "surface-fault-app"
item = "missing_item"
kind = "function"
level = "deny"
reason = "the configured item must exist"
"#,
        )?;
        let blocked = run_cargo_rail(&workspace.path, &["rail", "surface", "--fix", "--format", "json"])?;
        assert_eq!(blocked.status.code(), Some(1));
        let blocked_report: serde_json::Value = serde_json::from_slice(&blocked.stdout)?;
        assert_eq!(blocked_report["mutation"]["phase"], "planned");
        assert_eq!(blocked_report["configuration_diagnostics"][0]["code"], "unknown-item");
        assert_eq!(fs::read_to_string(package.join("src/main.rs"))?, main_source);
        assert_eq!(fs::read_to_string(package.join("src/support.rs"))?, support_source);

        let explained = run_cargo_rail(&workspace.path, &["rail", "surface", "--check", "--explain"])?;
        assert_eq!(explained.status.code(), Some(1));
        let explained = String::from_utf8(explained.stdout)?;
        assert!(explained.contains("1 configuration diagnostic(s)"));
        assert!(explained.contains("configuration unknown-item at surface.override[0]"));
        assert!(explained.contains("reason: the configured item must exist"));
        Ok(())
    })();
    super::helpers::finish_test(result);
}
