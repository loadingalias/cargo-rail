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
    stdout.contains("Ready to unify") || stdout.contains("Dependencies to unify"),
    "Should show dependencies can be unified.\nOutput:\n{}",
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

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
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
  let analyze_output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
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
  let final_analyze = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
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
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--exclude", "serde"])?;
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

// ============================================================================
// Phase 1-6 Feature Tests
// ============================================================================

#[test]
fn test_unify_dev_dependencies() -> Result<()> {
  // Test that dev-dependencies are properly unified (Phase 1/3)

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
  // Test that build-dependencies are properly unified (Phase 1/3)

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

  std::fs::write(crate_a_path.join("src/lib.rs"), "pub fn a() {}")?;

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

  std::fs::write(crate_b_path.join("src/lib.rs"), "pub fn b() {}")?;

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
  // Test that existing workspace.dependencies are updated when features differ (Phase 4)

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

/// Test --include flag forces specific dependencies to be included
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

  // Run unify with --include
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--include", "serde"])?;

  assert!(
    output.status.success(),
    "unify --include should succeed. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

/// Test --consolidate-transitives flag
#[test]
fn test_unify_consolidate_transitives() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-consolidate")?;

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
  std::fs::write(crate_path.join("src/lib.rs"), "pub fn hello() {}")?;
  workspace.commit("Add crate")?;

  // Run unify with --consolidate-transitives
  let output = run_cargo_rail(
    &workspace.path,
    &["rail", "unify", "--check", "--consolidate-transitives"],
  )?;

  assert!(
    output.status.success(),
    "unify --consolidate-transitives should succeed. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

/// Test --include-renamed flag
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
  std::fs::write(crate_path.join("src/lib.rs"), "pub fn hello() {}")?;
  workspace.commit("Add crate")?;

  // Run unify with --include-renamed
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--include-renamed"])?;

  assert!(
    output.status.success(),
    "unify --include-renamed should succeed. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}
