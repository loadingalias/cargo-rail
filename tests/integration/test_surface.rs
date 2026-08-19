//! Front-door contracts for complete Rust source-surface analysis.

use std::fs;

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::{Result, anyhow};

const SURFACE_V1_SCHEMA: &str = include_str!("../../schemas/surface-v1.schema.json");

#[test]
fn surface_schema_is_pre_context_and_matches_the_published_contract() -> Result<()> {
  let workspace = TestWorkspace::new_named("surface-schema-command")?;
  let output = run_cargo_rail(&workspace.path, &["rail", "surface", "--schema"])?;

  assert!(
    output.status.success(),
    "surface --schema should not load workspace state"
  );
  assert_eq!(String::from_utf8_lossy(&output.stdout), SURFACE_V1_SCHEMA);
  assert!(output.stderr.is_empty(), "schema output must keep stderr empty");
  let schema: serde_json::Value = serde_json::from_str(SURFACE_V1_SCHEMA)?;
  jsonschema::validator_for(&schema).map_err(|error| anyhow::anyhow!("invalid surface schema: {error}"))?;
  Ok(())
}

/// Complete surface analysis reads authenticated typed compiler facts, so this
/// front-door contract only holds for a cargo-rail built with an embedded
/// driver authority. `scripts/test-compiler-fact-protocol.sh` provisions that
/// build; an ordinary source build has no producer authority to exercise.
#[test]
#[ignore = "requires the exact rustc-dev companion authority embedded by the protocol harness"]
fn surface_check_collects_production_tests_and_doctests_once() -> Result<()> {
  let workspace = TestWorkspace::new_named("surface-complete-views")?;
  let package = workspace.add_crate("surface-app", "0.1.0", &[])?;
  let manifest = fs::read_to_string(package.join("Cargo.toml"))?.replace(
    "authors.workspace = true\n",
    "authors.workspace = true\npublish = false\n",
  );
  fs::write(package.join("Cargo.toml"), manifest)?;
  fs::write(
    package.join("src/lib.rs"),
    r#"//! Library API.

/// Return the fixture value.
///
/// ```
/// assert_eq!(surface_app::exported(), 7);
/// ```
pub fn exported() -> usize { 7 }
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
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(
    output.stderr.is_empty(),
    "JSON mode must keep compiler progress off stderr"
  );

  let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  let schema: serde_json::Value = serde_json::from_str(SURFACE_V1_SCHEMA)?;
  let validator = jsonschema::validator_for(&schema).map_err(|error| anyhow!("invalid surface schema: {error}"))?;
  let errors = validator
    .iter_errors(&report)
    .map(|error| error.to_string())
    .collect::<Vec<_>>();
  assert!(errors.is_empty(), "surface report failed its schema: {errors:#?}");
  assert_eq!(report["metrics"]["acquisition"]["analysis_views"], 2);
  assert_eq!(report["metrics"]["acquisition"]["cargo_views_executed"], 2);
  assert_eq!(report["metrics"]["graph"]["traversals"], 3);
  assert!(
    report["completeness"]["retention_reasons"]["generated-registration"]
      .as_u64()
      .is_some_and(|count| count > 0),
    "generated test-harness registration must remain named conservative evidence"
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
  let cargo_views = repeated_acquisition["cargo_views_executed"]
    .as_u64()
    .ok_or_else(|| anyhow!("cargo view metric is not an integer"))?;
  let fact_hits = repeated_acquisition["fact_cache_hits"]
    .as_u64()
    .ok_or_else(|| anyhow!("fact hit metric is not an integer"))?;
  assert_eq!(cargo_views + fact_hits, 2, "every warm view must hit or execute");
  assert!(fact_hits >= 1, "the warm run should reuse at least one complete view");
  if cargo_views > 0 {
    assert!(
      repeated_acquisition["fact_cache_bypass_reasons"]
        .as_object()
        .is_some_and(|reasons| !reasons.is_empty()),
      "an uncacheable warm view must name its conservative bypass: {repeated_acquisition}"
    );
  }

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
  Ok(())
}
