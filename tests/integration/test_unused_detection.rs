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

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
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

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
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

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
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
  let config = r#"targets = ["x86_64-unknown-linux-gnu"]

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

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
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
  let check_json: serde_json::Value = serde_json::from_slice(&check.stdout)?;
  let proof = check_json["proof_certificates"]
    .as_array()
    .and_then(|certificates| {
      certificates
        .iter()
        .find(|certificate| certificate["subject"]["declaration"] == "eyre")
    })
    .expect("unused eyre dependency proof");
  assert_eq!(proof["schema_version"], 1);
  assert_eq!(proof["subject"]["declaration"], "eyre");
  assert_eq!(proof["subject"]["dependency_kind"], "normal");
  assert_eq!(proof["decision"], "remove");
  assert_eq!(proof["used_observations"], 0);
  assert_eq!(proof["incomplete_observations"], 0);
  assert_eq!(proof["applicable_configurations"], proof["complete_configurations"]);

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

#[test]
fn test_unused_detection_keeps_duplicate_package_versions_and_aliases_distinct() -> Result<()> {
  let workspace = create_workspace_with_unused_detection()?;
  add_crate_with_manifest(
    &workspace,
    "old-consumer",
    r#"[package]
name = "old-consumer"
version = "0.1.0"
edition = "2021"

[dependencies]
syn-old = { package = "syn", version = "=1.0.109" }
"#,
  )?;
  add_crate_with_manifest(
    &workspace,
    "new-consumer",
    r#"[package]
name = "new-consumer"
version = "0.1.0"
edition = "2021"

[dependencies]
syn-new = { package = "syn", version = "=2.0.118" }
"#,
  )?;
  fs::write(
    workspace.path.join("crates/new-consumer/src/lib.rs"),
    "pub fn parses() -> bool { syn_new::parse_str::<syn_new::Type>(\"u8\").is_ok() }\n",
  )?;
  workspace.commit("Resolve two aliased versions of one package")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "-f", "json"])?;
  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  let removals: Vec<_> = json["proof_certificates"]
    .as_array()
    .into_iter()
    .flatten()
    .filter(|certificate| certificate["decision"] == "remove")
    .filter_map(|certificate| {
      Some((
        certificate["member"].as_str()?,
        certificate["subject"]["declaration"].as_str()?,
      ))
    })
    .collect();
  assert!(
    removals.contains(&("old-consumer", "syn-old")),
    "unused alias must be removable: {json:#}"
  );
  assert!(
    !removals.contains(&("new-consumer", "syn-new")),
    "usage of one package ID must not be attributed to another version: {json:#}"
  );

  Ok(())
}

#[test]
fn test_unused_detection_removes_unused_renamed_declaration_by_alias() -> Result<()> {
  let workspace = create_workspace_with_unused_detection()?;

  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
tracing_log = { package = "log", version = "0.4" }
"#,
  )?;
  workspace.commit("Add unused renamed dependency")?;

  let check = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
  let check_stdout = String::from_utf8_lossy(&check.stdout);
  assert!(
    check_stdout.contains("Unused dependencies flagged for removal"),
    "{check_stdout}"
  );
  assert!(
    check_stdout.contains("tracing_log"),
    "unused dependency should be reported by its manifest alias\nstdout:\n{}",
    check_stdout
  );

  let apply = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    apply.status.success(),
    "unify should remove the renamed declaration\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&apply.stdout),
    String::from_utf8_lossy(&apply.stderr)
  );

  let manifest = fs::read_to_string(workspace.path.join("crates/test-crate/Cargo.toml"))?;
  assert!(
    !manifest.contains("tracing_log"),
    "renamed dependency alias should be removed\n{}",
    manifest
  );

  Ok(())
}

#[test]
fn test_unused_detection_preserves_dependency_used_only_without_default_features() -> Result<()> {
  let workspace = create_workspace_with_unused_detection()?;

  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[features]
default = ["extra"]
extra = []

[dependencies]
log = "0.4"
"#,
  )?;
  fs::write(
    workspace.path.join("crates/test-crate/src/lib.rs"),
    r#"#[cfg(not(feature = "extra"))]
pub fn initialize() {
  log::set_max_level(log::LevelFilter::Info);
}
"#,
  )?;
  workspace.commit("Use dependency only without default features")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    !stdout.contains("Remove log"),
    "a no-default-features usage must disprove removal\nstdout:\n{}\nstderr:\n{}",
    stdout,
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

#[test]
fn test_unused_detection_plans_mutually_exclusive_feature_condition() -> Result<()> {
  let workspace = create_workspace_with_unused_detection()?;

  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[features]
backend-a = []
backend-b = []
backend-common = []

[dependencies]
log = "0.4"
"#,
  )?;
  fs::write(
    workspace.path.join("crates/test-crate/src/lib.rs"),
    r#"#[cfg(all(feature = "backend-a", feature = "backend-common", not(feature = "backend-b")))]
pub fn initialize_backend_a() {
  log::set_max_level(log::LevelFilter::Info);
}
"#,
  )?;
  workspace.commit("Use dependency in a mutually exclusive feature branch")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
  assert!(
    !String::from_utf8_lossy(&output.stdout).contains("Remove log"),
    "the planner must compile backend-a + backend-common without backend-b before authorizing removal\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

#[test]
fn test_unused_detection_applies_independent_dev_build_and_optional_completeness() -> Result<()> {
  let workspace = create_workspace_with_unused_detection()?;
  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", optional = true }

[dev-dependencies]
log = "0.4"

[build-dependencies]
cc = "1"
"#,
  )?;
  fs::create_dir_all(workspace.path.join("crates/test-crate/tests"))?;
  fs::write(
    workspace.path.join("crates/test-crate/tests/empty.rs"),
    "#[test] fn empty() {}\n",
  )?;
  workspace.commit("Add dependencies requiring separate evidence domains")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    !stdout.contains("cc in test-crate"),
    "no build script means build evidence is incomplete\n{stdout}"
  );
  assert!(
    stdout.contains("preserved normal dependency `serde` in `test-crate`"),
    "published optional dependency must remain with an explicit reason\n{stdout}"
  );
  assert!(stdout.contains("optional activation reachability"), "{stdout}");

  Ok(())
}

#[test]
fn test_unused_detection_checks_required_feature_cargo_target_configuration() -> Result<()> {
  let workspace = create_workspace_with_unused_detection()?;
  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[features]
example-gate = []

[dependencies]
log = "0.4"

[[example]]
name = "gated"
required-features = ["example-gate"]
"#,
  )?;
  fs::create_dir_all(workspace.path.join("crates/test-crate/examples"))?;
  fs::write(
    workspace.path.join("crates/test-crate/examples/gated.rs"),
    "fn main() { log::set_max_level(log::LevelFilter::Info); }\n",
  )?;
  workspace.commit("Use dependency from a required-features example")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
  assert!(
    !String::from_utf8_lossy(&output.stdout).contains("Remove log"),
    "required-features Cargo targets must contribute usage evidence\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  let cache = fs::read_to_string(workspace.path.join("target/cargo-rail/cache/compiler-diags-v1.json"))?;
  assert!(
    cache.contains("example-gate"),
    "the evidence cache should retain the required feature configuration\n{cache}"
  );

  Ok(())
}

#[test]
fn test_unused_detection_preserves_dependency_used_only_by_doctest() -> Result<()> {
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
  fs::write(
    workspace.path.join("crates/test-crate/src/lib.rs"),
    r#"/// Initializes logging.
///
/// ```
/// log::set_max_level(log::LevelFilter::Info);
/// ```
pub fn initialize() {}
"#,
  )?;
  workspace.commit("Use dependency only from a doctest")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
  assert!(
    !String::from_utf8_lossy(&output.stdout).contains("Remove log"),
    "doctest-only usage must preserve the dependency\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

#[test]
fn test_unused_detection_preserves_dev_dependencies_used_by_examples_and_benchmarks() -> Result<()> {
  let workspace = create_workspace_with_unused_detection()?;
  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dev-dependencies]
log = "0.4"
once_cell = "1"
"#,
  )?;
  fs::create_dir_all(workspace.path.join("crates/test-crate/examples"))?;
  fs::create_dir_all(workspace.path.join("crates/test-crate/benches"))?;
  fs::write(
    workspace.path.join("crates/test-crate/examples/log_usage.rs"),
    "fn main() { log::set_max_level(log::LevelFilter::Info); }\n",
  )?;
  fs::write(
    workspace.path.join("crates/test-crate/benches/cell_usage.rs"),
    "use once_cell::sync::Lazy;\nstatic VALUE: Lazy<u8> = Lazy::new(|| 1);\n#[test] fn bench_domain() { assert_eq!(*VALUE, 1); }\n",
  )?;
  workspace.commit("Use dev dependencies in separate Cargo target domains")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "-f", "json"])?;
  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  assert!(
    !json["proof_certificates"].as_array().is_some_and(|certificates| {
      certificates.iter().any(|certificate| {
        certificate["decision"] == "remove"
          && matches!(
            certificate["subject"]["declaration"].as_str(),
            Some("log" | "once_cell")
          )
      })
    }),
    "usage in any applicable dev compilation unit must disprove removal\n{}",
    serde_json::to_string_pretty(&json)?
  );

  Ok(())
}

#[test]
fn test_unused_detection_preserves_dependency_used_only_by_proc_macro_expansion() -> Result<()> {
  let workspace = create_workspace_with_unused_detection()?;
  add_crate_with_manifest(
    &workspace,
    "macro-provider",
    r#"[package]
name = "macro-provider"
version = "0.1.0"
edition = "2021"

[lib]
proc-macro = true
"#,
  )?;
  fs::write(
    workspace.path.join("crates/macro-provider/src/lib.rs"),
    r#"extern crate proc_macro;
use proc_macro::TokenStream;

#[proc_macro]
pub fn generate(_input: TokenStream) -> TokenStream {
  "pub fn generated() { ::log::set_max_level(::log::LevelFilter::Info); }"
    .parse()
    .expect("valid generated tokens")
}
"#,
  )?;
  add_crate_with_manifest(
    &workspace,
    "consumer",
    r#"[package]
name = "consumer"
version = "0.1.0"
edition = "2021"

[dependencies]
macro-provider = { path = "../macro-provider" }
log = "0.4"
"#,
  )?;
  fs::write(
    workspace.path.join("crates/consumer/src/lib.rs"),
    "macro_provider::generate!();\n",
  )?;
  workspace.commit("Use dependency only from procedural macro expansion")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "-f", "json"])?;
  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  assert!(
    !json["proof_certificates"].as_array().is_some_and(|certificates| {
      certificates.iter().any(|certificate| {
        certificate["member"] == "consumer"
          && certificate["decision"] == "remove"
          && certificate["subject"]["declaration"] == "log"
      })
    }),
    "rustc expansion evidence must preserve the generated dependency use\n{}",
    serde_json::to_string_pretty(&json)?
  );

  Ok(())
}

#[test]
fn test_unused_detection_preserves_dependency_used_only_by_generated_source() -> Result<()> {
  let workspace = create_workspace_with_unused_detection()?;
  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"
build = "build.rs"

[dependencies]
log = "0.4"
"#,
  )?;
  fs::write(
    workspace.path.join("crates/test-crate/build.rs"),
    r#"fn main() {
  let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
  std::fs::write(output.join("generated.rs"),
    "pub fn initialize() { log::set_max_level(log::LevelFilter::Info); }").expect("write generated source");
}
"#,
  )?;
  fs::write(
    workspace.path.join("crates/test-crate/src/lib.rs"),
    "include!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\"));\n",
  )?;
  workspace.commit("Use dependency only from generated Rust source")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
  assert!(
    !String::from_utf8_lossy(&output.stdout).contains("Remove log"),
    "rustc-expanded generated usage must preserve the dependency\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

#[test]
fn test_unused_detection_removes_unreachable_optional_only_from_private_package() -> Result<()> {
  let workspace = create_workspace_with_unused_detection()?;
  fs::write(
    workspace.path.join(".config/rail.toml"),
    "[workspace]\nroot = \".\"\n\n[unify]\ndetect_unused = true\nconsumer_scope = \"workspace\"\n",
  )?;
  add_crate_with_manifest(
    &workspace,
    "private-crate",
    r#"[package]
name = "private-crate"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
log = { version = "0.4", optional = true }
"#,
  )?;
  add_crate_with_manifest(
    &workspace,
    "public-crate",
    r#"[package]
name = "public-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
log = { version = "0.4", optional = true }
"#,
  )?;
  workspace.commit("Add private and public inactive optional dependencies")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "optional dependency cleanup should verify\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  let private = fs::read_to_string(workspace.path.join("crates/private-crate/Cargo.toml"))?;
  let public = fs::read_to_string(workspace.path.join("crates/public-crate/Cargo.toml"))?;
  assert!(
    !private.contains("log ="),
    "private unreachable optional should be removed\n{private}"
  );
  assert!(
    public.contains("log ="),
    "published optional is public API and must remain\n{public}"
  );

  Ok(())
}

#[test]
fn test_unused_detection_separates_dev_and_build_compilation_domains() -> Result<()> {
  let workspace = create_workspace_with_unused_detection()?;
  fs::write(
    workspace.path.join(".config/rail.toml"),
    "[workspace]\nroot = \".\"\n\n[unify]\ndetect_unused = true\ncompiler_diag_cache = true\n",
  )?;
  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"
build = "build.rs"

[dev-dependencies]
log = "0.4"
tempfile = "3"

[build-dependencies]
cc = "1"
glob = "0.3"
"#,
  )?;
  fs::create_dir_all(workspace.path.join("crates/test-crate/tests"))?;
  fs::write(
    workspace.path.join("crates/test-crate/tests/uses_log.rs"),
    "#[test] fn uses_log() { log::set_max_level(log::LevelFilter::Info); }\n",
  )?;
  fs::write(
    workspace.path.join("crates/test-crate/build.rs"),
    "fn main() { let _builder = cc::Build::new(); }\n",
  )?;
  workspace.commit("Add independently used and unused dev/build dependencies")?;

  let check = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
  let check_stdout = String::from_utf8_lossy(&check.stdout);
  assert!(
    check_stdout.contains("tempfile"),
    "unused dev evidence missing\n{check_stdout}"
  );
  assert!(
    check_stdout.contains("glob"),
    "unused build evidence missing\n{check_stdout}"
  );
  let evidence_cache = fs::read_to_string(workspace.path.join("target/cargo-rail/cache/compiler-diags-v1.json"))?;
  assert!(evidence_cache.contains("CustomBuild"), "{evidence_cache}");
  assert!(evidence_cache.contains("unit_evidence"), "{evidence_cache}");
  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "domain-specific cleanup should verify\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  let manifest = fs::read_to_string(workspace.path.join("crates/test-crate/Cargo.toml"))?;
  assert!(
    manifest.contains("log ="),
    "test-only usage must preserve log\n{manifest}"
  );
  assert!(
    manifest.contains("cc ="),
    "build-script usage must preserve cc\n{manifest}"
  );
  assert!(
    !manifest.contains("tempfile ="),
    "unused dev dependency should be removed\n{manifest}\ncheck:\n{check_stdout}"
  );
  assert!(
    !manifest.contains("glob ="),
    "unused build dependency should be removed\n{manifest}\ncheck:\n{check_stdout}"
  );

  Ok(())
}

#[test]
fn test_unused_detection_preserves_build_dependency_with_native_links_contract() -> Result<()> {
  let workspace = create_workspace_with_unused_detection()?;
  let native = workspace.path.join("vendor/native-side-effect");
  fs::create_dir_all(native.join("src"))?;
  fs::write(
    native.join("Cargo.toml"),
    r#"[package]
name = "native-side-effect"
version = "0.1.0"
edition = "2021"
links = "cargo_rail_test_native"
build = "build.rs"
"#,
  )?;
  fs::write(native.join("src/lib.rs"), "pub fn marker() {}\n")?;
  fs::write(
    native.join("build.rs"),
    "fn main() { println!(\"cargo:rerun-if-changed=build.rs\"); }\n",
  )?;
  add_crate_with_manifest(
    &workspace,
    "test-crate",
    r#"[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"
build = "build.rs"

[build-dependencies]
native-side-effect = { path = "../../vendor/native-side-effect" }
"#,
  )?;
  fs::write(workspace.path.join("crates/test-crate/build.rs"), "fn main() {}\n")?;
  workspace.commit("Add build dependency with a native links contract")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--explain"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    output.status.code() != Some(2),
    "analysis failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
  );
  assert!(
    !stdout.contains("Remove native-side-effect"),
    "links dependencies can have required native side effects\n{stdout}"
  );
  assert!(
    stdout.contains("native links contract"),
    "preservation must name the exact safety boundary\nstdout:\n{stdout}\nstderr:\n{stderr}"
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
  let cache: serde_json::Value = serde_json::from_str(&fs::read_to_string(&cache_file)?)?;
  assert_eq!(
    cache["version"], 6,
    "per-compilation-unit evidence cache must use schema version 6"
  );
  let entries = cache["entries"].as_object().expect("cache entries object");
  let entry = entries.values().next().expect("at least one cache entry");
  assert!(
    entry["key"]["package_id"].as_str().is_some(),
    "cache key must retain Cargo package identity: {entry}"
  );
  assert_eq!(entry["key"]["target"], "default");
  assert!(
    entry["key"]["features"].as_str().is_some(),
    "cache key must retain Cargo feature selection: {entry}"
  );
  assert!(entry["key"]["cargo_version"].is_string(), "{entry}");
  assert!(
    entry["evidence"]["compiled_units"].is_array(),
    "cache must persist typed compilation-unit evidence: {entry}"
  );

  let warm = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "-f", "json"])?;
  let warm_json: serde_json::Value = serde_json::from_slice(&warm.stdout)?;
  let warm_cache = warm_json["evidence_cache"]
    .as_array()
    .and_then(|entries| entries.iter().find(|entry| entry["member"] == "test-crate"))
    .expect("unused dependency cache telemetry");
  assert!(
    warm_cache["hits"].as_u64().is_some_and(|hits| hits > 0),
    "warm analysis must expose exact cache reuse\n{}",
    serde_json::to_string_pretty(&warm_json)?
  );
  assert_eq!(warm_cache["misses"], 0);

  fs::write(
    workspace.path.join("crates/test-crate/src/lib.rs"),
    "pub fn changed_source_without_dependency_use() {}\n",
  )?;
  let invalidated = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "-f", "json"])?;
  let invalidated_json: serde_json::Value = serde_json::from_slice(&invalidated.stdout)?;
  let invalidated_cache = invalidated_json["evidence_cache"]
    .as_array()
    .and_then(|entries| entries.iter().find(|entry| entry["member"] == "test-crate"))
    .expect("invalidated dependency cache telemetry");
  assert!(
    invalidated_cache["miss_reasons"]
      .as_array()
      .is_some_and(|reasons| reasons.iter().any(|reason| reason
        .as_str()
        .is_some_and(|value| value.starts_with("source_changed=")))),
    "source invalidation must be explicit\n{}",
    serde_json::to_string_pretty(&invalidated_json)?
  );

  Ok(())
}

#[test]
fn test_unused_detection_checks_only_members_requiring_source_evidence() -> Result<()> {
  let workspace = create_workspace_with_unused_detection()?;

  add_crate_with_manifest(
    &workspace,
    "dependency",
    r#"[package]
name = "dependency"
version = "0.1.0"
edition = "2021"
"#,
  )?;
  add_crate_with_manifest(
    &workspace,
    "graph-only",
    r#"[package]
name = "graph-only"
version = "0.1.0"
edition = "2021"

[dependencies]
dependency = { path = "../dependency" }
"#,
  )?;
  add_crate_with_manifest(
    &workspace,
    "source-check",
    r#"[package]
name = "source-check"
version = "0.1.0"
edition = "2021"

[dependencies]
log = "0.4"
"#,
  )?;

  workspace.commit("Add graph-only and source-check members")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("(1 package)"),
    "only source-check should require compiler diagnostics\nstderr:\n{}",
    stderr
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
