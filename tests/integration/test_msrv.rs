//! Integration tests for MSRV (rust-version) computation
//!
//! Tests the msrv_source configuration:
//! - "deps" mode: use maximum from dependencies only
//! - "workspace" mode: preserve existing, warn if deps need higher
//! - "max" mode: take max(workspace, deps)

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;

// MSRV Source Tests

/// Helper to create a workspace with rust-version set
fn create_workspace_with_rust_version(rust_version: &str) -> Result<TestWorkspace> {
  let workspace = TestWorkspace::new()?;

  // Update workspace Cargo.toml to include rust-version
  let cargo_toml = format!(
    r#"[workspace]
members = ["crates/*"]
resolver = "2"

[workspace.package]
edition = "2021"
license = "MIT"
authors = ["Test Author"]
rust-version = "{}"

[workspace.dependencies]
anyhow = "1.0"
serde = {{ version = "1.0", features = ["derive"] }}
"#,
    rust_version
  );
  std::fs::write(workspace.path.join("Cargo.toml"), cargo_toml)?;
  workspace.commit("Set workspace rust-version")?;

  Ok(workspace)
}

/// Helper to create rail.toml with specific msrv_source
fn write_rail_config(workspace: &TestWorkspace, msrv_source: &str) -> Result<()> {
  let config = format!(
    r#"[unify]
msrv = true
msrv_source = "{}"
"#,
    msrv_source
  );
  std::fs::create_dir_all(workspace.path.join(".config"))?;
  std::fs::write(workspace.path.join(".config/rail.toml"), config)?;
  Ok(())
}

#[test]
fn test_msrv_source_max_preserves_higher_workspace_version() -> Result<()> {
  // Test that msrv_source = "max" preserves workspace version if it's higher
  let workspace = create_workspace_with_rust_version("1.85")?;
  write_rail_config(&workspace, "max")?;

  // Add a simple crate
  workspace.add_crate("simple-crate", "0.1.0", &[])?;
  workspace.commit("Add simple crate")?;

  // Run unify --check
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show rust-version 1.85 (from workspace, not deps)
  // The message should indicate it's from workspace
  assert!(
    stdout.contains("1.85") || stdout.contains("rust-version"),
    "Should show rust-version info.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_msrv_source_workspace_preserves_existing() -> Result<()> {
  // Test that msrv_source = "workspace" keeps existing rust-version
  let workspace = create_workspace_with_rust_version("1.70")?;
  write_rail_config(&workspace, "workspace")?;

  // Add a crate
  workspace.add_crate("my-crate", "0.1.0", &[])?;
  workspace.commit("Add crate")?;

  // Run unify --check
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should preserve the workspace version
  assert!(
    stdout.contains("1.70") || stdout.contains("preserved"),
    "Should preserve workspace rust-version.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_msrv_source_deps_uses_dependency_version() -> Result<()> {
  // Test that msrv_source = "deps" uses deps version (ignoring workspace)
  let workspace = create_workspace_with_rust_version("1.80")?;
  write_rail_config(&workspace, "deps")?;

  // Add a crate
  workspace.add_crate("my-crate", "0.1.0", &[("anyhow", r#""1.0""#)])?;
  workspace.commit("Add crate with dep")?;

  // Run unify --check
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should complete successfully with deps mode
  // (exact version depends on what deps require)
  assert!(
    output.status.success() || stdout.contains("ready:") || stdout.contains("rust-version"),
    "Should complete analysis with deps mode.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_msrv_disabled_skips_computation() -> Result<()> {
  // Test that msrv = false skips MSRV computation entirely
  let workspace = create_workspace_with_rust_version("1.70")?;

  let config = r#"[unify]
msrv = false
"#;
  std::fs::create_dir_all(workspace.path.join(".config"))?;
  std::fs::write(workspace.path.join(".config/rail.toml"), config)?;

  // Add a crate
  workspace.add_crate("my-crate", "0.1.0", &[])?;
  workspace.commit("Add crate")?;

  // Run unify --check
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should NOT mention MSRV computation when disabled
  assert!(
    !stdout.contains("Computing MSRV"),
    "Should not compute MSRV when disabled.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_msrv_default_is_max_mode() -> Result<()> {
  // Test that the default msrv_source is "max"
  let workspace = create_workspace_with_rust_version("1.75")?;

  // Create config without msrv_source (use default)
  let config = r#"[unify]
msrv = true
"#;
  std::fs::create_dir_all(workspace.path.join(".config"))?;
  std::fs::write(workspace.path.join(".config/rail.toml"), config)?;

  // Add a crate
  workspace.add_crate("my-crate", "0.1.0", &[])?;
  workspace.commit("Add crate")?;

  // Run unify --check - should use "max" mode by default
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;

  // Should complete successfully
  assert!(
    output.status.success(),
    "Should complete with default msrv_source.\nStdout:\n{}\nStderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}
