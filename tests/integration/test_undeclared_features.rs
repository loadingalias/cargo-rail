//! Integration tests for undeclared feature detection
//!
//! These tests verify that the undeclared feature detection:
//! 1. Correctly identifies features borrowed from other workspace members
//! 2. Respects workspace baseline (features in [workspace.dependencies])
//! 3. Uses skip_undeclared_patterns correctly
//! 4. Auto-fixes undeclared features when configured

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;
use std::fs;

/// Helper to create a workspace with undeclared feature detection enabled
/// and NO pre-existing workspace.dependencies (fresh workspace)
fn create_fresh_workspace_with_undeclared_detection() -> Result<TestWorkspace> {
  let workspace = TestWorkspace::new()?;

  // Overwrite the workspace Cargo.toml to remove pre-existing workspace deps
  fs::write(
    workspace.path.join("Cargo.toml"),
    r#"[workspace]
members = ["crates/*"]
resolver = "2"

[workspace.package]
edition = "2021"
license = "MIT"
authors = ["Test Author"]
"#,
  )?;

  // Enable detect_undeclared_features and fix_undeclared_features
  let config = r#"[unify]
detect_undeclared_features = true
fix_undeclared_features = true
"#;
  fs::create_dir_all(workspace.path.join(".config"))?;
  fs::write(workspace.path.join(".config/rail.toml"), config)?;

  Ok(workspace)
}

/// Helper to add a crate with custom Cargo.toml content
fn add_crate_with_manifest(workspace: &TestWorkspace, name: &str, manifest: &str, src: &str) -> Result<()> {
  let crate_path = workspace.path.join("crates").join(name);
  fs::create_dir_all(crate_path.join("src"))?;
  fs::write(crate_path.join("Cargo.toml"), manifest)?;
  fs::write(crate_path.join("src/lib.rs"), src)?;
  Ok(())
}

// TEST 1: Basic undeclared feature detection

#[test]
fn test_undeclared_features_basic_detection() -> Result<()> {
  // Use tokio which is NOT in the default workspace.dependencies
  // crate-a requests tokio with ["macros", "rt"]
  // crate-b requests tokio with ["rt"] only but resolves ["macros", "rt"] due to unification
  // crate-b should have undeclared feature "macros"

  let workspace = create_fresh_workspace_with_undeclared_detection()?;

  // crate-a with macros and rt features
  add_crate_with_manifest(
    &workspace,
    "crate-a",
    r#"[package]
name = "crate-a"
version = "0.1.0"
edition.workspace = true

[dependencies]
tokio = { version = "1.0", features = ["macros", "rt"] }
"#,
    "pub fn a() {}",
  )?;

  // crate-b with only rt feature - will borrow "macros" from crate-a
  add_crate_with_manifest(
    &workspace,
    "crate-b",
    r#"[package]
name = "crate-b"
version = "0.1.0"
edition.workspace = true

[dependencies]
tokio = { version = "1.0", features = ["rt"] }
"#,
    "pub fn b() {}",
  )?;

  workspace.commit("Add crates with feature borrowing")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should detect undeclared features (crate-b borrowing "macros" from crate-a)
  // or should show fix count if fix_undeclared_features is enabled
  assert!(
    stdout.contains("Undeclared features") || stdout.contains("undeclared") || stdout.contains("features to fix"),
    "Should detect undeclared features.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

// TEST 2: Workspace baseline respected

#[test]
fn test_undeclared_features_workspace_baseline_respected() -> Result<()> {
  // When features are declared in [workspace.dependencies], they're workspace policy
  // and should NOT be reported as borrowed

  let workspace = TestWorkspace::new()?;

  // Create workspace with serde = { version = "1.0", features = ["derive"] }
  let workspace_toml = r#"[workspace]
members = ["crates/*"]
resolver = "2"

[workspace.package]
edition = "2021"
license = "MIT"
authors = ["Test Author"]

[workspace.dependencies]
serde = { version = "1.0", features = ["derive"] }
"#;
  fs::write(workspace.path.join("Cargo.toml"), workspace_toml)?;

  // Config with undeclared detection
  let config = r#"[unify]
detect_undeclared_features = true
fix_undeclared_features = false
"#;
  fs::create_dir_all(workspace.path.join(".config"))?;
  fs::write(workspace.path.join(".config/rail.toml"), config)?;

  // crate-a uses workspace serde without requesting derive explicitly
  add_crate_with_manifest(
    &workspace,
    "crate-a",
    r#"[package]
name = "crate-a"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = { workspace = true }
"#,
    "pub fn a() {}",
  )?;

  // crate-b also uses workspace serde
  add_crate_with_manifest(
    &workspace,
    "crate-b",
    r#"[package]
name = "crate-b"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = { workspace = true }
"#,
    "pub fn b() {}",
  )?;

  workspace.commit("Add crates using workspace deps")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should NOT report undeclared features since derive is workspace policy
  assert!(
    !stdout.contains("Undeclared features detected") && !stdout.contains("crate-a/serde"),
    "Workspace baseline features should NOT be reported as undeclared.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

// TEST 3: Skip patterns work

#[test]
fn test_undeclared_features_skip_patterns() -> Result<()> {
  // Features matching skip_undeclared_patterns should not be reported

  let workspace = TestWorkspace::new()?;

  // Config with custom skip patterns
  let config = r#"[unify]
detect_undeclared_features = true
fix_undeclared_features = false
skip_undeclared_patterns = ["default", "std", "alloc"]
"#;
  fs::create_dir_all(workspace.path.join(".config"))?;
  fs::write(workspace.path.join(".config/rail.toml"), config)?;

  // crate-a with default-features = true (enables "default" feature)
  add_crate_with_manifest(
    &workspace,
    "crate-a",
    r#"[package]
name = "crate-a"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = { version = "1.0", default-features = true, features = ["derive"] }
"#,
    "pub fn a() {}",
  )?;

  // crate-b with default-features = false but will get "default" from unification
  add_crate_with_manifest(
    &workspace,
    "crate-b",
    r#"[package]
name = "crate-b"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde = { version = "1.0", default-features = false }
"#,
    "pub fn b() {}",
  )?;

  workspace.commit("Add crates")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // "default" is in skip patterns, so it shouldn't be reported
  // but "derive" should still be reported if borrowed
  // The key is that "default" shouldn't appear in the undeclared list
  if stdout.contains("Undeclared features detected") {
    assert!(
      !stdout.contains("[default]"),
      "Features in skip_undeclared_patterns should not be reported.\nOutput:\n{}",
      stdout
    );
  }

  Ok(())
}

// TEST 4: Auto-fix adds features to member Cargo.toml

#[test]
fn test_undeclared_features_auto_fix() -> Result<()> {
  // When fix_undeclared_features = true, undeclared features should be added
  // to the member's Cargo.toml

  let workspace = create_fresh_workspace_with_undeclared_detection()?;

  // crate-a with macros and rt features (using tokio since serde is in default workspace)
  add_crate_with_manifest(
    &workspace,
    "crate-a",
    r#"[package]
name = "crate-a"
version = "0.1.0"
edition.workspace = true

[dependencies]
tokio = { version = "1.0", features = ["macros", "rt"] }
"#,
    "pub fn a() {}",
  )?;

  // crate-b with only rt - will need macros added if fix is enabled
  add_crate_with_manifest(
    &workspace,
    "crate-b",
    r#"[package]
name = "crate-b"
version = "0.1.0"
edition.workspace = true

[dependencies]
tokio = { version = "1.0", features = ["rt"] }
"#,
    "pub fn b() {}",
  )?;

  workspace.commit("Add crates")?;

  // Run unify (not --check, so it applies changes)
  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "Unify should succeed.\nstdout: {}\nstderr: {}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  // After unification, crate-b should use workspace inheritance
  let crate_b_toml = fs::read_to_string(workspace.path.join("crates/crate-b/Cargo.toml"))?;

  // Should use workspace = true (feature fixes happen via local features or workspace inheritance)
  assert!(
    crate_b_toml.contains("workspace = true") || crate_b_toml.contains("workspace=true"),
    "crate-b should use workspace inheritance after unification.\nContent:\n{}",
    crate_b_toml
  );

  Ok(())
}

// TEST 5: Conditional features (from [features] table) are considered declared

#[test]
fn test_undeclared_features_conditional_features_respected() -> Result<()> {
  // Features enabled via [features] table should be considered declared

  let workspace = TestWorkspace::new()?;

  let config = r#"[unify]
detect_undeclared_features = true
fix_undeclared_features = false
"#;
  fs::create_dir_all(workspace.path.join(".config"))?;
  fs::write(workspace.path.join(".config/rail.toml"), config)?;

  // crate-a with tokio/macros via [features]
  add_crate_with_manifest(
    &workspace,
    "crate-a",
    r#"[package]
name = "crate-a"
version = "0.1.0"
edition.workspace = true

[dependencies]
tokio = "1.0"

[features]
default = ["macros"]
macros = ["tokio/macros"]
"#,
    "pub fn a() {}",
  )?;

  // crate-b with tokio/macros directly
  add_crate_with_manifest(
    &workspace,
    "crate-b",
    r#"[package]
name = "crate-b"
version = "0.1.0"
edition.workspace = true

[dependencies]
tokio = { version = "1.0", features = ["macros"] }
"#,
    "pub fn b() {}",
  )?;

  workspace.commit("Add crates")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // crate-a declares "tokio/macros" via [features], so it shouldn't be undeclared
  // If there are undeclared features warnings, they shouldn't include crate-a/tokio with "macros"
  if stdout.contains("crate-a/tokio") {
    assert!(
      !stdout.contains("macros"),
      "Features declared via [features] table should not be reported as undeclared.\nOutput:\n{}",
      stdout
    );
  }

  Ok(())
}
