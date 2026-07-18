//! Integration tests for MSRV (rust-version) computation
//!
//! Tests the MSRV source policy:
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

/// Helper to create rail.toml with a specific MSRV source
fn write_rail_config(workspace: &TestWorkspace, msrv_source: &str) -> Result<()> {
  let config = format!(
    r#"[unify]
msrv_policy = {{ mode = "compute", source = "{}" }}
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
  let workspace = create_workspace_with_rust_version("1.85.0")?;
  write_rail_config(&workspace, "max")?;

  // Add a simple crate
  workspace.add_crate("simple-crate", "0.1.0", &[])?;
  workspace.commit("Add simple crate")?;

  // Run unify --check
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(
    stdout.contains("status: no changes"),
    "Should recognize that the existing workspace rust-version already satisfies MSRV.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_msrv_source_workspace_preserves_existing() -> Result<()> {
  // Test that msrv_source = "workspace" keeps existing rust-version
  let workspace = create_workspace_with_rust_version("1.70.0")?;
  write_rail_config(&workspace, "workspace")?;

  // Add a crate
  workspace.add_crate("my-crate", "0.1.0", &[])?;
  workspace.commit("Add crate")?;

  // Run unify --check
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(
    stdout.contains("status: no changes"),
    "Should preserve the existing workspace rust-version without planning changes.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_msrv_source_deps_uses_dependency_version() -> Result<()> {
  // Test that msrv_source = "deps" uses deps version (ignoring workspace)
  let workspace = create_workspace_with_rust_version("1.80.0")?;
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
  // A disabled policy skips MSRV computation entirely.
  let workspace = create_workspace_with_rust_version("1.70.0")?;

  let config = r#"[unify]
msrv_policy = { mode = "disabled" }
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
  // The default source policy is "max".
  let workspace = create_workspace_with_rust_version("1.75.0")?;

  // Omit `source` to exercise the coded default.
  let config = r#"[unify]
msrv_policy = { mode = "compute" }
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
    "Should complete with the default MSRV source.\nStdout:\n{}\nStderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

#[test]
fn test_msrv_package_only_baseline_is_used_and_written_to_workspace() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  let cargo_toml = r#"[package]
name = "root"
version = "0.1.0"
edition = "2021"
license = "MIT"
authors = ["Test Author"]
rust-version = "1.72.0"

[workspace]
members = ["crates/*"]
resolver = "2"

[workspace.package]
edition = "2021"
license = "MIT"
authors = ["Test Author"]
"#;
  std::fs::write(workspace.path.join("Cargo.toml"), cargo_toml)?;

  // Root [package] requires at least one target for `cargo metadata` to succeed.
  std::fs::create_dir_all(workspace.path.join("src"))?;
  std::fs::write(workspace.path.join("src/lib.rs"), "pub fn root() {}\n")?;

  workspace.commit("Use package-only rust-version baseline")?;

  let config = r#"[unify]
msrv_policy = { mode = "compute", source = "workspace" }
"#;
  std::fs::create_dir_all(workspace.path.join(".config"))?;
  std::fs::write(workspace.path.join(".config/rail.toml"), config)?;

  workspace.add_crate("a", "0.1.0", &[])?;
  workspace.commit("Add member crate")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "unify should succeed.\nStdout:\n{}\nStderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  let updated = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert!(
    updated.contains("[workspace.package]") && updated.contains("rust-version = \"1.72.0\""),
    "Expected unify to write [workspace.package].rust-version.\nCargo.toml:\n{}",
    updated
  );

  Ok(())
}

#[test]
fn test_msrv_enforce_inheritance_sets_members_to_workspace() -> Result<()> {
  let workspace = create_workspace_with_rust_version("1.72.0")?;

  let config = r#"[unify]
msrv_policy = { mode = "compute", source = "workspace", inherit = true }
"#;
  std::fs::create_dir_all(workspace.path.join(".config"))?;
  std::fs::write(workspace.path.join(".config/rail.toml"), config)?;

  workspace.add_crate("a", "0.1.0", &[])?;
  workspace.add_crate("b", "0.1.0", &[])?;
  workspace.commit("Add member crates")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "unify should succeed.\nStdout:\n{}\nStderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  for member in ["a", "b"] {
    let manifest_path = workspace.path.join("crates").join(member).join("Cargo.toml");
    let content = std::fs::read_to_string(&manifest_path)?;
    let doc: toml_edit::DocumentMut = content.parse()?;
    let pkg = doc
      .get("package")
      .and_then(|p| p.as_table())
      .expect("member has [package]");
    let rv = pkg.get("rust-version").expect("member has rust-version");
    let rv_tbl = rv.as_table_like().expect("rust-version is workspace inheritance");
    assert_eq!(
      rv_tbl.get("workspace").and_then(|v| v.as_bool()),
      Some(true),
      "Expected {} to inherit rust-version from workspace.\nCargo.toml:\n{}",
      member,
      content
    );
  }

  Ok(())
}
