//! Integration tests for unused dependency detection
//!
//! These tests verify that the unused dependency detection:
//! 1. Has ZERO false positives (never flags legitimate deps)
//! 2. Correctly detects truly unused deps
//!
//! This is critical - false positives would cause users to remove deps they need!

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;
use std::fs;

/// Helper to create a workspace with detect_unused enabled
fn create_workspace_with_unused_detection() -> Result<TestWorkspace> {
  let workspace = TestWorkspace::new()?;

  // Enable detect_unused in config
  let config = r#"[workspace]
root = "."

[unify]
detect_unused = true
"#;
  fs::write(workspace.path.join(".config/rail.toml"), config)?;

  Ok(workspace)
}

/// Helper to add a crate with custom Cargo.toml content
fn add_crate_with_manifest(workspace: &TestWorkspace, name: &str, manifest: &str) -> Result<()> {
  let crate_path = workspace.path.join("crates").join(name);
  fs::create_dir_all(crate_path.join("src"))?;
  fs::write(crate_path.join("Cargo.toml"), manifest)?;
  fs::write(
    crate_path.join("src/lib.rs"),
    format!("//! {} crate\npub fn hello() {{}}\n", name),
  )?;
  Ok(())
}

// TEST 1: Crate name normalization (hyphens vs underscores)

#[test]
fn test_unused_detection_crate_name_normalization() -> Result<()> {
  // This tests that deps with hyphens (js-sys) are correctly matched
  // against cargo metadata which uses underscores (js_sys)

  let workspace = create_workspace_with_unused_detection()?;

  // Use a real crate with hyphen that will be in resolved graph
  // `once_cell` is a good test because it's commonly used and will resolve
  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
once_cell = "1.0"
"#,
  )?;
  fs::write(
    workspace.path.join("crates/test-crate/src/lib.rs"),
    r#"//! test crate
use once_cell::sync::Lazy;

static CELL: Lazy<u8> = Lazy::new(|| 7);

pub fn hello() -> u8 {
  *CELL
}
"#,
  )?;

  workspace.commit("Add crate with hyphenated dependency")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  // once_cell is a real dep that WILL be in the resolved graph
  // It should NOT be flagged as unused
  assert!(
    !stdout.contains("once_cell") || !stdout.contains("Unused"),
    "once_cell should NOT be flagged as unused (name normalization should work)\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  Ok(())
}

// TEST 2: Optional deps referenced in features are NOT flagged

#[test]
fn test_unused_detection_optional_dep_in_features_not_flagged() -> Result<()> {
  // Optional deps that are referenced in [features] should NOT be flagged
  // because they're feature-gated, not truly unused

  let workspace = create_workspace_with_unused_detection()?;

  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1.0", optional = true }

[features]
default = []
serialization = ["serde"]
"#,
  )?;

  workspace.commit("Add crate with optional dep referenced in features")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // serde is optional but referenced in features - should NOT be flagged
  assert!(
    !stdout.contains("serde") || !stdout.contains("Unused"),
    "serde should NOT be flagged as unused (it's referenced in features)\nOutput:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_unused_detection_multi_target_does_not_union_false_positive() -> Result<()> {
  // Regression: rustc's unused-crate-dependencies lint is target-local.
  // A dep used in lib but not in bin must NOT be treated as package-unused.
  let workspace = create_workspace_with_unused_detection()?;

  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
once_cell = "1.0"
"#,
  )?;

  fs::write(
    workspace.path.join("crates/test-crate/src/lib.rs"),
    r#"//! test crate
use once_cell::sync::Lazy;

static CELL: Lazy<u8> = Lazy::new(|| 7);

pub fn hello() -> u8 {
  *CELL
}
"#,
  )?;
  fs::write(
    workspace.path.join("crates/test-crate/src/main.rs"),
    r#"fn main() {
  println!("bin target");
}
"#,
  )?;

  workspace.commit("Add lib+bin crate using dependency only in lib")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(
    !stdout.contains("once_cell") || !stdout.contains("Unused"),
    "once_cell should NOT be flagged as unused when used by lib target\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  Ok(())
}

#[test]
fn test_unused_detection_optional_dep_with_dep_syntax_not_flagged() -> Result<()> {
  // Test the `dep:name` syntax (Rust 2021+) for optional deps

  let workspace = create_workspace_with_unused_detection()?;

  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1.0", optional = true }

[features]
default = []
serialization = ["dep:serde"]
"#,
  )?;

  workspace.commit("Add crate with dep:name syntax")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // serde is referenced via dep:serde - should NOT be flagged
  assert!(
    !stdout.contains("serde") || !stdout.contains("Unused"),
    "serde should NOT be flagged (referenced via dep:serde syntax)\nOutput:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_unused_detection_optional_dep_with_feature_syntax_not_flagged() -> Result<()> {
  // Test that serde/derive syntax marks the dep as referenced

  let workspace = create_workspace_with_unused_detection()?;

  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1.0", optional = true }

[features]
default = []
serialization = ["serde/derive"]
"#,
  )?;

  workspace.commit("Add crate with serde/derive syntax")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // serde is referenced via serde/derive - should NOT be flagged
  assert!(
    !stdout.contains("serde") || !stdout.contains("Unused"),
    "serde should NOT be flagged (referenced via serde/derive syntax)\nOutput:\n{}",
    stdout
  );

  Ok(())
}

// TEST 3: Optional deps are NEVER flagged (even if not in [features])

#[test]
fn test_unused_detection_optional_deps_never_flagged() -> Result<()> {
  // Optional deps create implicit features in Cargo, so code can use
  // `#[cfg(feature = "dep_name")]` even without explicit [features] entries.
  // We cannot safely detect if an optional dep is unused without analyzing
  // source code for cfg attributes, so we conservatively skip all optional deps.

  let workspace = create_workspace_with_unused_detection()?;

  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1.0", optional = true }
log = "0.4"

[features]
default = []
# Note: serde is NOT referenced in any feature, but may be used via #[cfg(feature = "serde")]
"#,
  )?;
  fs::write(
    workspace.path.join("crates/test-crate/src/lib.rs"),
    r#"//! test crate
use log as _;

pub fn hello() {}
"#,
  )?;

  workspace.commit("Add crate with optional dep not in features")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // serde is optional - should NOT be flagged as unused because we can't
  // verify if it's used via #[cfg(feature = "serde")] in source code.
  assert!(
    !stdout.contains("serde") || !stdout.contains("Unused"),
    "Optional dep 'serde' should NOT be flagged as unused\nOutput:\n{}",
    stdout
  );

  Ok(())
}

// TEST 4: Target-specific deps for unconfigured targets NOT flagged

#[test]
fn test_unused_detection_target_specific_unconfigured_not_flagged() -> Result<()> {
  // Deps under [target.'cfg(windows)'.dependencies] should NOT be flagged
  // when we don't have windows target configured (we can't verify them)

  let workspace = create_workspace_with_unused_detection()?;

  // Config with only linux target (no windows)
  let config = r#"[workspace]
root = "."
targets = ["x86_64-unknown-linux-gnu"]

[unify]
detect_unused = true
"#;
  fs::write(workspace.path.join(".config/rail.toml"), config)?;

  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
log = "0.4"

[target.'cfg(windows)'.dependencies]
winapi = "0.3"
"#,
  )?;
  fs::write(
    workspace.path.join("crates/test-crate/src/lib.rs"),
    r#"//! test crate
use log as _;

pub fn hello() {}
"#,
  )?;

  workspace.commit("Add crate with windows-specific dep")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // winapi is windows-only and we don't have windows target configured
  // It should NOT be flagged (we can't verify it)
  assert!(
    !stdout.contains("winapi") || !stdout.contains("Unused"),
    "winapi should NOT be flagged (target not configured, can't verify)\nOutput:\n{}",
    stdout
  );

  Ok(())
}

// TEST 5: Regular unused deps ARE flagged

#[test]
fn test_unused_detection_truly_unused_dep_is_flagged() -> Result<()> {
  // A non-optional dep that's resolved but never referenced in source should be flagged.
  // This validates source-level detection (`unused_crate_dependencies`).

  let workspace = create_workspace_with_unused_detection()?;

  // Create a crate that declares a dep but doesn't actually use it
  // We'll use a dep that's not in the default feature set
  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
log = "0.4"

[dev-dependencies]
# This should be used for tests
"#,
  )?;

  workspace.commit("Add test crate")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // log is declared but never referenced in source - it should be flagged.
  assert!(
    stdout.contains("log"),
    "log should be flagged as unused\nOutput:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_unused_detection_single_crate_issue_11_repro() -> Result<()> {
  // Repro from GH issue #11:
  // a single-crate repo with an unused dependency should be auto-removed by `unify`.
  let workspace = TestWorkspace::new_single_crate("my-lib", "0.1.0")?;

  let cargo_toml = r#"[package]
name = "my-lib"
version = "0.1.0"
edition = "2021"

[dependencies]
eyre = "0.6"
"#;
  fs::write(workspace.path.join("Cargo.toml"), cargo_toml)?;
  workspace.commit("Add unused eyre dependency")?;

  let check = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "-f", "json"])?;
  let check_stdout = String::from_utf8_lossy(&check.stdout);
  assert!(
    check_stdout.contains("\"unused_deps\": 1"),
    "expected one unused dependency in check output\nstdout:\n{}",
    check_stdout
  );

  let apply = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    apply.status.success(),
    "unify apply should succeed\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&apply.stdout),
    String::from_utf8_lossy(&apply.stderr)
  );

  let final_toml = fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert!(
    !final_toml.contains("eyre"),
    "eyre should be removed from Cargo.toml\n{}",
    final_toml
  );

  Ok(())
}

// TEST 6: Workspace member deps are NOT flagged

#[test]
fn test_unused_detection_workspace_member_not_flagged() -> Result<()> {
  // Dependencies on other workspace members should never be flagged as unused

  let workspace = create_workspace_with_unused_detection()?;

  // Create two crates where one depends on the other
  add_crate_with_manifest(
    &workspace,
    "core-lib",
    r#"[package]
name = "core-lib"
version = "0.1.0"
edition = "2021"

[dependencies]
"#,
  )?;

  add_crate_with_manifest(
    &workspace,
    "app",
    r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
core-lib = { path = "../core-lib" }
"#,
  )?;

  workspace.commit("Add workspace with internal dependency")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // core-lib is a workspace member dep - should NEVER be flagged
  assert!(
    !stdout.contains("core-lib") || !stdout.contains("Unused"),
    "Workspace member 'core-lib' should NEVER be flagged as unused\nOutput:\n{}",
    stdout
  );

  Ok(())
}

// TEST 7: Multiple targets - deps present in ANY target are NOT flagged

#[test]
fn test_unused_detection_multi_target_union() -> Result<()> {
  // If a dep is in the resolved graph for ANY configured target,
  // it should NOT be flagged as unused

  let workspace = create_workspace_with_unused_detection()?;

  // Config with multiple targets
  let config = r#"[workspace]
root = "."
targets = ["x86_64-unknown-linux-gnu", "x86_64-apple-darwin"]

[unify]
detect_unused = true
"#;
  fs::write(workspace.path.join(".config/rail.toml"), config)?;

  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
log = "0.4"
"#,
  )?;
  fs::write(
    workspace.path.join("crates/test-crate/src/lib.rs"),
    r#"//! test crate
use log as _;

pub fn hello() {}
"#,
  )?;

  workspace.commit("Add crate with multi-target config")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // log is a real dep present on both targets - should NOT be flagged
  assert!(
    !stdout.contains("log") || !stdout.contains("Unused"),
    "log should NOT be flagged (present in resolved graph)\nOutput:\n{}",
    stdout
  );

  Ok(())
}

// TEST 8: Renamed deps (package = "...") are NOT false positives

#[test]
fn test_unused_detection_renamed_package_not_flagged() -> Result<()> {
  // Renamed deps like `memmap = { package = "memmap2" }` should NOT be flagged
  // The resolved graph uses the alias ("memmap") not the package name ("memmap2")

  let workspace = create_workspace_with_unused_detection()?;

  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
# Renamed dep: cargo.toml key is "memmap", package is "memmap2"
memmap = { package = "memmap2", version = "0.9" }
"#,
  )?;

  // Add usage to ensure it's in the resolved graph
  fs::write(
    workspace.path.join("crates/test-crate/src/lib.rs"),
    r#"//! Test crate
use memmap::Mmap;
pub fn hello() {}
"#,
  )?;

  workspace.commit("Add crate with renamed package dependency")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // memmap2 should NOT be flagged as unused - it's used via the "memmap" alias
  assert!(
    !stdout.contains("memmap2") || !stdout.contains("Unused"),
    "Renamed dep 'memmap2' should NOT be flagged as unused (alias 'memmap' is in resolved graph)\nOutput:\n{}",
    stdout
  );

  Ok(())
}

// TEST 9: Verify zero false positives with common patterns

#[test]
fn test_unused_detection_common_patterns_no_false_positives() -> Result<()> {
  // Test common real-world patterns to ensure no false positives

  let workspace = create_workspace_with_unused_detection()?;

  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
# Regular dep
log = "0.4"

# Dep with features
serde = { version = "1.0", features = ["derive"] }

# Optional dep referenced in features
tokio = { version = "1.0", optional = true }

[features]
default = []
async-runtime = ["tokio"]

[dev-dependencies]
# Dev dep for tests
tempfile = "3.0"
"#,
  )?;
  fs::write(
    workspace.path.join("crates/test-crate/src/lib.rs"),
    r#"//! Test crate
use log as _;

#[derive(serde::Serialize)]
pub struct Entry {
  pub id: u64,
}

pub fn build() -> u64 {
  let entry = Entry { id: 1 };
  entry.id
}

#[cfg(test)]
mod tests {
  use tempfile::NamedTempFile;

  #[test]
  fn creates_temp_file() {
    let file = NamedTempFile::new().expect("temp file");
    assert!(file.path().exists());
  }
}
"#,
  )?;

  workspace.commit("Add crate with common dependency patterns")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // None of these should be flagged as unused
  let false_positives = ["log", "serde", "tokio", "tempfile"]
    .iter()
    .filter(|dep| stdout.contains(*dep) && stdout.contains("Unused"))
    .collect::<Vec<_>>();

  assert!(
    false_positives.is_empty(),
    "Found false positives: {:?}\nOutput:\n{}",
    false_positives,
    stdout
  );

  Ok(())
}

#[test]
fn test_unused_detection_compiler_diag_cache_writes_cache_file() -> Result<()> {
  let workspace = create_workspace_with_unused_detection()?;

  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
log = "0.4"
"#,
  )?;

  workspace.commit("Add crate for compiler diag cache test")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  assert_eq!(
    output.status.code(),
    Some(1),
    "unify --check should exit with 1 when changes are detected\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  let cache_file = workspace.path.join("target/cargo-rail/cache/compiler-diags-v1.json");
  assert!(
    cache_file.exists(),
    "compiler diagnostics cache should exist at {}\nstdout:\n{}\nstderr:\n{}",
    cache_file.display(),
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

#[test]
fn test_unused_detection_compiler_diag_cache_disabled_does_not_write_cache_file() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  let config = r#"[workspace]
root = "."

[unify]
detect_unused = true
compiler_diag_cache = false
"#;
  fs::write(workspace.path.join(".config/rail.toml"), config)?;

  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
log = "0.4"
"#,
  )?;

  workspace.commit("Add crate for compiler diag cache disabled test")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  assert_eq!(
    output.status.code(),
    Some(1),
    "unify --check should exit with 1 when changes are detected\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  let cache_file = workspace.path.join("target/cargo-rail/cache/compiler-diags-v1.json");
  assert!(
    !cache_file.exists(),
    "compiler diagnostics cache should not exist when disabled at {}\nstdout:\n{}\nstderr:\n{}",
    cache_file.display(),
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}
