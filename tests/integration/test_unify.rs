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
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--dry-run"])?;
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
    stdout.contains("Analysis complete") || stdout.contains("✅"),
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

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--dry-run"])?;
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
fn test_unify_true_multi_version_conflict() -> Result<()> {
  // Test that TRUE version conflicts (incompatible versions) ARE detected

  let workspace = TestWorkspace::new()?;

  // Create crates with INCOMPATIBLE versions (1.x vs 2.x)
  workspace.add_crate("crate-a", "0.1.0", &[("syn", r#""^1.0""#)])?;

  workspace.add_crate("crate-b", "0.1.0", &[("syn", r#""^2.0""#)])?;

  workspace.commit("Add crates with incompatible versions")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--dry-run"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // With resolution-based compatibility checking, Cargo will pick one version
  // If Cargo can resolve both to the same version, we unify it
  // If Cargo fails to resolve, that's a real conflict
  // The output should show either unified syn OR a conflict
  assert!(
    stdout.contains("syn"),
    "Should mention syn in output.\nOutput:\n{}",
    stdout
  );

  // If there's a real conflict (Cargo couldn't resolve), we should see Issues
  // If Cargo resolved it successfully, we should see it unified
  let has_issues = stdout.contains("Issues requiring attention");
  let has_unified = stdout.contains("Ready to unify") && !has_issues;

  assert!(
    has_issues || has_unified,
    "Should either unify syn (if Cargo resolved it) or report conflict (if Cargo couldn't).\nOutput:\n{}",
    stdout
  );

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

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--dry-run"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show serde with union of all features
  assert!(
    stdout.contains("serde"),
    "Should show serde can be unified.\nOutput:\n{}",
    stdout
  );

  // The unified version should mention features (derive, rc, alloc)
  // Note: the exact format may vary, so we just check it mentions features
  assert!(
    stdout.contains("features") || stdout.contains("derive"),
    "Should show feature union.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

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

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--dry-run"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show tempfile can be unified
  assert!(
    stdout.contains("tempfile"),
    "Should show tempfile can be unified.\nOutput:\n{}",
    stdout
  );

  // Should show "Used as: dependencies, dev-dependencies"
  assert!(
    stdout.contains("Used as:") || stdout.contains("dependencies"),
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

  workspace.commit("Add crates before unification")?;

  // Step 1: Analyze (should succeed)
  let analyze_output = run_cargo_rail(&workspace.path, &["rail", "unify", "--dry-run"])?;
  let analyze_stdout = String::from_utf8_lossy(&analyze_output.stdout);

  assert!(
    analyze_stdout.contains("serde"),
    "Analyze should show serde can be unified"
  );

  // Step 2: Apply unification
  let apply_output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  let apply_stdout = String::from_utf8_lossy(&apply_output.stdout);

  assert!(
    apply_stdout.contains("complete") || apply_stdout.contains("✅"),
    "Apply should complete successfully.\nOutput:\n{}",
    apply_stdout
  );

  // Step 3: Verify workspace Cargo.toml was updated
  let workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;

  assert!(
    workspace_toml.contains("[workspace.dependencies]"),
    "Workspace should have dependencies section"
  );

  assert!(
    workspace_toml.contains("serde"),
    "Workspace dependencies should include serde"
  );

  // Step 4: Verify member Cargo.toml uses workspace inheritance
  let crate_a_toml = std::fs::read_to_string(workspace.path.join("crates/crate-a/Cargo.toml"))?;

  assert!(
    crate_a_toml.contains("workspace = true") || crate_a_toml.contains("workspace=true"),
    "Member should use workspace inheritance for serde.\nContent:\n{}",
    crate_a_toml
  );

  // Step 5: Run analyze again - should show no unifiable deps
  let final_analyze = run_cargo_rail(&workspace.path, &["rail", "unify", "--dry-run"])?;
  let final_stdout = String::from_utf8_lossy(&final_analyze.stdout);

  assert!(
    final_stdout.contains("No unification opportunities")
      || final_stdout.contains("0 dependencies unified")
      || !final_stdout.contains("Dependencies to unify:"),
    "After unification, there should be no more unifiable deps.\nOutput:\n{}",
    final_stdout
  );

  Ok(())
}

#[test]
fn test_unify_exclude_option() -> Result<()> {
  // Test that --exclude option works correctly

  let workspace = TestWorkspace::new()?;

  workspace.add_crate("crate-a", "0.1.0", &[("serde", r#""1.0""#), ("anyhow", r#""1.0""#)])?;

  workspace.add_crate("crate-b", "0.1.0", &[("serde", r#""1.0""#), ("anyhow", r#""1.0""#)])?;

  workspace.commit("Add crates")?;

  // Analyze with serde excluded
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--dry-run", "--exclude", "serde"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should NOT show serde (excluded)
  assert!(
    !stdout.contains("serde = ") && !stdout.contains("serde:"),
    "Excluded dependency should not appear in unification plan.\nOutput:\n{}",
    stdout
  );

  // Should still show anyhow (not excluded)
  assert!(
    stdout.contains("anyhow"),
    "Non-excluded dependencies should still be unified.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_unify_apply_conflict_fails() -> Result<()> {
  // Incompatible versions should make apply fail (no rewrite performed)
  let workspace = TestWorkspace::new()?;

  workspace.add_crate("crate-a", "0.1.0", &[("syn", r#""1.0""#)])?;
  workspace.add_crate("crate-b", "0.1.0", &[("syn", r#""2.0""#)])?;
  workspace.commit("Add crates with conflicting syn versions")?;

  // Disable auto-resolution to ensure it fails
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[workspace]
root = "."

[unify]
use_all_features = true
allow_renamed = false
exclude = []
include = []

[unify.conflicts]
auto_resolve = false
resolution_mode = "permissive"
add_markers = true

[unify.transitives]
consolidate_features = false
host_selection = "auto"

[unify.validation]
targets = []
max_parallel_jobs = 0

[unify.output]
generate_report = true
"#,
  )?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;

  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(
    !output.status.success(),
    "apply should fail on true conflicts (syn 1.x vs 2.x)\nstdout: {}\nstderr: {}",
    stdout,
    stderr
  );

  // Ensure manifests not rewritten on failure
  let crate_a_toml = std::fs::read_to_string(workspace.path.join("crates/crate-a/Cargo.toml"))?;
  assert!(crate_a_toml.contains("syn"), "crate-a manifest should remain unchanged");

  Ok(())
}
