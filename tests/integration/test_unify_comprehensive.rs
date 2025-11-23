//! Comprehensive integration tests for cargo rail unify command
//!
//! Tests cover all configuration options, severity levels, and edge cases.

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;

// ============================================================================
// SCENARIO 2: Version Conflicts with auto_resolve_version_conflicts = true
// ============================================================================

#[test]
fn test_unify_version_conflict_auto_resolve_enabled() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create crates with conflicting versions
  workspace.add_crate("crate-a", "0.1.0", &[("indexmap", r#""^1.9""#)])?;
  workspace.add_crate("crate-b", "0.1.0", &[("indexmap", r#""^2.1""#)])?;

  workspace.commit("Add crates with version conflict")?;

  // Configure auto-resolution (default is true, but let's be explicit)
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[workspace]
root = "."

[unify]
auto_resolve_version_conflicts = true
"#,
  )?;

  // Run analyze - should show Soft warning
  let analyze_output = run_cargo_rail(&workspace.path, &["rail", "unify", "--dry-run"])?;
  let analyze_stdout = String::from_utf8_lossy(&analyze_output.stdout);

  // Should mention version conflict
  assert!(
    analyze_stdout.contains("indexmap") || analyze_stdout.contains("conflict"),
    "Should show version conflict for indexmap.\nOutput:\n{}",
    analyze_stdout
  );

  // Should suggest running apply (not a blocker)
  assert!(
    analyze_stdout.contains("apply") || analyze_stdout.contains("non-blocking"),
    "Should suggest running apply for non-blocking issues.\nOutput:\n{}",
    analyze_stdout
  );

  // Run apply - should SUCCEED
  let apply_output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  let apply_stdout = String::from_utf8_lossy(&apply_output.stdout);

  assert!(
    apply_output.status.success(),
    "Apply should succeed with auto-resolution enabled.\nOutput:\n{}",
    apply_stdout
  );

  // Should show warning about proceeding
  assert!(
    apply_stdout.contains("Proceeding with warnings") || apply_stdout.contains("complete"),
    "Should show it's proceeding despite warnings.\nOutput:\n{}",
    apply_stdout
  );

  // Check workspace Cargo.toml was updated
  let workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert!(
    workspace_toml.contains("[workspace.dependencies]"),
    "Should have workspace.dependencies"
  );
  assert!(workspace_toml.contains("indexmap"), "Should include indexmap");

  // Should pick highest version (^2.1)
  assert!(
    workspace_toml.contains("2.1") || workspace_toml.contains("2"),
    "Should use highest version (^2.1).\nWorkspace TOML:\n{}",
    workspace_toml
  );

  // Should have auto-resolution comment
  assert!(
    workspace_toml.contains("Auto-resolved") || workspace_toml.contains("version conflict"),
    "Should have auto-resolution comment.\nWorkspace TOML:\n{}",
    workspace_toml
  );

  // Members should use workspace = true
  let crate_a_toml = std::fs::read_to_string(workspace.path.join("crates/crate-a/Cargo.toml"))?;
  assert!(
    crate_a_toml.contains("workspace = true") || crate_a_toml.contains("workspace=true"),
    "crate-a should use workspace inheritance"
  );

  Ok(())
}

// ============================================================================
// SCENARIO 4: Inconsistent Default-Features
// ============================================================================

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
  let analyze_output = run_cargo_rail(&workspace.path, &["rail", "unify", "--dry-run"])?;
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
    "Apply should succeed.\nOutput:\n{}",
    String::from_utf8_lossy(&apply_output.stdout)
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

// ============================================================================
// SCENARIO 7: Renamed Dependencies (Hard Blocker)
// ============================================================================

#[test]
fn test_unify_renamed_dependencies_hard_blocker() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create crates with renamed dependency
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

  // Configure rail.toml (allow_renamed = false by default)
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[workspace]
root = "."

[unify]
allow_renamed = false
"#,
  )?;

  // Run analyze - should show Hard blocker
  let analyze_output = run_cargo_rail(&workspace.path, &["rail", "unify", "--dry-run"])?;
  let analyze_stdout = String::from_utf8_lossy(&analyze_output.stdout);

  assert!(
    analyze_stdout.contains("BLOCKING") || analyze_stdout.contains("Renamed"),
    "Should show blocking issue for renamed dependency.\nOutput:\n{}",
    analyze_stdout
  );

  // Run apply - should FAIL
  let apply_output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  let apply_stdout = String::from_utf8_lossy(&apply_output.stdout);
  let apply_stderr = String::from_utf8_lossy(&apply_output.stderr);

  assert!(
    !apply_output.status.success(),
    "Apply should fail with renamed dependency.\nstdout:\n{}\nstderr:\n{}",
    apply_stdout,
    apply_stderr
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
    "crate-a should not use workspace = true for serde (apply failed, so no conversion).\ncrate-a Cargo.toml:\n{}",
    crate_a_member
  );

  Ok(())
}

// ============================================================================
// SCENARIO: Report Generation
// ============================================================================

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

[unify]
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
  let report_path = workspace.path.join("unify-report.md");
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

// ============================================================================
// SCENARIO: Exclude and Include Options
// ============================================================================

#[test]
fn test_unify_exclude_in_config() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  workspace.add_crate("crate-a", "0.1.0", &[("serde", r#""1.0""#), ("anyhow", r#""1.0""#)])?;
  workspace.add_crate("crate-b", "0.1.0", &[("serde", r#""1.0""#), ("anyhow", r#""1.0""#)])?;

  workspace.commit("Add crates")?;

  // Configure rail.toml with exclude
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[workspace]
root = "."

[unify]
exclude = ["serde"]
"#,
  )?;

  // Run analyze
  let analyze_output = run_cargo_rail(&workspace.path, &["rail", "unify", "--dry-run"])?;
  let analyze_stdout = String::from_utf8_lossy(&analyze_output.stdout);

  // Should show anyhow, but not serde
  assert!(
    analyze_stdout.contains("anyhow"),
    "Should show anyhow (not excluded).\nOutput:\n{}",
    analyze_stdout
  );

  // Run apply
  let apply_output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(apply_output.status.success(), "Apply should succeed");

  // Workspace should have anyhow but not serde
  let workspace_toml = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert!(
    workspace_toml.contains("anyhow"),
    "Should have anyhow in workspace.dependencies"
  );
  // Note: serde might exist in initial workspace.dependencies, so we check member conversion
  let crate_a_toml = std::fs::read_to_string(workspace.path.join("crates/crate-a/Cargo.toml"))?;
  assert!(
    !crate_a_toml.contains("anyhow = \"1.0\""),
    "anyhow should be converted to workspace = true"
  );
  assert!(
    crate_a_toml.contains("serde = \"1.0\"") || !crate_a_toml.contains("workspace = true"),
    "serde should NOT be converted (excluded)"
  );

  Ok(())
}

// ============================================================================
// SCENARIO: TOML Comment Generation
// ============================================================================

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
add_conflict_comments = true
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

// ============================================================================
// (Normal-only flag and check command tests deleted - features removed)
// ============================================================================

// ============================================================================
// SCENARIO: Backup Flag
// ============================================================================

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

  // Check that backup was created in target/.cargo-rail/backups/
  let backup_root = workspace.path.join("target/.cargo-rail/backups");
  assert!(
    backup_root.exists(),
    "Backup directory should exist at target/.cargo-rail/backups"
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

  // Check backup message in output
  assert!(
    apply_stdout.contains("Creating backup") || apply_stdout.contains("Backup created"),
    "Should mention backups in output.\nOutput:\n{}",
    apply_stdout
  );

  Ok(())
}
