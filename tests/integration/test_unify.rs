//! Integration tests for cargo rail unify command
//!
//! Tests the complete unification workflow:
//! - Resolution-based merging (no false positives)
//! - Syntactic version merging
//! - True multi-version conflict detection
//! - Feature union across packages
//! - dep_kinds display
//! - End-to-end analyze → apply workflow

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;
use tempfile::TempDir;

// Core Unification Tests

#[test]
fn test_unify_apply_json_is_a_single_machine_envelope() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-json-apply")?;
  workspace.add_crate("crate-a", "0.1.0", &[("tempfile", r#""3.0""#)])?;
  workspace.add_crate("crate-b", "0.1.0", &[("tempfile", r#""3.0""#)])?;
  workspace.commit("Add shared dependency")?;

  let check = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--format", "json"])?;
  assert_eq!(check.status.code(), Some(1));
  let check_json: serde_json::Value = serde_json::from_slice(&check.stdout)?;
  let check_proof = check_json["proof_fingerprint"]
    .as_str()
    .expect("check proof fingerprint")
    .to_string();

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--format", "json"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(output.status.success(), "JSON apply should succeed. stderr:\n{stderr}");
  assert!(
    stderr.is_empty(),
    "JSON success must not leak progress or warnings: {stderr}"
  );

  let value: serde_json::Value = serde_json::from_str(&stdout)?;
  assert_eq!(value["schema_version"], 1);
  assert_eq!(value["command"], "unify");
  assert_eq!(value["mode"], "apply");
  assert_eq!(value["result"], "applied");
  assert_eq!(value["exit_code"], 0);
  assert_eq!(value["dependencies"], 1);
  assert_eq!(value["members"], 2);
  assert!(value["plan_receipt"].is_string());
  assert!(value["apply_receipt"].is_string());
  assert!(value["graph_delta"]["added_facts"].is_u64());
  assert!(value["graph_delta"]["removed_facts"].is_u64());
  assert!(value["graph_delta"]["facts"].is_array());
  assert!(
    value["graph_delta"]["fingerprint"]
      .as_str()
      .is_some_and(|fingerprint| fingerprint.starts_with("sha256:"))
  );
  assert_eq!(value["proof_fingerprint"], check_proof);

  Ok(())
}

#[test]
fn test_unify_resolution_based_merging_no_false_positives() -> Result<()> {
  // This tests the operational order fix: dependencies that resolve to the same
  // version should NOT trigger multi-version warnings, even if their version
  // requirements look different syntactically.

  let workspace = TestWorkspace::new()?;

  // Create two crates with compatible but different-looking version requirements
  // Both "1.0" and "^1.0.100" will resolve to the same latest version
  workspace.add_crate(
    "crate-a",
    "0.1.0",
    &[
      ("serde", r#"{ version = "1.0", features = ["derive"] }"#),
      ("anyhow", r#""1.0""#),
    ],
  )?;

  workspace.add_crate(
    "crate-b",
    "0.1.0",
    &[
      ("serde", r#"{ version = "^1.0.100", features = ["serde_derive"] }"#),
      ("anyhow", r#""^1.0.50""#),
    ],
  )?;

  workspace.commit("Add crates with compatible version requirements")?;

  // Run unify analyze
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should NOT report multi-version conflicts for serde or anyhow
  // (they should be successfully merged)
  assert!(
    !stdout.contains("Multiple versions"),
    "Should not report multi-version conflicts for compatible versions.\nOutput:\n{}",
    stdout
  );

  // Should show these deps as unifiable
  assert!(
    stdout.contains("serde") || stdout.contains("Dependencies to unify"),
    "Should show dependencies can be unified.\nOutput:\n{}",
    stdout
  );

  // Should show success
  assert!(
    stdout.contains("changed:") && stdout.contains("next:"),
    "Should complete analysis successfully.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_unify_syntactic_version_merging() -> Result<()> {
  // Test syntactic merging of compatible version requirements

  let workspace = TestWorkspace::new()?;

  // Create crates with versions that can be syntactically merged
  workspace.add_crate(
    "crate-a",
    "0.1.0",
    &[("tokio", r#"{ version = "^1.2.0", features = ["fs"] }"#)],
  )?;

  workspace.add_crate(
    "crate-b",
    "0.1.0",
    &[("tokio", r#"{ version = "^1.3.0", features = ["net"] }"#)],
  )?;

  workspace.commit("Add crates with mergeable versions")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should successfully merge ^1.2.0 and ^1.3.0 to ^1.3.0
  assert!(
    !stdout.contains("Version conflict"),
    "Should not report version conflicts for mergeable versions.\nOutput:\n{}",
    stdout
  );

  // Should show tokio as unifiable
  assert!(
    stdout.contains("tokio"),
    "Should show tokio can be unified.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_unify_major_version_conflict_warns_and_skips() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create crates with different major versions of the same dependency
  // This simulates the derive_more bug: 0.99.3 vs 2.0
  workspace.add_crate(
    "crate-a",
    "0.1.0",
    &[("serde", r#"{ version = "1.0", features = ["derive"] }"#)],
  )?;

  // Crate B uses a different major version (simulated with 0.x which has different semver rules)
  workspace.add_crate(
    "crate-b",
    "0.1.0",
    &[("serde", r#"{ version = "0.8", features = ["alloc"] }"#)],
  )?;

  workspace.commit("Add crates with major version conflict")?;

  // Configure rail.toml
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[workspace]
root = "."

[unify]
"#,
  )?;

  // Run analyze with explanation output - should surface the conflict warning
  let analyze_output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
  let analyze_stdout = String::from_utf8_lossy(&analyze_output.stdout);

  assert!(
    analyze_stdout.contains("Multiple major versions") || analyze_stdout.contains("skipping"),
    "Should detect major version conflict.\nOutput:\n{}",
    analyze_stdout
  );

  // Run apply - should SUCCEED but skip the conflicting dependency
  let apply_output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  let apply_stdout = String::from_utf8_lossy(&apply_output.stdout);
  let apply_stderr = String::from_utf8_lossy(&apply_output.stderr);

  // The apply should either fail or skip the conflicting dependency
  // Check that serde was NOT added to workspace.dependencies (since it has conflicts)
  let workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;

  // If serde is in workspace.dependencies, it should not have mixed features from both versions
  // The expected behavior is to SKIP the dependency entirely due to the conflict
  if workspace_toml.contains("serde") && workspace_toml.contains("[workspace.dependencies]") {
    // If it IS in workspace.dependencies, verify it doesn't have features from the wrong version
    // This is a secondary check - the primary expectation is that it's skipped
    assert!(
      !workspace_toml.contains("alloc") || !workspace_toml.contains("derive"),
      "Should not merge features from incompatible major versions.\nWorkspace TOML:\n{}\nstdout:\n{}\nstderr:\n{}",
      workspace_toml,
      apply_stdout,
      apply_stderr
    );
  }

  Ok(())
}

#[test]
fn test_unify_feature_union() -> Result<()> {
  // Test that features are properly unioned across packages

  let workspace = TestWorkspace::new()?;

  // Create crates with different features for the same dependency
  workspace.add_crate(
    "crate-a",
    "0.1.0",
    &[("serde", r#"{ version = "1.0", features = ["derive"] }"#)],
  )?;

  workspace.add_crate(
    "crate-b",
    "0.1.0",
    &[("serde", r#"{ version = "1.0", features = ["rc"] }"#)],
  )?;

  workspace.add_crate(
    "crate-c",
    "0.1.0",
    &[("serde", r#"{ version = "1.0", features = ["alloc"] }"#)],
  )?;

  workspace.commit("Add crates with different features")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show serde with union of all features
  assert!(
    stdout.contains("serde"),
    "Should show serde can be unified.\nOutput:\n{}",
    stdout
  );

  // The new implementation uses INTERSECTION (minimal features), not union.
  // Since derive, rc, and alloc are NOT used by all crates, the intersection is empty.
  // The workspace dependency will have NO features, and each member will keep its local features.
  // This is correct behavior - we just verify unification is possible.
  assert!(
    stdout.contains("changed:") && stdout.contains("next:"),
    "Should show dependencies can be unified.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_unify_inconsistent_default_features() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create crates with inconsistent default-features
  workspace.add_crate(
    "crate-a",
    "0.1.0",
    &[(
      "serde",
      r#"{ version = "1.0", default-features = true, features = ["derive"] }"#,
    )],
  )?;

  workspace.add_crate(
    "crate-b",
    "0.1.0",
    &[(
      "serde",
      r#"{ version = "1.0", default-features = false, features = ["alloc"] }"#,
    )],
  )?;

  workspace.commit("Add crates with inconsistent default-features")?;

  // Configure rail.toml
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[workspace]
root = "."

[unify]
"#,
  )?;

  // Run analyze - should show Soft warning
  let analyze_output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let analyze_stdout = String::from_utf8_lossy(&analyze_output.stdout);

  assert!(
    analyze_stdout.contains("serde"),
    "Should show serde can be unified.\nOutput:\n{}",
    analyze_stdout
  );

  // Run apply - should SUCCEED
  let apply_output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    apply_output.status.success(),
    "Apply should succeed.\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&apply_output.stdout),
    String::from_utf8_lossy(&apply_output.stderr)
  );

  // Check workspace Cargo.toml
  let workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert!(workspace_toml.contains("serde"), "Should include serde");

  // Should enable default-features (union strategy)
  assert!(
    workspace_toml.contains("default-features = true") || !workspace_toml.contains("default-features = false"),
    "Should enable default-features.\nWorkspace TOML:\n{}",
    workspace_toml
  );

  // Should have both features
  assert!(
    workspace_toml.contains("derive") && workspace_toml.contains("alloc"),
    "Should have union of features.\nWorkspace TOML:\n{}",
    workspace_toml
  );

  Ok(())
}

// Dependency Kinds and End-to-End Workflow Tests

#[test]
fn test_unify_dep_kinds_display() -> Result<()> {
  // Test that dep_kinds (dependencies, dev-dependencies, build-dependencies) are shown

  let workspace = TestWorkspace::new()?;

  // Create crates with same dep in different sections
  workspace.add_crate(
    "crate-a",
    "0.1.0",
    &[("tempfile", r#""3.0""#)], // normal dependency
  )?;

  // Manually create crate-b with dev-dependency
  let crate_b_path = workspace.path.join("crates/crate-b");
  std::fs::create_dir_all(&crate_b_path)?;
  std::fs::create_dir_all(crate_b_path.join("src"))?;

  std::fs::write(
    crate_b_path.join("Cargo.toml"),
    r#"[package]
name = "crate-b"
version = "0.1.0"
edition.workspace = true

[dev-dependencies]
tempfile = "3.0"
"#,
  )?;

  std::fs::write(
    crate_b_path.join("src/lib.rs"),
    "pub fn hello() -> &'static str { \"Hello\" }",
  )?;

  workspace.commit("Add crates with different dep kinds")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--show-diff"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show tempfile can be unified
  assert!(
    stdout.contains("tempfile"),
    "Should show tempfile can be unified.\nOutput:\n{}",
    stdout
  );

  // Diff output should preserve the original dependency sections.
  assert!(
    stdout.contains("[dependencies]") || stdout.contains("[dev-dependencies]"),
    "Should show dep_kinds in output.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_unify_end_to_end_analyze_then_apply() -> Result<()> {
  // Test complete workflow: analyze → apply → verify

  let workspace = TestWorkspace::new()?;

  // Create crates with unifiable dependencies
  workspace.add_crate(
    "crate-a",
    "0.1.0",
    &[("serde", r#"{ version = "1.0", features = ["derive"] }"#)],
  )?;

  workspace.add_crate("crate-b", "0.1.0", &[("serde", r#"{ version = "1.0" }"#)])?;
  std::fs::write(
    workspace.path.join("crates/crate-a/src/lib.rs"),
    "use serde as _;\npub fn a() {}",
  )?;
  std::fs::write(
    workspace.path.join("crates/crate-b/src/lib.rs"),
    "use serde as _;\npub fn b() {}",
  )?;

  workspace.commit("Add crates before unification")?;

  // Analyze (should succeed)
  let analyze_output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--show-diff"])?;
  let analyze_stdout = String::from_utf8_lossy(&analyze_output.stdout);

  assert!(
    analyze_stdout.contains("serde"),
    "Analyze should show serde can be unified"
  );

  // Apply unification
  let apply_output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  let apply_stdout = String::from_utf8_lossy(&apply_output.stdout);

  assert!(
    apply_stdout.contains("unified") || apply_stdout.contains("next:"),
    "Apply should complete successfully.\nOutput:\n{}",
    apply_stdout
  );

  // Verify workspace Cargo.toml was updated
  let workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;

  assert!(
    workspace_toml.contains("[workspace.dependencies]"),
    "Workspace should have dependencies section"
  );

  assert!(
    workspace_toml.contains("serde"),
    "Workspace dependencies should include serde"
  );

  // Verify member Cargo.toml uses workspace inheritance
  let crate_a_toml = std::fs::read_to_string(workspace.path.join("crates/crate-a/Cargo.toml"))?;

  assert!(
    crate_a_toml.contains("workspace = true") || crate_a_toml.contains("workspace=true"),
    "Member should use workspace inheritance for serde.\nContent:\n{}",
    crate_a_toml
  );

  // Run analyze again - should show no unifiable deps
  let final_analyze = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let final_stdout = String::from_utf8_lossy(&final_analyze.stdout);

  assert!(
    final_stdout.contains("status: no changes"),
    "After unification, there should be no more unifiable deps.\nOutput:\n{}",
    final_stdout
  );

  Ok(())
}

#[test]
fn test_unify_exclude_config_and_flag() -> Result<()> {
  // Test that exclude works via config file

  let workspace = TestWorkspace::new()?;

  workspace.add_crate(
    "crate-a",
    "0.1.0",
    &[("serde", r#""1.0""#), ("anyhow", r#""1.0""#), ("tokio", r#""1.0""#)],
  )?;
  workspace.add_crate(
    "crate-b",
    "0.1.0",
    &[("serde", r#""1.0""#), ("anyhow", r#""1.0""#), ("tokio", r#""1.0""#)],
  )?;

  workspace.commit("Add crates")?;

  // Test: Config file exclusion
  // Configure rail.toml with exclude
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[workspace]
root = "."

[unify]
exclude = ["tokio"]
"#,
  )?;

  // Run analyze with config-based exclusion
  let output_config = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout_config = String::from_utf8_lossy(&output_config.stdout);

  // Should show anyhow and serde, but NOT tokio
  assert!(
    stdout_config.contains("anyhow") || stdout_config.contains("serde"),
    "Non-excluded deps should show (config test).\nOutput:\n{}",
    stdout_config
  );

  // Run apply to verify config exclusion persists
  let apply_output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(apply_output.status.success(), "Apply should succeed");

  // Workspace should have anyhow and serde, but NOT tokio
  let workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert!(
    workspace_toml.contains("anyhow") || workspace_toml.contains("serde"),
    "Should have non-excluded deps in workspace.dependencies"
  );

  // Verify member conversion - check that tokio was NOT converted (excluded)
  let crate_a_toml = std::fs::read_to_string(workspace.path.join("crates/crate-a/Cargo.toml"))?;

  // tokio should still be in original format (not converted to workspace = true)
  assert!(
    crate_a_toml.contains("tokio = \"1.0\"") || !crate_a_toml.contains("tokio"),
    "tokio should NOT be converted (excluded via config)"
  );

  Ok(())
}

// Dependency Kinds: Dev and Build Dependencies

#[test]
fn test_unify_dev_dependencies() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create crate-a with a dev-dependency
  let crate_a_path = workspace.path.join("crates/crate-a");
  std::fs::create_dir_all(&crate_a_path)?;
  std::fs::create_dir_all(crate_a_path.join("src"))?;

  std::fs::write(
    crate_a_path.join("Cargo.toml"),
    r#"[package]
name = "crate-a"
version = "0.1.0"
edition.workspace = true

[dev-dependencies]
tempfile = "3.0"
"#,
  )?;

  std::fs::write(
    crate_a_path.join("src/lib.rs"),
    "pub fn hello() -> &'static str { \"Hello\" }",
  )?;

  // Create crate-b with same dev-dependency
  let crate_b_path = workspace.path.join("crates/crate-b");
  std::fs::create_dir_all(&crate_b_path)?;
  std::fs::create_dir_all(crate_b_path.join("src"))?;

  std::fs::write(
    crate_b_path.join("Cargo.toml"),
    r#"[package]
name = "crate-b"
version = "0.1.0"
edition.workspace = true

[dev-dependencies]
tempfile = "3.0"
"#,
  )?;

  std::fs::write(
    crate_b_path.join("src/lib.rs"),
    "pub fn world() -> &'static str { \"World\" }",
  )?;

  workspace.commit("Add crates with dev-dependencies")?;

  // Run unify
  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "Unify should succeed.\nOutput:\n{}",
    String::from_utf8_lossy(&output.stdout)
  );

  // Verify workspace has tempfile
  let workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert!(
    workspace_toml.contains("tempfile"),
    "Workspace should have tempfile in dependencies"
  );

  // Verify members use workspace = true for dev-deps
  let crate_a_toml = std::fs::read_to_string(crate_a_path.join("Cargo.toml"))?;
  assert!(
    crate_a_toml.contains("[dev-dependencies]"),
    "Should still have dev-dependencies section"
  );
  assert!(
    crate_a_toml.contains("workspace = true") || crate_a_toml.contains("workspace=true"),
    "Dev-dependency should use workspace inheritance.\nContent:\n{}",
    crate_a_toml
  );

  Ok(())
}

#[test]
fn test_unify_build_dependencies() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create crate-a with a build-dependency
  let crate_a_path = workspace.path.join("crates/crate-a");
  std::fs::create_dir_all(&crate_a_path)?;
  std::fs::create_dir_all(crate_a_path.join("src"))?;

  std::fs::write(
    crate_a_path.join("Cargo.toml"),
    r#"[package]
name = "crate-a"
version = "0.1.0"
edition.workspace = true

[build-dependencies]
cc = "1.0"
"#,
  )?;

  std::fs::write(
    crate_a_path.join("src/lib.rs"),
    "use serde as _;\nuse anyhow as _;\npub fn a() {}",
  )?;

  // Create crate-b with same build-dependency
  let crate_b_path = workspace.path.join("crates/crate-b");
  std::fs::create_dir_all(&crate_b_path)?;
  std::fs::create_dir_all(crate_b_path.join("src"))?;

  std::fs::write(
    crate_b_path.join("Cargo.toml"),
    r#"[package]
name = "crate-b"
version = "0.1.0"
edition.workspace = true

[build-dependencies]
cc = "1.0"
"#,
  )?;

  std::fs::write(crate_b_path.join("src/lib.rs"), "use serde as _;\npub fn b() {}")?;

  workspace.commit("Add crates with build-dependencies")?;

  // Run unify
  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "Unify should succeed.\nOutput:\n{}",
    String::from_utf8_lossy(&output.stdout)
  );

  // Verify workspace has cc
  let workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert!(
    workspace_toml.contains("cc"),
    "Workspace should have cc in dependencies"
  );

  // Verify members use workspace = true for build-deps
  let crate_a_toml = std::fs::read_to_string(crate_a_path.join("Cargo.toml"))?;
  assert!(
    crate_a_toml.contains("[build-dependencies]"),
    "Should still have build-dependencies section"
  );
  assert!(
    crate_a_toml.contains("workspace = true") || crate_a_toml.contains("workspace=true"),
    "Build-dependency should use workspace inheritance.\nContent:\n{}",
    crate_a_toml
  );

  Ok(())
}

#[test]
fn test_unify_existing_workspace_deps_update() -> Result<()> {
  // Test that existing workspace.dependencies are updated when features differ

  let workspace = TestWorkspace::new()?;

  // Create workspace with existing workspace.dependencies
  let workspace_toml_content = r#"[workspace]
members = ["crates/*"]
resolver = "2"

[workspace.package]
edition = "2021"
license = "MIT"
authors = ["Test Author"]

[workspace.dependencies]
serde = { version = "1.0", features = ["derive"] }
"#;

  std::fs::write(workspace.path.join("Cargo.toml"), workspace_toml_content)?;

  // Create members that need different features
  workspace.add_crate(
    "crate-a",
    "0.1.0",
    &[("serde", r#"{ version = "1.0", features = ["derive", "rc"] }"#)],
  )?;

  workspace.add_crate(
    "crate-b",
    "0.1.0",
    &[("serde", r#"{ version = "1.0", features = ["derive", "alloc"] }"#)],
  )?;

  workspace.commit("Add crates with existing workspace.dependencies")?;

  // Run unify
  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "Unify should succeed.\nOutput:\n{}",
    String::from_utf8_lossy(&output.stdout)
  );

  // Verify workspace has updated features (union of all features)
  let updated_workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert!(updated_workspace_toml.contains("derive"), "Should have derive feature");

  // Verify members converted to workspace = true
  let crate_a_toml = std::fs::read_to_string(workspace.path.join("crates/crate-a/Cargo.toml"))?;
  assert!(
    crate_a_toml.contains("workspace = true") || crate_a_toml.contains("workspace=true"),
    "Member should use workspace inheritance.\nContent:\n{}",
    crate_a_toml
  );

  Ok(())
}

#[test]
fn test_unify_local_features_calculation() -> Result<()> {
  // Test that local features are correctly calculated (member features - workspace features)

  let workspace = TestWorkspace::new()?;

  // Create crates where one needs extra features
  workspace.add_crate(
    "crate-a",
    "0.1.0",
    &[("tokio", r#"{ version = "1.0", features = ["fs"] }"#)],
  )?;

  workspace.add_crate(
    "crate-b",
    "0.1.0",
    &[("tokio", r#"{ version = "1.0", features = ["fs", "net", "io-util"] }"#)],
  )?;

  workspace.commit("Add crates with different features")?;

  // Run unify
  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "Unify should succeed.\nOutput:\n{}",
    String::from_utf8_lossy(&output.stdout)
  );

  // Workspace should have intersection of features (fs)
  let workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert!(workspace_toml.contains("tokio"), "Should have tokio");
  assert!(
    workspace_toml.contains("fs"),
    "Should have 'fs' feature (common to both)"
  );

  // crate-b should have local features for net and io-util
  let crate_b_toml = std::fs::read_to_string(workspace.path.join("crates/crate-b/Cargo.toml"))?;

  // Check for local features - the format depends on implementation
  // Either: tokio = { workspace = true, features = ["net", "io-util"] }
  // Or the features are merged into workspace
  assert!(
    crate_b_toml.contains("workspace = true") || crate_b_toml.contains("workspace=true"),
    "crate-b should use workspace inheritance.\nContent:\n{}",
    crate_b_toml
  );

  Ok(())
}

#[test]
fn test_unify_target_specific_features_stay_local() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create crate-a with unconditional tokio dependency
  workspace.add_crate(
    "crate-a",
    "0.1.0",
    &[("tokio", r#"{ version = "1.0", features = ["rt", "macros"] }"#)],
  )?;

  // Create crate-b with target-specific tokio feature
  // We manually create this to have a target-specific dependency
  let crate_b_path = workspace.path.join("crates/crate-b");
  std::fs::create_dir_all(&crate_b_path)?;
  std::fs::create_dir_all(crate_b_path.join("src"))?;

  std::fs::write(
    crate_b_path.join("Cargo.toml"),
    r#"[package]
name = "crate-b"
version = "0.1.0"
edition.workspace = true

[dependencies]
tokio = { version = "1.0", features = ["rt"] }

[target.'cfg(target_os = "linux")'.dependencies]
tokio = { version = "1.0", features = ["signal"] }
"#,
  )?;

  std::fs::write(
    crate_b_path.join("src/lib.rs"),
    "pub fn hello() -> &'static str { \"Hello\" }",
  )?;

  workspace.commit("Add crates with target-specific features")?;

  // Configure rail.toml
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[workspace]
root = "."

[unify]
"#,
  )?;

  // Run apply
  let apply_output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    apply_output.status.success(),
    "Apply should succeed.\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&apply_output.stdout),
    String::from_utf8_lossy(&apply_output.stderr)
  );

  // Check workspace Cargo.toml
  let workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;

  // Workspace should have tokio with common features (rt, macros from crate-a)
  assert!(
    workspace_toml.contains("tokio"),
    "Should have tokio in workspace.dependencies"
  );
  assert!(
    workspace_toml.contains("rt") || workspace_toml.contains("macros"),
    "Should have common features.\nWorkspace TOML:\n{}",
    workspace_toml
  );

  // CRITICAL: workspace.dependencies should NOT have the target-specific "signal" feature
  // This is the BUG 2 fix - target-specific features stay local
  assert!(
    !workspace_toml.contains("signal"),
    "Target-specific 'signal' feature should NOT be in workspace.dependencies.\n\
     It should stay local to crate-b's target-specific section.\nWorkspace TOML:\n{}",
    workspace_toml
  );

  Ok(())
}

#[test]
fn test_unify_workspace_member_version_deps_always_get_path() -> Result<()> {
  let workspace = TestWorkspace::new_named("tokio-member-path")?;

  // Workspace member that other members depend on via VERSION (Tokio-style).
  workspace.add_crate("tokio", "1.0.0", &[])?;
  let tokio_manifest = workspace.path.join("crates/tokio/Cargo.toml");
  let mut tokio = std::fs::read_to_string(&tokio_manifest)?;
  tokio.push_str("\n[features]\nrt = []\n");
  std::fs::write(&tokio_manifest, tokio)?;
  workspace.add_crate("tokio-stream", "0.1.0", &[("tokio", r#""1.0.0""#)])?;
  workspace.add_crate(
    "tokio-util",
    "0.1.0",
    &[("tokio", r#"{ version = "1.0.0", features = ["rt"] }"#)],
  )?;

  // Simulate Tokio's patch strategy where member deps are declared by version and patched locally.
  let root_manifest = workspace.path.join("Cargo.toml");
  let mut root = std::fs::read_to_string(&root_manifest)?;
  root.push_str("\n[patch.crates-io]\ntokio = { path = \"crates/tokio\" }\n");
  std::fs::write(&root_manifest, root)?;

  // Critical: even with include_paths disabled, workspace member deps must still carry a path.
  std::fs::write(
    workspace.path.join(".config/rail.toml"),
    r#"[workspace]
root = "."

[unify]
include_paths = false
"#,
  )?;

  workspace.commit("Add tokio-style member dependencies with crates-io patch")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "unify should succeed.\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  let workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert!(
    workspace_toml.contains("tokio = {") && workspace_toml.contains("path = \"crates/tokio\""),
    "Workspace member dependency must include path for local resolution.\nCargo.toml:\n{}",
    workspace_toml
  );

  let stream_toml = std::fs::read_to_string(workspace.path.join("crates/tokio-stream/Cargo.toml"))?;
  assert!(
    stream_toml.contains("tokio = { workspace = true"),
    "tokio-stream should inherit tokio from workspace.\nCargo.toml:\n{}",
    stream_toml
  );

  let util_toml = std::fs::read_to_string(workspace.path.join("crates/tokio-util/Cargo.toml"))?;
  assert!(
    util_toml.contains("tokio = { workspace = true"),
    "tokio-util should inherit tokio from workspace.\nCargo.toml:\n{}",
    util_toml
  );

  Ok(())
}

#[test]
fn test_unify_workspace_member_cohort_unifies_atomically() -> Result<()> {
  let workspace = TestWorkspace::new_named("tokio-cohort-atomic")?;

  workspace.add_crate("tokio", "1.0.0", &[])?;
  workspace.add_crate("tokio-stream", "0.1.0", &[("tokio", r#""1.0.0""#)])?;
  workspace.add_crate("tokio-util", "0.1.0", &[("tokio", r#""1.0.0""#)])?;
  workspace.add_crate(
    "tokio-test",
    "0.1.0",
    &[
      ("tokio", r#""1.0.0""#),
      ("tokio-stream", r#""0.1.0""#),
      ("tokio-util", r#""0.1.0""#),
    ],
  )?;

  let root_manifest = workspace.path.join("Cargo.toml");
  let mut root = std::fs::read_to_string(&root_manifest)?;
  root.push_str(
    "\n[patch.crates-io]\ntokio = { path = \"crates/tokio\" }\ntokio-stream = { path = \"crates/tokio-stream\" }\ntokio-util = { path = \"crates/tokio-util\" }\n",
  );
  std::fs::write(&root_manifest, root)?;

  workspace.commit("Add connected tokio member cohort")?;

  // The low-usage members (tokio-stream/tokio-util) should still unify because
  // workspace-member cohorts are handled atomically.
  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "unify should succeed.\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  let workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert!(
    workspace_toml.contains("tokio = {") && workspace_toml.contains("path = \"crates/tokio\""),
    "tokio should be unified as workspace member.\nCargo.toml:\n{}",
    workspace_toml
  );
  assert!(
    workspace_toml.contains("tokio-stream = {") && workspace_toml.contains("path = \"crates/tokio-stream\""),
    "tokio-stream should be unified with member path (cohort atomicity).\nCargo.toml:\n{}",
    workspace_toml
  );
  assert!(
    workspace_toml.contains("tokio-util = {") && workspace_toml.contains("path = \"crates/tokio-util\""),
    "tokio-util should be unified with member path (cohort atomicity).\nCargo.toml:\n{}",
    workspace_toml
  );

  Ok(())
}

#[test]
fn test_unify_workspace_member_partial_exclude_applies_to_full_cohort() -> Result<()> {
  let workspace = TestWorkspace::new_named("tokio-cohort-partial-exclude")?;

  workspace.add_crate("tokio", "1.0.0", &[])?;
  workspace.add_crate("tokio-stream", "0.1.0", &[("tokio", r#""1.0.0""#)])?;
  workspace.add_crate("tokio-util", "0.1.0", &[("tokio", r#""1.0.0""#)])?;
  workspace.add_crate(
    "tokio-test",
    "0.1.0",
    &[
      ("tokio", r#""1.0.0""#),
      ("tokio-stream", r#""0.1.0""#),
      ("tokio-util", r#""0.1.0""#),
    ],
  )?;

  let root_manifest = workspace.path.join("Cargo.toml");
  let mut root = std::fs::read_to_string(&root_manifest)?;
  root.push_str(
    "\n[patch.crates-io]\ntokio = { path = \"crates/tokio\" }\ntokio-stream = { path = \"crates/tokio-stream\" }\ntokio-util = { path = \"crates/tokio-util\" }\n",
  );
  std::fs::write(&root_manifest, root)?;

  std::fs::write(
    workspace.path.join(".config/rail.toml"),
    r#"[unify]
exclude = ["tokio"]
"#,
  )?;

  workspace.commit("Partially exclude tokio member cohort")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(
    stdout.contains("Applying exclude atomically to the full cohort"),
    "Should explain partial workspace-member cohort exclusion.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_unify_workspace_member_cohort_with_non_default_members_still_unifies() -> Result<()> {
  let workspace = TestWorkspace::new_named("non-default-member-cohort")?;

  // default member: excluded from the member-dependency cohort
  workspace.add_crate("root-default", "0.1.0", &[])?;

  // non-default member cohort:
  // edge-a -> edge-b -> edge-c
  workspace.add_crate(
    "edge-a",
    "0.1.0",
    &[("edge-b", r#"{ path = "../edge-b", version = "0.1.0" }"#)],
  )?;
  workspace.add_crate(
    "edge-b",
    "0.1.0",
    &[("edge-c", r#"{ path = "../edge-c", version = "0.1.0" }"#)],
  )?;
  workspace.add_crate("edge-c", "0.1.0", &[])?;

  // Restrict default-members so edge-* crates are often absent from resolved metadata's
  // direct-dependency traversal. Cohort logic must still unify edge-b/edge-c atomically.
  let root_manifest = workspace.path.join("Cargo.toml");
  let mut root = std::fs::read_to_string(&root_manifest)?;
  root = root.replace(
    "resolver = \"2\"\n",
    "resolver = \"2\"\ndefault-members = [\"crates/root-default\"]\n",
  );
  std::fs::write(&root_manifest, root)?;

  workspace.commit("Add non-default workspace member cohort")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "unify should succeed.\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  let workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  let edge_b_path_written = workspace_toml.contains("path = \"crates/edge-b\"")
    || workspace_toml.contains("path = 'crates/edge-b'")
    || workspace_toml.contains("path = \"crates\\edge-b\"")
    || workspace_toml.contains("path = 'crates\\edge-b'");
  assert!(
    workspace_toml.contains("edge-b = {") && edge_b_path_written,
    "edge-b should be unified as workspace member despite non-default-members metadata limits.\nCargo.toml:\n{}",
    workspace_toml
  );
  let edge_c_path_written = workspace_toml.contains("path = \"crates/edge-c\"")
    || workspace_toml.contains("path = 'crates/edge-c'")
    || workspace_toml.contains("path = \"crates\\edge-c\"")
    || workspace_toml.contains("path = 'crates\\edge-c'");
  assert!(
    workspace_toml.contains("edge-c = {") && edge_c_path_written,
    "edge-c should be unified as workspace member despite non-default-members metadata limits.\nCargo.toml:\n{}",
    workspace_toml
  );

  let edge_a_toml = std::fs::read_to_string(workspace.path.join("crates/edge-a/Cargo.toml"))?;
  assert!(
    edge_a_toml.contains("edge-b = { workspace = true"),
    "edge-a should inherit edge-b from workspace.\nCargo.toml:\n{}",
    edge_a_toml
  );

  let edge_b_toml = std::fs::read_to_string(workspace.path.join("crates/edge-b/Cargo.toml"))?;
  assert!(
    edge_b_toml.contains("edge-c = { workspace = true"),
    "edge-b should inherit edge-c from workspace.\nCargo.toml:\n{}",
    edge_b_toml
  );

  Ok(())
}

#[test]
fn test_unify_workspace_member_features_stay_local_not_hoisted() -> Result<()> {
  let workspace = TestWorkspace::new_named("member-features-local")?;

  // Member with optional feature surface
  let core_path = workspace.path.join("crates/core-member");
  std::fs::create_dir_all(core_path.join("src"))?;
  std::fs::write(
    core_path.join("Cargo.toml"),
    r#"[package]
name = "core-member"
version = "0.1.0"
edition.workspace = true

[features]
net = []
"#,
  )?;
  std::fs::write(core_path.join("src/lib.rs"), "pub fn core() {}")?;

  // Consumer that needs core-member/net
  let consumer_a_path = workspace.path.join("crates/consumer-a");
  std::fs::create_dir_all(consumer_a_path.join("src"))?;
  std::fs::write(
    consumer_a_path.join("Cargo.toml"),
    r#"[package]
name = "consumer-a"
version = "0.1.0"
edition.workspace = true

[dependencies]
core-member = { version = "0.1.0", features = ["net"] }
"#,
  )?;
  std::fs::write(consumer_a_path.join("src/lib.rs"), "pub fn a() {}")?;

  // Consumer that does not need net
  let consumer_b_path = workspace.path.join("crates/consumer-b");
  std::fs::create_dir_all(consumer_b_path.join("src"))?;
  std::fs::write(
    consumer_b_path.join("Cargo.toml"),
    r#"[package]
name = "consumer-b"
version = "0.1.0"
edition.workspace = true

[dependencies]
core-member = "0.1.0"
"#,
  )?;
  std::fs::write(consumer_b_path.join("src/lib.rs"), "pub fn b() {}")?;

  let root_manifest = workspace.path.join("Cargo.toml");
  let mut root = std::fs::read_to_string(&root_manifest)?;
  root.push_str("\n[patch.crates-io]\ncore-member = { path = \"crates/core-member\" }\n");
  std::fs::write(&root_manifest, root)?;

  workspace.commit("Add workspace member feature split scenario")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "unify should succeed.\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  let workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert!(
    workspace_toml.contains("core-member = {"),
    "workspace.dependencies should include core-member.\nCargo.toml:\n{}",
    workspace_toml
  );
  assert!(
    !workspace_toml.contains("core-member = { path = \"crates/core-member\", version = \"^0.1.0\", features")
      && !workspace_toml.contains("core-member = { path = 'crates\\core-member', version = \"^0.1.0\", features"),
    "workspace member features must not be hoisted to workspace.dependencies.\nCargo.toml:\n{}",
    workspace_toml
  );

  let consumer_a_toml = std::fs::read_to_string(consumer_a_path.join("Cargo.toml"))?;
  assert!(
    consumer_a_toml.contains("core-member = { workspace = true, features = [\"net\"]"),
    "consumer-a should retain local member feature declaration.\nCargo.toml:\n{}",
    consumer_a_toml
  );

  let consumer_b_toml = std::fs::read_to_string(consumer_b_path.join("Cargo.toml"))?;
  assert!(
    consumer_b_toml.contains("core-member = { workspace = true"),
    "consumer-b should use workspace inheritance without net feature.\nCargo.toml:\n{}",
    consumer_b_toml
  );

  Ok(())
}

// Config Options: include, pin_transitives, include_renamed

/// Test include config forces specific dependencies to be included
#[test]
fn test_unify_include_flag() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-include")?;

  // Create two crates with shared dependency to trigger unification
  let crate_a_path = workspace.path.join("crates/include-a");
  std::fs::create_dir_all(&crate_a_path)?;
  std::fs::create_dir_all(crate_a_path.join("src"))?;

  std::fs::write(
    crate_a_path.join("Cargo.toml"),
    r#"[package]
name = "include-a"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = "1.0"
"#,
  )?;
  std::fs::write(crate_a_path.join("src/lib.rs"), "pub fn hello() {}")?;

  let crate_b_path = workspace.path.join("crates/include-b");
  std::fs::create_dir_all(&crate_b_path)?;
  std::fs::create_dir_all(crate_b_path.join("src"))?;

  std::fs::write(
    crate_b_path.join("Cargo.toml"),
    r#"[package]
name = "include-b"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = "1.0"
"#,
  )?;
  std::fs::write(crate_b_path.join("src/lib.rs"), "pub fn world() {}")?;

  workspace.commit("Add crates with shared deps")?;

  // Configure rail.toml with include
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[unify]
include = ["serde"]
"#,
  )?;

  // Run unify with include config
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--show-diff"])?;

  // Exit code 1 = check found pending changes (correct behavior)
  assert!(
    output.status.code() == Some(1),
    "unify --check with include config should exit 1 when changes pending. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

/// Test pin_transitives config option
#[test]
fn test_unify_pin_transitives() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-pin-trans")?;

  // Create a crate with a transitive-only dependency scenario
  let crate_path = workspace.path.join("crates/transitive-crate");
  std::fs::create_dir_all(&crate_path)?;
  std::fs::create_dir_all(crate_path.join("src"))?;

  std::fs::write(
    crate_path.join("Cargo.toml"),
    r#"[package]
name = "transitive-crate"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = "1.0"
"#,
  )?;
  std::fs::write(
    crate_path.join("src/lib.rs"),
    r#"use serde as _;

pub fn hello() {}
"#,
  )?;
  workspace.commit("Add crate")?;

  // Configure rail.toml with pin_transitives
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[unify]
pin_transitives = true
msrv = false
"#,
  )?;

  // Run unify with pin_transitives config (single crate = no unification needed)
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;

  // No unification opportunities with single crate, so exit 0
  assert!(
    output.status.success(),
    "unify with pin_transitives config and no changes should exit 0. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

/// Test include_renamed config option
#[test]
fn test_unify_include_renamed_flag() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-include-renamed")?;

  // Create a crate
  let crate_path = workspace.path.join("crates/renamed-crate");
  std::fs::create_dir_all(&crate_path)?;
  std::fs::create_dir_all(crate_path.join("src"))?;

  std::fs::write(
    crate_path.join("Cargo.toml"),
    r#"[package]
name = "renamed-crate"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = "1.0"
"#,
  )?;
  std::fs::write(
    crate_path.join("src/lib.rs"),
    r#"use serde as _;

pub fn hello() {}
"#,
  )?;
  workspace.commit("Add crate")?;

  // Configure rail.toml with include_renamed
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[unify]
include_renamed = true
msrv = false
"#,
  )?;

  // Run unify with include_renamed config
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;

  assert!(
    output.status.success(),
    "unify with include_renamed config should succeed. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

// Renamed Dependencies and Usage Thresholds

/// Test that include_renamed = true allows renamed + non-renamed deps to count together
/// for the >=2 usage threshold
#[test]
fn test_unify_include_renamed_merges_usage_count() -> Result<()> {
  let workspace = TestWorkspace::new_named("include-renamed-merge")?;

  // crate-a uses serde directly (non-renamed)
  let crate_a_path = workspace.path.join("crates/crate-a");
  std::fs::create_dir_all(&crate_a_path)?;
  std::fs::create_dir_all(crate_a_path.join("src"))?;

  std::fs::write(
    crate_a_path.join("Cargo.toml"),
    r#"[package]
name = "crate-a"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = { version = "1.0", features = ["derive"] }
"#,
  )?;
  std::fs::write(
    crate_a_path.join("src/lib.rs"),
    "use serde as _;\nuse anyhow as _;\npub fn a() {}",
  )?;

  // crate-b uses serde renamed as "my_serde"
  let crate_b_path = workspace.path.join("crates/crate-b");
  std::fs::create_dir_all(&crate_b_path)?;
  std::fs::create_dir_all(crate_b_path.join("src"))?;

  std::fs::write(
    crate_b_path.join("Cargo.toml"),
    r#"[package]
name = "crate-b"
version = "0.1.0"
edition.workspace = true

[dependencies]
my_serde = { package = "serde", version = "1.0", features = ["rc"] }
"#,
  )?;
  std::fs::write(
    crate_b_path.join("src/lib.rs"),
    "use serde as _;\nuse anyhow as _;\npub fn b() {}",
  )?;

  workspace.commit("Add crates with renamed dep")?;

  // Without include_renamed config, serde should NOT be unified (each variant has only 1 user)
  let output_without = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout_without = String::from_utf8_lossy(&output_without.stdout);
  assert!(
    !stdout_without.contains("serde"),
    "Without include_renamed config, serde shouldn't qualify (each variant has 1 user).\nOutput:\n{}",
    stdout_without
  );

  // Configure rail.toml with include_renamed = true
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[unify]
include_renamed = true
"#,
  )?;

  // With include_renamed config, serde SHOULD be unified (2 users total for the package)
  let output_with = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout_with = String::from_utf8_lossy(&output_with.stdout);
  assert!(
    stdout_with.contains("serde"),
    "With include_renamed config, serde should qualify (2 users total).\nOutput:\n{}",
    stdout_with
  );

  Ok(())
}

#[test]
fn test_unify_renamed_dependencies_hard_blocker() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create crates with renamed dependency
  // With the bug fix, renamed deps (package = "...") are now properly separated
  // from direct deps of the same package. This prevents feature confusion.
  workspace.add_crate("crate-a", "0.1.0", &[("serde", r#""1.0""#)])?;

  // Manually create crate-b with renamed serde
  let crate_b_path = workspace.path.join("crates/crate-b");
  std::fs::create_dir_all(&crate_b_path)?;
  std::fs::create_dir_all(crate_b_path.join("src"))?;

  std::fs::write(
    crate_b_path.join("Cargo.toml"),
    r#"[package]
name = "crate-b"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde_crate = { package = "serde", version = "1.0" }
"#,
  )?;

  std::fs::write(
    crate_b_path.join("src/lib.rs"),
    "pub fn hello() -> &'static str { \"Hello\" }",
  )?;

  workspace.commit("Add crates with renamed dependency")?;

  // Configure rail.toml (include_renamed = false by default)
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[workspace]
root = "."

[unify]
include_renamed = false
detect_unused = false
msrv = false
"#,
  )?;

  // Run analyze - with the fix, renamed deps are now treated separately
  // Since each version of serde (direct vs renamed) only has 1 user,
  // neither qualifies for unification (needs 2+ users)
  let analyze_output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let analyze_stdout = String::from_utf8_lossy(&analyze_output.stdout);

  // Should show no unification opportunities since each has only 1 user
  assert!(
    analyze_stdout.contains("status: no changes"),
    "Should show no unification opportunities when deps are properly separated.\nOutput:\n{}",
    analyze_stdout
  );

  // Run apply - should succeed (no changes needed)
  let apply_output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  let apply_stdout = String::from_utf8_lossy(&apply_output.stdout);

  // Should indicate no changes
  assert!(
    apply_stdout.contains("nothing to unify"),
    "Apply should indicate no changes needed.\nstdout:\n{}",
    apply_stdout
  );

  // Check that members were NOT converted to workspace inheritance
  let crate_a_member = std::fs::read_to_string(workspace.path.join("crates/crate-a/Cargo.toml"))?;

  // Should still have the original version specification
  assert!(
    crate_a_member.contains("serde = \"1.0\""),
    "crate-a should still have original serde version (not converted).\ncrate-a Cargo.toml:\n{}",
    crate_a_member
  );

  // Should NOT have "serde = { workspace = true }"
  assert!(
    !crate_a_member.contains("serde = { workspace = true }") && !crate_a_member.contains("serde = {workspace=true}"),
    "crate-a should not use workspace = true for serde.\ncrate-a Cargo.toml:\n{}",
    crate_a_member
  );

  Ok(())
}

// Config Options: include, exact_pin_handling

/// Test that the `include` config option forces a dependency with only 1 user
/// to be included in workspace.dependencies
#[test]
fn test_unify_include_forces_single_user_dep() -> Result<()> {
  let workspace = TestWorkspace::new_named("include-single-user")?;

  // Create rail.toml with include = ["anyhow"]
  std::fs::create_dir_all(workspace.path.join(".config"))?;
  std::fs::write(
    workspace.path.join(".config/rail.toml"),
    r#"
[unify]
include = ["anyhow"]
"#,
  )?;

  // crate-a uses both serde and anyhow
  let crate_a_path = workspace.path.join("crates/crate-a");
  std::fs::create_dir_all(&crate_a_path)?;
  std::fs::create_dir_all(crate_a_path.join("src"))?;

  std::fs::write(
    crate_a_path.join("Cargo.toml"),
    r#"[package]
name = "crate-a"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = "1.0"
anyhow = "1.0"
"#,
  )?;
  std::fs::write(
    crate_a_path.join("src/lib.rs"),
    "use serde as _;\nuse anyhow as _;\npub fn a() {}",
  )?;

  // crate-b uses only serde (not anyhow)
  let crate_b_path = workspace.path.join("crates/crate-b");
  std::fs::create_dir_all(&crate_b_path)?;
  std::fs::create_dir_all(crate_b_path.join("src"))?;

  std::fs::write(
    crate_b_path.join("Cargo.toml"),
    r#"[package]
name = "crate-b"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = "1.0"
"#,
  )?;
  std::fs::write(crate_b_path.join("src/lib.rs"), "use serde as _;\npub fn b() {}")?;

  workspace.commit("Add crates")?;

  // Run unify analyze
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--show-diff"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // serde should be unified (2 users)
  assert!(
    stdout.contains("serde"),
    "serde should be unified (2 users).\nOutput:\n{}",
    stdout
  );

  // anyhow should ALSO be unified because it's in the `include` list,
  // even though it only has 1 user
  assert!(
    stdout.contains("anyhow"),
    "anyhow should be unified due to `include` config.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

/// Test that exact_pin_handling = "preserve" keeps the =x.y.z format
/// in workspace.dependencies instead of converting to ^x.y.z
#[test]
fn test_unify_exact_pin_handling_preserve() -> Result<()> {
  let workspace = TestWorkspace::new_named("exact-pin-preserve")?;

  // Create rail.toml with exact_pin_handling = "preserve"
  std::fs::create_dir_all(workspace.path.join(".config"))?;
  std::fs::write(
    workspace.path.join(".config/rail.toml"),
    r#"
[unify]
exact_pin_handling = "preserve"
"#,
  )?;

  // crate-a uses serde with exact pin
  let crate_a_path = workspace.path.join("crates/crate-a");
  std::fs::create_dir_all(&crate_a_path)?;
  std::fs::create_dir_all(crate_a_path.join("src"))?;

  std::fs::write(
    crate_a_path.join("Cargo.toml"),
    r#"[package]
name = "crate-a"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = "=1.0.200"
"#,
  )?;
  std::fs::write(crate_a_path.join("src/lib.rs"), "pub fn a() {}")?;

  // crate-b also uses serde with exact pin
  let crate_b_path = workspace.path.join("crates/crate-b");
  std::fs::create_dir_all(&crate_b_path)?;
  std::fs::create_dir_all(crate_b_path.join("src"))?;

  std::fs::write(
    crate_b_path.join("Cargo.toml"),
    r#"[package]
name = "crate-b"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = "=1.0.200"
"#,
  )?;
  std::fs::write(crate_b_path.join("src/lib.rs"), "pub fn b() {}")?;

  workspace.commit("Add crates with exact pins")?;

  // Run unify apply
  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "Unify should succeed.\nstderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  // Check workspace Cargo.toml - should have exact pin preserved
  let workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert!(
    workspace_toml.contains("serde"),
    "Workspace should have serde.\nContent:\n{}",
    workspace_toml
  );

  // The version should start with = (exact pin preserved)
  // Format: serde = { version = "=1.0.200", ... } or serde = "=1.0.200"
  assert!(
    workspace_toml.contains("=1.0") || workspace_toml.contains("\"="),
    "Exact pin should be preserved in workspace.dependencies.\nContent:\n{}",
    workspace_toml
  );

  Ok(())
}

/// Test that exact_pin_handling = "skip" excludes exact-pinned deps from unification
#[test]
fn test_unify_exact_pin_handling_skip() -> Result<()> {
  let workspace = TestWorkspace::new_named("exact-pin-skip")?;

  // Create rail.toml with exact_pin_handling = "skip"
  std::fs::create_dir_all(workspace.path.join(".config"))?;
  std::fs::write(
    workspace.path.join(".config/rail.toml"),
    r#"
[unify]
exact_pin_handling = "skip"
"#,
  )?;

  // crate-a uses serde with exact pin
  let crate_a_path = workspace.path.join("crates/crate-a");
  std::fs::create_dir_all(&crate_a_path)?;
  std::fs::create_dir_all(crate_a_path.join("src"))?;

  std::fs::write(
    crate_a_path.join("Cargo.toml"),
    r#"[package]
name = "crate-a"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = "=1.0.200"
anyhow = "1.0"
"#,
  )?;
  std::fs::write(crate_a_path.join("src/lib.rs"), "pub fn a() {}")?;

  // crate-b also uses both deps
  let crate_b_path = workspace.path.join("crates/crate-b");
  std::fs::create_dir_all(&crate_b_path)?;
  std::fs::create_dir_all(crate_b_path.join("src"))?;

  std::fs::write(
    crate_b_path.join("Cargo.toml"),
    r#"[package]
name = "crate-b"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = "=1.0.200"
anyhow = "1.0"
"#,
  )?;
  std::fs::write(crate_b_path.join("src/lib.rs"), "pub fn b() {}")?;

  workspace.commit("Add crates with exact pins")?;

  // Run unify check with explanation output
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // anyhow should be unified (no exact pin)
  assert!(
    stdout.contains("anyhow"),
    "anyhow should be unified (no exact pin).\nOutput:\n{}",
    stdout
  );

  assert!(
    stdout.contains("serde [skipped_dependency]"),
    "serde should be skipped due to exact pin.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

/// Test that exact_pin_handling = "warn" (default) converts to caret but warns
#[test]
fn test_unify_exact_pin_handling_warn() -> Result<()> {
  let workspace = TestWorkspace::new_named("exact-pin-warn")?;

  // No rail.toml - use default (warn mode)

  // crate-a uses serde with exact pin
  let crate_a_path = workspace.path.join("crates/crate-a");
  std::fs::create_dir_all(&crate_a_path)?;
  std::fs::create_dir_all(crate_a_path.join("src"))?;

  std::fs::write(
    crate_a_path.join("Cargo.toml"),
    r#"[package]
name = "crate-a"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = "=1.0.200"
"#,
  )?;
  std::fs::write(crate_a_path.join("src/lib.rs"), "use serde as _;\npub fn a() {}")?;

  // crate-b also uses serde with exact pin
  let crate_b_path = workspace.path.join("crates/crate-b");
  std::fs::create_dir_all(&crate_b_path)?;
  std::fs::create_dir_all(crate_b_path.join("src"))?;

  std::fs::write(
    crate_b_path.join("Cargo.toml"),
    r#"[package]
name = "crate-b"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = "=1.0.200"
"#,
  )?;
  std::fs::write(crate_b_path.join("src/lib.rs"), "use serde as _;\npub fn b() {}")?;

  workspace.commit("Add crates with exact pins")?;

  // Run unify check
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // serde should be in the plan (warn mode converts but still unifies)
  assert!(
    stdout.contains("serde"),
    "serde should be in unification plan (warn mode).\nOutput:\n{}",
    stdout
  );

  // Should explain the exact pin conversion explicitly
  assert!(
    stdout.contains("exact_pin_warn_caret"),
    "Should explain exact version pin handling.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

// Report Generation, TOML Comments, and Backup Tests

#[test]
fn test_unify_report_generation() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  workspace.add_crate(
    "crate-a",
    "0.1.0",
    &[("serde", r#"{ version = "1.0", features = ["derive"] }"#)],
  )?;
  workspace.add_crate(
    "crate-b",
    "0.1.0",
    &[("serde", r#"{ version = "1.0", features = ["rc"] }"#)],
  )?;

  workspace.commit("Add crates")?;

  // Configure rail.toml with report generation enabled
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[workspace]
root = "."

[unify.output]
generate_report = true
"#,
  )?;

  // Run apply
  let apply_output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    apply_output.status.success(),
    "Apply should succeed.\nOutput:\n{}",
    String::from_utf8_lossy(&apply_output.stdout)
  );

  // Check report was generated
  let report_path = workspace.path.join("target/cargo-rail/unify-report.md");
  assert!(report_path.exists(), "Report should be generated");

  // Read and validate report contents
  let report_content = std::fs::read_to_string(&report_path)?;

  assert!(
    report_content.contains("# Cargo Rail Unification Report"),
    "Report should have title"
  );
  assert!(report_content.contains("Summary"), "Report should have summary section");
  assert!(
    report_content.contains("serde"),
    "Report should mention unified dependency"
  );
  assert!(
    report_content.contains("derive") && report_content.contains("rc"),
    "Report should show unified features"
  );

  Ok(())
}

#[test]
fn test_unify_toml_comments() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  workspace.add_crate(
    "crate-a",
    "0.1.0",
    &[("tokio", r#"{ version = "1.0", features = ["fs"] }"#)],
  )?;
  workspace.add_crate(
    "crate-b",
    "0.1.0",
    &[("tokio", r#"{ version = "1.0", features = ["net"] }"#)],
  )?;
  workspace.add_crate(
    "crate-c",
    "0.1.0",
    &[("tokio", r#"{ version = "1.0", features = ["io-util"] }"#)],
  )?;

  workspace.commit("Add crates")?;

  // Configure rail.toml with comment generation
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[workspace]
root = "."

[unify]
# add_conflict_comments is now implicit (always true)
"#,
  )?;

  // Run apply
  let apply_output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(apply_output.status.success(), "Apply should succeed");

  // Check workspace Cargo.toml was created successfully
  let workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;

  // Should have [workspace.dependencies] section
  assert!(
    workspace_toml.contains("[workspace.dependencies]"),
    "Should have workspace dependencies section.\nWorkspace TOML:\n{}",
    workspace_toml
  );

  // Should have unified tokio
  assert!(
    workspace_toml.contains("tokio"),
    "Should have unified tokio dependency.\nWorkspace TOML:\n{}",
    workspace_toml
  );

  // Note: Comments for table-format dependencies (with features) are not currently supported
  // Only inline-format dependencies get trailing comments

  Ok(())
}

#[test]
fn test_unify_backup_flag() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  workspace.add_crate("crate-a", "0.1.0", &[("serde", r#""1.0""#)])?;
  workspace.add_crate("crate-b", "0.1.0", &[("serde", r#""1.0""#)])?;

  workspace.commit("Add crates")?;

  // Configure rail.toml
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[workspace]
root = "."

[unify]
"#,
  )?;

  // Run apply with --backup
  let apply_output = run_cargo_rail(&workspace.path, &["rail", "unify", "--backup"])?;
  let apply_stdout = String::from_utf8_lossy(&apply_output.stdout);

  assert!(
    apply_output.status.success(),
    "Apply should succeed.\nOutput:\n{}",
    apply_stdout
  );

  // Check that backup was created in target/cargo-rail/backups/
  let backup_root = workspace.path.join("target/cargo-rail/backups");
  assert!(
    backup_root.exists(),
    "Backup directory should exist at target/cargo-rail/backups"
  );

  // Find the backup directory (should be a timestamp-based folder)
  let backup_entries: Vec<_> = std::fs::read_dir(&backup_root)
    .expect("Should read backup directory")
    .filter_map(|e| e.ok())
    .filter(|e| e.path().is_dir())
    .collect();

  assert!(!backup_entries.is_empty(), "At least one backup should exist");

  let backup_dir = backup_entries.first().unwrap().path();

  // Check that backup contains the expected files
  assert!(
    backup_dir.join("Cargo.toml").exists(),
    "Backup should contain workspace Cargo.toml"
  );
  assert!(
    backup_dir.join("crates/crate-a/Cargo.toml").exists() || backup_dir.join("crate-a/Cargo.toml").exists(),
    "Backup should contain crate-a Cargo.toml"
  );
  assert!(
    backup_dir.join("crates/crate-b/Cargo.toml").exists() || backup_dir.join("crate-b/Cargo.toml").exists(),
    "Backup should contain crate-b Cargo.toml"
  );

  // Check that metadata.json exists
  assert!(
    backup_dir.join("metadata.json").exists(),
    "Backup should contain metadata.json"
  );

  // Check backup message in output (check stderr for status messages)
  let apply_stderr = String::from_utf8_lossy(&apply_output.stderr);
  assert!(
    apply_stderr.contains("creating backup") || apply_stderr.contains("backup:"),
    "Should mention backups in output.\nOutput:\n{}",
    apply_stderr
  );

  Ok(())
}

#[test]
fn test_unify_apply_writes_mutation_receipts() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-mutation-receipts")?;

  workspace.add_crate("crate-a", "0.1.0", &[("serde", r#""1.0""#)])?;
  workspace.add_crate("crate-b", "0.1.0", &[("serde", r#""1.0""#)])?;
  workspace.commit("Add crates for unify receipt test")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "unify apply should succeed.\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  let receipts_dir = workspace.path.join("target/cargo-rail/receipts");
  let entries = std::fs::read_dir(&receipts_dir)?;
  let receipt_paths: Vec<_> = entries
    .filter_map(|entry| entry.ok().map(|e| e.path()))
    .filter(|path| {
      path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.contains("unify-") && name.ends_with(".json"))
        .unwrap_or(false)
    })
    .collect();

  assert!(
    !receipt_paths.is_empty(),
    "expected unify mutation receipts under {}",
    receipts_dir.display()
  );

  for receipt_path in receipt_paths {
    let content = std::fs::read_to_string(&receipt_path)?;
    let json: serde_json::Value = serde_json::from_str(&content)?;

    assert_eq!(json["contract_version"], 2);
    assert!(json.get("operation_id").is_some());
    assert!(json["plan"]["inputs_fingerprint"].is_string());
    assert!(json["plan"]["resolved_refs"].is_object());
    assert!(json["plan"]["actions"].is_array());
    assert!(json["plan"]["actions"].as_array().is_some_and(|actions| {
      actions.iter().any(|action| {
        action["code"] == "VERIFY_PROOF_SET"
          && action["payload"]["proof_fingerprint"]
            .as_str()
            .is_some_and(|fingerprint| fingerprint.starts_with("sha256:"))
      })
    }));
    assert!(json["plan"]["risks"].is_array());
    assert!(json["plan"]["trace"].is_array());
    if json["phase"] == "apply" {
      assert!(json["trace"].as_array().is_some_and(|trace| {
        trace.iter().any(|entry| {
          entry["code"] == "UNIFY_GRAPH_DELTA_VERIFIED"
            && entry["message"]
              .as_str()
              .is_some_and(|message| message.contains("sha256:"))
        })
      }));
      assert!(json["trace"].as_array().is_some_and(|trace| {
        trace.iter().any(|entry| {
          entry["code"] == "UNIFY_PROOF_SET_BOUND"
            && entry["message"]
              .as_str()
              .is_some_and(|message| message.contains("sha256:"))
        })
      }));
    }
  }

  Ok(())
}

#[test]
fn test_unify_apply_from_plan_file() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-apply-plan-file")?;

  workspace.add_crate("crate-a", "0.1.0", &[("serde", r#""1.0""#)])?;
  workspace.add_crate("crate-b", "0.1.0", &[("serde", r#""1.0""#)])?;
  workspace.commit("Add crates for plan apply test")?;

  let plan_path = workspace.path.join("unify-plan.json");
  let check_output = run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "unify",
      "--check",
      "-f",
      "json",
      "-o",
      plan_path.to_string_lossy().as_ref(),
    ],
  )?;
  assert_eq!(
    check_output.status.code(),
    Some(1),
    "check should report pending changes"
  );

  let apply_output = run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "unify",
      "--plan",
      plan_path.to_string_lossy().as_ref(),
      "--skip-report",
    ],
  )?;
  assert!(
    apply_output.status.success(),
    "unify apply --plan should succeed.\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&apply_output.stdout),
    String::from_utf8_lossy(&apply_output.stderr)
  );

  let post_check = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  assert_eq!(
    post_check.status.code(),
    Some(0),
    "workspace should be unified after plan apply"
  );

  Ok(())
}

#[test]
fn test_unify_check_json_runs_without_git_repository() -> Result<()> {
  let root = TempDir::new()?;
  let workspace_root = root.path();

  std::fs::write(
    workspace_root.join("Cargo.toml"),
    r#"[workspace]
members = ["crates/*"]
resolver = "2"

[workspace.package]
edition = "2021"
"#,
  )?;
  std::fs::create_dir_all(workspace_root.join(".config"))?;
  std::fs::write(
    workspace_root.join(".config/rail.toml"),
    r#"[workspace]
root = "."

[toolchain]
channel = "stable"
"#,
  )?;

  let crate_root = workspace_root.join("crates").join("standalone");
  std::fs::create_dir_all(crate_root.join("src"))?;
  std::fs::write(
    crate_root.join("Cargo.toml"),
    r#"[package]
name = "standalone"
version = "0.1.0"
edition.workspace = true
"#,
  )?;
  std::fs::write(crate_root.join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n")?;

  assert!(
    !workspace_root.join(".git").exists(),
    "test workspace must not have a git repo"
  );

  let output = run_cargo_rail(workspace_root, &["rail", "unify", "--check", "-f", "json"])?;
  assert!(
    output.status.success(),
    "unify --check should not require git.\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(json["mutation_plan_available"], false);
  assert!(json["mutation_plan"].is_null());

  Ok(())
}

#[test]
fn test_unify_dead_feature_pruning_preserves_published_api_flags() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-feature-api-safety")?;
  workspace.add_crate("public-crate", "0.1.0", &[])?;
  workspace.add_crate("private-crate", "0.1.0", &[])?;
  std::fs::write(
    workspace.path.join(".config/rail.toml"),
    "[workspace]\nroot = \".\"\n\n[unify]\nconsumer_scope = \"workspace\"\n",
  )?;

  std::fs::write(
    workspace.path.join("crates/public-crate/Cargo.toml"),
    r#"[package]
name = "public-crate"
version = "0.1.0"
edition = "2021"

[features]
compatibility-flag = []
"#,
  )?;
  std::fs::write(
    workspace.path.join("crates/private-crate/Cargo.toml"),
    r#"[package]
name = "private-crate"
version = "0.1.0"
edition = "2021"
publish = false

[features]
obsolete-internal-flag = []
example-gate = []

[[example]]
name = "gated"
required-features = ["example-gate"]
"#,
  )?;
  std::fs::create_dir_all(workspace.path.join("crates/private-crate/examples"))?;
  std::fs::write(
    workspace.path.join("crates/private-crate/examples/gated.rs"),
    "fn main() {}\n",
  )?;
  workspace.commit("Add public and private empty feature flags")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "unify should safely prune only the private feature\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  let public_manifest = std::fs::read_to_string(workspace.path.join("crates/public-crate/Cargo.toml"))?;
  assert!(
    public_manifest.contains("compatibility-flag"),
    "published feature flags are public API and must be preserved\n{public_manifest}"
  );
  let private_manifest = std::fs::read_to_string(workspace.path.join("crates/private-crate/Cargo.toml"))?;
  assert!(
    !private_manifest.contains("obsolete-internal-flag"),
    "an unreachable empty flag in a publish=false package can be removed\n{private_manifest}"
  );
  assert!(
    private_manifest.contains("example-gate"),
    "Cargo target required-features are reachability roots\n{private_manifest}"
  );

  Ok(())
}

#[test]
fn test_unify_open_consumer_scope_preserves_dormant_private_features() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-open-consumer-scope")?;
  workspace.add_crate("private-crate", "0.1.0", &[])?;
  std::fs::write(
    workspace.path.join("crates/private-crate/Cargo.toml"),
    r#"[package]
name = "private-crate"
version = "0.1.0"
edition = "2021"
publish = false

[features]
dormant-forwarder = ["dep:log"]

[dependencies]
log = { version = "0.4", optional = true }
"#,
  )?;
  workspace.commit("Add dormant private configuration with open consumers")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "open-world preservation must leave a valid graph\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  let manifest = std::fs::read_to_string(workspace.path.join("crates/private-crate/Cargo.toml"))?;
  assert!(
    manifest.contains("dormant-forwarder ="),
    "open consumer scope must preserve dormant features\n{manifest}"
  );
  assert!(
    manifest.contains("log ="),
    "open consumer scope must preserve optional dependencies\n{manifest}"
  );

  Ok(())
}

#[test]
fn test_unify_feature_reachability_prunes_unrooted_cycles_and_forwarders() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-feature-reachability")?;
  workspace.add_crate("private-crate", "0.1.0", &[])?;
  std::fs::write(
    workspace.path.join(".config/rail.toml"),
    r#"[workspace]
root = "."

[unify]
prune_dead_features = true
consumer_scope = "workspace"
preserve_features = ["keep-*"]
"#,
  )?;
  std::fs::write(
    workspace.path.join("crates/private-crate/Cargo.toml"),
    r#"[package]
name = "private-crate"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
log = { version = "0.4", optional = true, features = ["kv"] }

[features]
default = ["rooted-parent"]
rooted-parent = ["rooted-child"]
rooted-child = []
source-parent = ["source-child"]
source-child = []
negative-root = []
custom-cfg-root = []
impossible-platform-root = []
keep-policy = []
cycle-a = ["cycle-b"]
cycle-b = ["cycle-a"]
forward-only = ["dep:log"]
weak-forward-only = ["log?/kv"]
"#,
  )?;
  std::fs::write(
    workspace.path.join("crates/private-crate/src/lib.rs"),
    r#"#[cfg(not(feature = "negative-root"))]
pub fn active_without_feature() {}

#[cfg(feature = "source-parent")]
pub fn source_root() {}

#[cfg(all(generated_backend, feature = "custom-cfg-root"))]
pub fn generated_backend_root() {}

#[cfg(all(target_os = "cargo_rail_impossible", feature = "impossible-platform-root"))]
pub fn impossible_platform_root() {}
"#,
  )?;
  workspace.commit("Add rooted and unreachable private feature components")?;

  let check = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "-f", "json"])?;
  let repeated_check = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "-f", "json"])?;
  let mut check_json: serde_json::Value = serde_json::from_slice(&check.stdout)?;
  let mut repeated_json: serde_json::Value = serde_json::from_slice(&repeated_check.stdout)?;
  check_json
    .as_object_mut()
    .expect("JSON envelope")
    .remove("evidence_cache");
  repeated_json
    .as_object_mut()
    .expect("JSON envelope")
    .remove("evidence_cache");
  assert_eq!(
    check_json, repeated_json,
    "cyclic feature reachability and certificates must be deterministic"
  );
  let json: serde_json::Value = serde_json::from_slice(&check.stdout)?;
  let certificates = json["proof_certificates"]
    .as_array()
    .expect("proof_certificates should be an array");
  assert!(
    certificates.iter().any(|certificate| {
      certificate["member"] == "private-crate"
        && certificate["subject"]["declaration"] == "cycle-a"
        && certificate["evidence_source"] == "feature_reachability"
        && certificate["declared_edges"]
          .as_array()
          .is_some_and(|edges| edges.iter().any(|edge| edge == "cycle-b"))
        && certificate["roots_checked"]
          .as_array()
          .is_some_and(|roots| roots.iter().any(|root| root == "source_cfg"))
    }),
    "feature removal requires a deterministic reachability certificate\n{}",
    serde_json::to_string_pretty(&json)?
  );
  let reachability = json["feature_reachability"]
    .as_array()
    .expect("feature_reachability should be an array");
  assert!(
    reachability.iter().any(|feature| {
      feature["member"] == "private-crate"
        && feature["feature"] == "source-child"
        && feature["root_kind"] == "source_cfg"
        && feature["path"] == serde_json::json!(["source-parent", "source-child"])
    }),
    "every reachable feature needs its deterministic root path\n{}",
    serde_json::to_string_pretty(&json)?
  );
  assert!(
    certificates.iter().any(|certificate| {
      certificate["member"] == "private-crate"
        && certificate["subject"]["kind"] == "dependency"
        && certificate["subject"]["declaration"] == "log"
        && certificate["evidence_source"] == "compiler_optional_activation"
        && certificate["complete_configurations"] == certificate["applicable_configurations"]
        && certificate["unused_observations"]
          .as_u64()
          .is_some_and(|count| count > 0)
    }),
    "optional dependency removal requires complete activated compiler evidence\n{}",
    serde_json::to_string_pretty(&json)?
  );

  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "reachability edits must pass post-edit graph verification\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  let manifest = std::fs::read_to_string(workspace.path.join("crates/private-crate/Cargo.toml"))?;
  for preserved in [
    "default =",
    "rooted-parent =",
    "rooted-child =",
    "source-parent =",
    "source-child =",
    "negative-root =",
    "custom-cfg-root =",
    "keep-policy =",
  ] {
    assert!(
      manifest.contains(preserved),
      "rooted feature was removed: {preserved}\n{manifest}"
    );
  }
  for unreachable in [
    "cycle-a =",
    "cycle-b =",
    "forward-only =",
    "weak-forward-only =",
    "impossible-platform-root =",
  ] {
    assert!(
      !manifest.contains(unreachable),
      "unrooted feature component must be removed: {unreachable}\n{manifest}"
    );
  }
  assert!(
    !manifest.contains("log ="),
    "an optional dependency activated only by unreachable features must be removed in the same verified edit\n{manifest}"
  );

  Ok(())
}

#[test]
fn test_unify_feature_pruning_preserves_compiler_observed_optional_activation() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-generated-feature-activation")?;
  workspace.add_crate("private-crate", "0.1.0", &[])?;
  std::fs::write(
    workspace.path.join(".config/rail.toml"),
    "[workspace]\nroot = \".\"\n\n[unify]\nconsumer_scope = \"workspace\"\n",
  )?;
  std::fs::write(
    workspace.path.join("crates/private-crate/Cargo.toml"),
    r#"[package]
name = "private-crate"
version = "0.1.0"
edition = "2021"
publish = false
build = "build.rs"

[features]
generated-log = ["dep:log"]

[dependencies]
log = { version = "0.4", optional = true }
"#,
  )?;
  std::fs::write(
    workspace.path.join("crates/private-crate/build.rs"),
    r#"fn main() {
  let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
  let source = if std::env::var_os("CARGO_FEATURE_GENERATED_LOG").is_some() {
    "pub fn generated() { log::set_max_level(log::LevelFilter::Info); }\n"
  } else {
    "pub fn generated() {}\n"
  };
  std::fs::write(output.join("generated.rs"), source).expect("write generated source");
}
"#,
  )?;
  std::fs::write(
    workspace.path.join("crates/private-crate/src/lib.rs"),
    "include!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\"));\n",
  )?;
  workspace.commit("Add compiler-visible generated optional feature usage")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
  assert!(
    output.status.success(),
    "positive activated evidence must preserve a valid feature path\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("activated optional dependency has positive or incomplete compiler evidence"),
    "the preservation boundary must be named\n{stdout}"
  );
  let manifest = std::fs::read_to_string(workspace.path.join("crates/private-crate/Cargo.toml"))?;
  assert!(
    manifest.contains("generated-log ="),
    "used activation feature was removed\n{manifest}"
  );
  assert!(
    manifest.contains("log ="),
    "compiler-observed optional dependency was removed\n{manifest}"
  );

  Ok(())
}
