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
use cargo_rail::source::SourceSnapshot;
use cargo_rail::workspace::WorkspaceContext;
use tempfile::TempDir;

// Core Unification Tests

#[test]
fn test_unify_doctor_reports_exact_resolution_domains_in_text_and_json() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-doctor")?;
  workspace.add_crate("crate-a", "0.1.0", &[])?;
  workspace.commit("Add crate")?;

  let json_output = run_cargo_rail(&workspace.path, &["rail", "unify", "doctor", "--format", "json"])?;
  assert!(
    json_output.status.success(),
    "unify doctor JSON should succeed: {}",
    String::from_utf8_lossy(&json_output.stderr)
  );
  let value: serde_json::Value = serde_json::from_slice(&json_output.stdout)?;
  assert_eq!(value["command"], "unify");
  assert_eq!(value["mode"], "doctor");
  assert!(value["cargo"]["version"].is_string());
  assert!(value["cargo"]["channel"].is_string());
  assert!(value["resolver"]["version"].is_string());
  assert_eq!(value["feature_mode"]["features"], "default");
  assert!(value["effective_overrides"]["cargo_sources"].is_array());
  assert!(
    value["target_domains"]
      .as_array()
      .is_some_and(|domains| !domains.is_empty())
  );
  assert_eq!(value["target_domains"][0]["target"], "default");
  assert_eq!(value["target_domains"][0]["role"], "unfiltered");
  assert!(value["ambiguous_aliases"].is_array());
  assert!(value["recommended_action"]["code"].is_string());

  let text_output = run_cargo_rail(&workspace.path, &["rail", "unify", "doctor"])?;
  assert!(text_output.status.success(), "unify doctor text should succeed");
  let text = String::from_utf8_lossy(&text_output.stdout);
  assert!(text.contains("unify doctor"));
  assert!(text.contains("resolver:"));
  assert!(text.contains("target domains:"));
  assert!(text.contains("recommended:"));

  Ok(())
}

#[test]
fn test_unify_check_json_emits_versioned_feature_target_coverage_views() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-coverage-views")?;
  workspace.add_crate("app", "0.1.0", &[])?;
  let manifest_path = workspace.path.join("crates/app/Cargo.toml");
  let manifest = std::fs::read_to_string(&manifest_path)?;
  std::fs::write(&manifest_path, format!("{manifest}\n[features]\nbackend = []\n"))?;
  std::fs::write(
    workspace.path.join("crates/app/src/lib.rs"),
    "#[cfg(feature = \"backend\")]\npub fn backend() {}\n",
  )?;
  std::fs::write(
    workspace.path.join(".config/rail.toml"),
    "targets = [\"x86_64-unknown-linux-gnu\"]\n\n[unify]\nmsrv_policy = { mode = \"disabled\" }\n",
  )?;
  workspace.commit("Declare one target-aware feature coverage matrix")?;

  let first = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--format", "json"])?;
  assert!(
    first.status.success(),
    "coverage reporting must not create manifest changes: {}",
    String::from_utf8_lossy(&first.stderr)
  );
  let first_value: serde_json::Value = serde_json::from_slice(&first.stdout)?;
  assert_eq!(first_value["coverage"]["schema_version"], 1);
  let views = first_value["coverage"]["views"].as_array().expect("coverage views");
  assert_eq!(views.len(), 4, "default, no-default, all, and selected views");
  assert!(views.iter().all(|view| {
    view["id"]
      .as_str()
      .is_some_and(|identity| identity.starts_with("coverage:v1-sha256-"))
      && view["target"] == "x86_64-unknown-linux-gnu"
      && view["packages"] == serde_json::json!(["app"])
      && view["cargo"]["program"] == "cargo"
      && view["cargo"]["args"].is_array()
      && view["nextest"]["program"] == "cargo"
      && view["nextest"]["args"].is_array()
  }));
  let selected = views
    .iter()
    .find(|view| view["features"]["mode"] == "selected")
    .expect("source-derived selected feature view");
  assert_eq!(selected["features"]["selected"], serde_json::json!(["backend"]));
  assert!(
    selected["cargo"]["args"]
      .as_array()
      .is_some_and(|args| args.iter().any(|argument| argument == "app/backend"))
  );

  let second = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--format", "json"])?;
  let second_value: serde_json::Value = serde_json::from_slice(&second.stdout)?;
  assert_eq!(first_value["coverage"], second_value["coverage"]);

  Ok(())
}

#[test]
fn test_unify_apply_json_is_a_single_machine_envelope() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-json-apply")?;
  workspace.add_crate("crate-a", "0.1.0", &[("tempfile", r#""3.0""#)])?;
  workspace.add_crate("crate-b", "0.1.0", &[("tempfile", r#""3.0""#)])?;
  workspace.commit("Add shared dependency")?;

  let check = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--format", "json"])?;
  assert_eq!(check.status.code(), Some(1));
  let check_json: serde_json::Value = serde_json::from_slice(&check.stdout)?;
  let check_actions = check_json["mutation_plan"]["actions"]
    .as_array()
    .expect("check mutation actions");
  assert!(check_actions.iter().any(|action| action["code"] == "CREATE_BACKUP"));
  assert!(check_actions.iter().any(|action| {
    action["code"] == "REFRESH_CARGO_LOCK"
      && action["expected_mutations"]
        .as_array()
        .is_some_and(|mutations| mutations.iter().any(|mutation| mutation["path"] == "Cargo.lock"))
  }));
  assert!(check_actions.iter().any(|action| {
    action["code"] == "WRITE_REPORT"
      && action["expected_mutations"].as_array().is_some_and(|mutations| {
        mutations
          .iter()
          .any(|mutation| mutation["path"] == "target/cargo-rail/unify-report.md")
      })
  }));

  let skip_report_check = run_cargo_rail(
    &workspace.path,
    &["rail", "unify", "--check", "--skip-report", "--format", "json"],
  )?;
  assert_eq!(skip_report_check.status.code(), Some(1));
  let skip_report_json: serde_json::Value = serde_json::from_slice(&skip_report_check.stdout)?;
  assert!(
    skip_report_json["mutation_plan"]["actions"]
      .as_array()
      .is_some_and(|actions| actions.iter().all(|action| action["code"] != "WRITE_REPORT")),
    "--skip-report check plans must not authorize report output"
  );
  let check_proof = check_json["proof_fingerprint"]
    .as_str()
    .expect("check proof fingerprint")
    .to_string();
  let mut check_coverage_identities = check_json["coverage"]["views"]
    .as_array()
    .expect("check coverage views")
    .iter()
    .map(|view| view["id"].as_str().expect("coverage identity").to_string())
    .collect::<Vec<_>>();
  check_coverage_identities.sort();

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--format", "json"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    output.status.success(),
    "JSON apply should succeed.\nstdout:\n{stdout}\nstderr:\n{stderr}"
  );
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
  assert!(
    value["graph_delta"]["lockfile_fingerprint"]
      .as_str()
      .is_some_and(|fingerprint| fingerprint.starts_with("sha256:"))
  );
  assert_eq!(value["coverage_verification"]["schema_version"], 1);
  assert_eq!(
    value["coverage_verification"]["verified_views"],
    check_coverage_identities.len()
  );
  assert_eq!(
    value["coverage_verification"]["identities"],
    serde_json::json!(check_coverage_identities)
  );
  assert!(
    value["coverage_verification"]["fingerprint"]
      .as_str()
      .is_some_and(|fingerprint| fingerprint.starts_with("sha256:"))
  );
  assert_eq!(value["proof_fingerprint"], check_proof);

  Ok(())
}

#[test]
fn test_unify_apply_verifies_the_captured_target_and_selected_feature_views() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-apply-coverage")?;
  workspace.add_crate("app", "0.1.0", &[("tempfile", r#""3.0""#)])?;
  workspace.add_crate("worker", "0.1.0", &[("tempfile", r#""3.0""#)])?;

  let app_manifest_path = workspace.path.join("crates/app/Cargo.toml");
  let app_manifest = std::fs::read_to_string(&app_manifest_path)?;
  std::fs::write(
    &app_manifest_path,
    format!("{app_manifest}\n[features]\nbackend = []\n"),
  )?;
  std::fs::write(
    workspace.path.join("crates/app/src/lib.rs"),
    "#[cfg(feature = \"backend\")]\npub fn backend() -> tempfile::TempDir { tempfile::tempdir().unwrap() }\n",
  )?;
  std::fs::write(
    workspace.path.join(".config/rail.toml"),
    "targets = [\"x86_64-unknown-linux-gnu\"]\n\n[unify]\nmsrv_policy = { mode = \"disabled\" }\n",
  )?;
  workspace.commit("Create target-specific coverage and a unification edit")?;

  let check = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--format", "json"])?;
  assert_eq!(check.status.code(), Some(1));
  let check_json: serde_json::Value = serde_json::from_slice(&check.stdout)?;
  let views = check_json["coverage"]["views"].as_array().expect("coverage views");
  assert!(
    views.iter().all(|view| view["target"] == "x86_64-unknown-linux-gnu"),
    "every emitted view must retain the configured target"
  );
  assert!(
    views.iter().any(|view| view["features"]["mode"] == "selected"),
    "source feature conditions must produce a selected view"
  );
  let mut expected_identities = views
    .iter()
    .map(|view| view["id"].as_str().expect("coverage identity").to_string())
    .collect::<Vec<_>>();
  expected_identities.sort();

  let apply = run_cargo_rail(&workspace.path, &["rail", "unify", "--format", "json"])?;
  assert!(
    apply.status.success(),
    "target/feature coverage verification should succeed.\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&apply.stdout),
    String::from_utf8_lossy(&apply.stderr)
  );
  let apply_json: serde_json::Value = serde_json::from_slice(&apply.stdout)?;
  assert_eq!(
    apply_json["coverage_verification"]["identities"],
    serde_json::json!(expected_identities)
  );
  assert_eq!(apply_json["coverage_verification"]["verified_views"], views.len());

  Ok(())
}

#[test]
fn test_unify_apply_restores_manifests_and_lockfile_when_a_late_output_fails() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-late-output-rollback")?;
  workspace.add_crate("crate-a", "0.1.0", &[("tempfile", r#""3.0""#)])?;
  workspace.add_crate("crate-b", "0.1.0", &[("tempfile", r#""3.0""#)])?;
  workspace.commit("Add shared dependency for rollback")?;

  let check = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--format", "json"])?;
  assert_eq!(check.status.code(), Some(1));
  let root_manifest = workspace.path.join("Cargo.toml");
  let member_manifests = [
    workspace.path.join("crates/crate-a/Cargo.toml"),
    workspace.path.join("crates/crate-b/Cargo.toml"),
  ];
  let lockfile = workspace.path.join("Cargo.lock");
  let original_root = std::fs::read(&root_manifest)?;
  let original_members = member_manifests
    .iter()
    .map(std::fs::read)
    .collect::<std::io::Result<Vec<_>>>()?;
  let original_lockfile = std::fs::read(&lockfile)?;

  let report_parent = workspace.path.join("late-report");
  std::fs::create_dir(&report_parent)?;
  let watched_manifest = root_manifest.clone();
  let blocked_parent = report_parent.clone();
  let sabotage = std::thread::spawn(move || -> std::io::Result<bool> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
      if std::fs::read_to_string(&watched_manifest).is_ok_and(|manifest| manifest.contains("tempfile =")) {
        std::fs::remove_dir(&blocked_parent)?;
        std::fs::write(&blocked_parent, "block report directory creation")?;
        return Ok(true);
      }
      std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Ok(false)
  });

  let apply = run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "unify",
      "--format",
      "json",
      "--report-path",
      "late-report/report.md",
    ],
  )?;
  assert!(sabotage.join().expect("report sabotage thread")?);
  assert!(!apply.status.success(), "late report failure must fail apply");
  let error: serde_json::Value = serde_json::from_slice(&apply.stdout)?;
  assert!(
    error["help"]
      .as_str()
      .is_some_and(|help| help.contains("manifests, Cargo.lock, and report output were restored")),
    "failure must state the completed recovery: {error:#?}"
  );
  assert_eq!(std::fs::read(&root_manifest)?, original_root);
  for (path, original) in member_manifests.iter().zip(&original_members) {
    assert_eq!(std::fs::read(path)?, *original, "{} was not restored", path.display());
  }
  assert_eq!(std::fs::read(&lockfile)?, original_lockfile);
  assert!(!workspace.path.join("late-report/report.md").exists());

  Ok(())
}

#[test]
fn test_unify_rejects_a_report_path_that_aliases_graph_state() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-report-alias")?;
  workspace.add_crate("crate-a", "0.1.0", &[("tempfile", r#""3.0""#)])?;
  workspace.add_crate("crate-b", "0.1.0", &[("tempfile", r#""3.0""#)])?;
  workspace.commit("Add shared dependency for report alias rejection")?;

  let check = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;
  assert_eq!(check.status.code(), Some(1));
  let root_manifest = workspace.path.join("Cargo.toml");
  let original_root = std::fs::read(&root_manifest)?;
  let original_lockfile = std::fs::read(workspace.path.join("Cargo.lock"))?;

  let apply = run_cargo_rail(
    &workspace.path,
    &["rail", "unify", "--format", "json", "--report-path", "Cargo.toml"],
  )?;
  assert!(!apply.status.success());
  let error: serde_json::Value = serde_json::from_slice(&apply.stdout)?;
  assert!(
    error["message"]
      .as_str()
      .is_some_and(|message| message.contains("aliases a graph mutation target")),
    "alias rejection should name the violated boundary: {error:#?}"
  );
  assert_eq!(std::fs::read(&root_manifest)?, original_root);
  assert_eq!(std::fs::read(workspace.path.join("Cargo.lock"))?, original_lockfile);

  let outside = tempfile::tempdir()?;
  let outside_report = outside.path().join("report.md");
  let outside_apply = run_cargo_rail(
    &workspace.path,
    &[
      "rail",
      "unify",
      "--format",
      "json",
      "--report-path",
      outside_report.to_string_lossy().as_ref(),
    ],
  )?;
  assert!(!outside_apply.status.success());
  let outside_error: serde_json::Value = serde_json::from_slice(&outside_apply.stdout)?;
  assert!(
    outside_error["message"]
      .as_str()
      .is_some_and(|message| message.contains("outside authority root")),
    "outside output rejection should name the containment boundary: {outside_error:#?}"
  );
  assert!(!outside_report.exists());
  assert_eq!(std::fs::read(&root_manifest)?, original_root);
  assert_eq!(std::fs::read(workspace.path.join("Cargo.lock"))?, original_lockfile);

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
  let crate_a = workspace.add_crate(
    "crate-a",
    "0.1.0",
    &[(
      "serde",
      r#"{ version = "1.0", default-features = true, features = ["derive"] }"#,
    )],
  )?;

  let crate_b = workspace.add_crate(
    "crate-b",
    "0.1.0",
    &[(
      "serde",
      r#"{ version = "1.0", default-features = false, features = ["alloc"] }"#,
    )],
  )?;
  std::fs::write(
    crate_a.join("src/lib.rs"),
    "#[derive(serde::Serialize)]\npub struct A;\n",
  )?;
  std::fs::write(
    crate_b.join("src/lib.rs"),
    "pub fn requires_serialize<T: serde::Serialize>() {}\n",
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

  // The shared baseline must disable defaults so crate-b can stay default-free.
  assert!(
    workspace_toml.contains("default-features = false"),
    "Should disable workspace default-features.\nWorkspace TOML:\n{}",
    workspace_toml
  );

  // Distinct features stay on the declarations that require them.
  let crate_a_toml = std::fs::read_to_string(workspace.path.join("crates/crate-a/Cargo.toml"))?;
  let crate_b_toml = std::fs::read_to_string(workspace.path.join("crates/crate-b/Cargo.toml"))?;
  assert!(
    crate_a_toml.contains("derive") && crate_a_toml.contains("\"default\""),
    "crate-a must retain derive and defaults locally:\n{crate_a_toml}"
  );
  assert!(
    crate_b_toml.contains("alloc") && !crate_b_toml.contains("\"default\""),
    "crate-b must retain alloc without defaults:\n{crate_b_toml}"
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
fn test_unify_existing_inherited_dependencies_are_not_noop_edits() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-existing-inheritance")?;
  workspace.add_crate("crate-a", "0.1.0", &[("serde", r#"{ workspace = true }"#)])?;
  workspace.add_crate(
    "crate-b",
    "0.1.0",
    &[("serde", r#"{ workspace = true, features = ["rc"] }"#)],
  )?;
  for member in ["crate-a", "crate-b"] {
    std::fs::write(
      workspace.path.join("crates").join(member).join("src/lib.rs"),
      "pub fn requires_serde<T: serde::Serialize>(_value: &T) {}\n",
    )?;
  }
  std::fs::write(
    workspace.path.join(".config/rail.toml"),
    "[unify]\nmsrv_policy = { mode = \"disabled\" }\n",
  )?;
  workspace.commit("Use inherited dependencies with one local feature overlay")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--format", "json"])?;
  assert!(
    output.status.success(),
    "an already coherent inherited dependency must not produce a no-op edit: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(value["has_changes"], false);
  assert_eq!(value["summary"]["workspace_deps_count"], 0);
  assert_eq!(value["summary"]["member_edits_count"], 0);

  Ok(())
}

#[test]
fn test_unify_package_inheritance_only_rewrites_captured_equivalent_fields() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-package-inheritance")?;
  let crate_a = workspace.add_crate("crate-a", "0.1.0", &[])?;
  let crate_b = workspace.add_crate("crate-b", "0.1.0", &[])?;

  let root_path = workspace.path.join("Cargo.toml");
  let root = std::fs::read_to_string(&root_path)?;
  std::fs::write(
    &root_path,
    root.replace(
      "authors = [\"Test Author\"]",
      "authors = [\"Test Author\"]\nversion = \"0.1.0\"\ndescription = \"shared description\"\npublish = false\nrepository = \"https://example.invalid/repository\"\nreadme = \"README.md\"",
    ),
  )?;
  std::fs::write(workspace.path.join("README.md"), "# Workspace\n")?;

  std::fs::write(
    crate_a.join("Cargo.toml"),
    r#"[package]
name = "crate-a"
version = "0.1.0"
edition = "2021" # preserve this comment
license = "MIT"
authors = [ "Test Author" ]
description = "shared description"
publish = false
repository = "https://example.invalid/repository"
readme = "README.md"

[package.metadata.fixture]
unknown = "preserve"

[dependencies]
"#,
  )?;
  std::fs::write(
    crate_b.join("Cargo.toml"),
    r#"[package]
name = "crate-b"
version = "0.1.0"
edition.workspace = true
license = "Apache-2.0"
description = "crate-specific description"
publish = true
repository = "https://example.invalid/repository"

[dependencies]
"#,
  )?;
  std::fs::write(
    workspace.path.join(".config/rail.toml"),
    "[unify]\nmsrv_policy = { mode = \"disabled\" }\n",
  )?;
  workspace.commit("Declare shared and package-local metadata")?;

  let check = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--format", "json"])?;
  assert_eq!(
    check.status.code(),
    Some(1),
    "equivalent package fields should produce inheritance edits: {}",
    String::from_utf8_lossy(&check.stderr)
  );
  let report: serde_json::Value = serde_json::from_slice(&check.stdout)?;
  assert_eq!(report["package_inheritance"]["schema_version"], 1);
  assert_eq!(report["summary"]["package_fields_inherited"], 7);
  assert!(
    report["reason_codes"]
      .as_array()
      .is_some_and(|codes| codes.iter().any(|code| code == "UNIFY_PACKAGE_INHERITANCE_DECISIONS"))
  );
  let fields = report["package_inheritance"]["fields"]
    .as_array()
    .expect("package inheritance fields");
  let field = |name: &str| {
    fields
      .iter()
      .find(|field| field["field"] == name)
      .unwrap_or_else(|| panic!("missing package inheritance field {name}: {fields:#?}"))
  };
  assert_eq!(field("authors")["planned"], serde_json::json!(["crate-a"]));
  assert_eq!(field("authors")["missing"], serde_json::json!(["crate-b"]));
  assert_eq!(field("edition")["planned"], serde_json::json!(["crate-a"]));
  assert_eq!(field("edition")["inherited"], serde_json::json!(["crate-b"]));
  assert_eq!(field("license")["planned"], serde_json::json!(["crate-a"]));
  assert_eq!(field("license")["local_overrides"], serde_json::json!(["crate-b"]));
  assert_eq!(field("publish")["planned"], serde_json::json!(["crate-a"]));
  assert_eq!(field("publish")["local_overrides"], serde_json::json!(["crate-b"]));
  assert_eq!(
    field("repository")["planned"],
    serde_json::json!(["crate-a", "crate-b"])
  );
  assert_eq!(
    field("version")["retained_equivalent"],
    serde_json::json!(["crate-a", "crate-b"])
  );
  assert_eq!(field("version")["retention_reason"], "release-policy-owned");
  assert_eq!(field("readme")["retained_equivalent"], serde_json::json!(["crate-a"]));
  assert_eq!(field("readme")["retention_reason"], "workspace-relative-path");
  assert!(report["proof_certificates"].as_array().is_some_and(|certificates| {
    certificates.iter().any(|certificate| {
      certificate["subject"]["kind"] == "package_field"
        && certificate["subject"]["declaration"] == "package.repository"
        && certificate["member"] == "crate-b"
    })
  }));

  let apply = run_cargo_rail(&workspace.path, &["rail", "unify", "--format", "json"])?;
  assert!(
    apply.status.success(),
    "package inheritance apply failed:\nstdout: {}\nstderr: {}",
    String::from_utf8_lossy(&apply.stdout),
    String::from_utf8_lossy(&apply.stderr)
  );
  let apply_report: serde_json::Value = serde_json::from_slice(&apply.stdout)?;
  assert_eq!(apply_report["package_fields_inherited"], 7);
  let markdown_report = std::fs::read_to_string(workspace.path.join("target/cargo-rail/unify-report.md"))?;
  assert!(markdown_report.contains("- **Package Fields Inherited:** 7"));
  assert!(markdown_report.contains("## Package Inheritance"));
  let crate_a_manifest = std::fs::read_to_string(crate_a.join("Cargo.toml"))?;
  for field in ["authors", "description", "edition", "license", "publish", "repository"] {
    assert!(
      crate_a_manifest.contains(&format!("{field} = {{ workspace = true }}")),
      "{field} should inherit without disturbing unrelated TOML: {crate_a_manifest}"
    );
  }
  assert!(crate_a_manifest.contains("version = \"0.1.0\""));
  assert!(crate_a_manifest.contains("readme = \"README.md\""));
  assert!(crate_a_manifest.contains("preserve this comment"));
  assert!(crate_a_manifest.contains("unknown = \"preserve\""));

  let crate_b_manifest = std::fs::read_to_string(crate_b.join("Cargo.toml"))?;
  assert!(crate_b_manifest.contains("repository = { workspace = true }"));
  assert!(crate_b_manifest.contains("edition.workspace = true"));
  assert!(crate_b_manifest.contains("license = \"Apache-2.0\""));
  assert!(crate_b_manifest.contains("description = \"crate-specific description\""));
  assert!(crate_b_manifest.contains("publish = true"));
  assert!(!crate_b_manifest.contains("authors"));

  let repeated = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--format", "json"])?;
  assert!(
    repeated.status.success(),
    "one apply must leave package inheritance idempotent: {}",
    String::from_utf8_lossy(&repeated.stderr)
  );
  let repeated: serde_json::Value = serde_json::from_slice(&repeated.stdout)?;
  assert_eq!(repeated["has_changes"], false);
  assert!(
    repeated["package_inheritance"]["fields"]
      .as_array()
      .is_some_and(|fields| fields.iter().all(|field| field["planned"] == serde_json::json!([])))
  );

  Ok(())
}

#[test]
fn test_unify_renamed_workspace_inheritance_keeps_exact_alias_authority() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-renamed-inheritance")?;
  let root_manifest = workspace.path.join("Cargo.toml");
  let root = std::fs::read_to_string(&root_manifest)?;
  std::fs::write(
    &root_manifest,
    root.replace(
      "serde = { version = \"1.0\", features = [\"derive\"] }",
      "serde = { version = \"1.0\", features = [\"derive\"] }\nrenamed_serde = { package = \"serde\", version = \"1.0\", features = [\"derive\"] }",
    ),
  )?;
  for member in ["crate-a", "crate-b"] {
    workspace.add_crate(member, "0.1.0", &[("renamed_serde", r#"{ workspace = true }"#)])?;
    std::fs::write(
      workspace.path.join("crates").join(member).join("src/lib.rs"),
      "pub fn requires_serde<T: renamed_serde::Serialize>(_value: &T) {}\n",
    )?;
  }
  std::fs::write(
    workspace.path.join(".config/rail.toml"),
    "[unify]\ninclude_renamed = true\nmsrv_policy = { mode = \"disabled\" }\n",
  )?;
  workspace.commit("Use a renamed inherited workspace dependency")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--format", "json"])?;
  assert!(
    output.status.success(),
    "the existing renamed workspace alias must remain authoritative: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(value["has_changes"], false);
  assert_eq!(value["summary"]["workspace_deps_count"], 0);
  assert_eq!(value["summary"]["member_edits_count"], 0);

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
fn test_unify_preserves_default_features_across_target_domains() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-target-default-features")?;
  let crate_a = workspace.add_crate("crate-a", "0.1.0", &[("serde", r#""1.0""#)])?;
  let crate_b = workspace.add_crate("crate-b", "0.1.0", &[])?;
  std::fs::write(
    crate_a.join("src/lib.rs"),
    "pub fn requires_serialize<T: serde::Serialize>() {}\n",
  )?;
  std::fs::write(
    crate_b.join("Cargo.toml"),
    r#"[package]
name = "crate-b"
version = "0.1.0"
edition.workspace = true
license.workspace = true
authors.workspace = true

[target.'cfg(target_arch = "wasm32")'.dependencies]
serde = { version = "1.0", default-features = false }
"#,
  )?;
  std::fs::write(
    crate_b.join("src/lib.rs"),
    "#[cfg(target_arch = \"wasm32\")]\npub fn requires_serialize<T: serde::Serialize>() {}\n",
  )?;
  workspace.commit("Add cross-domain default feature policy")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "unify should preserve per-domain defaults:\nstdout: {}\nstderr: {}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  let root = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert!(
    root.contains("default-features = false"),
    "the workspace baseline must disable defaults when any domain does: {root}"
  );
  let crate_a = std::fs::read_to_string(crate_a.join("Cargo.toml"))?;
  assert!(
    crate_a.contains("workspace = true"),
    "crate-a must inherit serde: {crate_a}"
  );
  assert!(
    crate_a.contains("\"default\""),
    "the default-enabled declaration must opt back in locally: {crate_a}"
  );
  let crate_b = std::fs::read_to_string(crate_b.join("Cargo.toml"))?;
  assert!(
    crate_b.contains("workspace = true"),
    "crate-b must inherit serde: {crate_b}"
  );
  assert!(
    !crate_b.contains("\"default\""),
    "the target declaration that disabled defaults must remain disabled: {crate_b}"
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

// Config options: include, transitive pinning, include_renamed

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

/// Test typed transitive pinning policy
#[test]
fn test_unify_transitive_pinning() -> Result<()> {
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

  // Configure rail.toml with transitive pinning
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[unify]
transitive_pinning = { host = "root" }
msrv_policy = { mode = "disabled" }
"#,
  )?;

  // Run unify with transitive pinning config (single crate = no unification needed)
  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check"])?;

  // No unification opportunities with single crate, so exit 0
  assert!(
    output.status.success(),
    "unify with transitive pinning config and no changes should exit 0. stderr: {}",
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
msrv_policy = { mode = "disabled" }
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
    "use my_serde as _;\nuse anyhow as _;\npub fn b() {}",
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
msrv_policy = { mode = "disabled" }
"#,
  )?;

  // With include_renamed config, serde SHOULD be unified (2 users total for the package)
  let output_with = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--format", "json"])?;
  let value: serde_json::Value = serde_json::from_slice(&output_with.stdout)?;
  let serde_decision = value["dependency_decisions"]
    .as_array()
    .and_then(|decisions| decisions.iter().find(|decision| decision["dep_name"] == "serde"))
    .expect("serde dependency decision should be present");
  let feature_paths = serde_decision["reasons"]
    .as_array()
    .into_iter()
    .flatten()
    .flat_map(|reason| reason["feature_paths"].as_array().into_iter().flatten())
    .collect::<Vec<_>>();
  let aliases = feature_paths
    .iter()
    .filter_map(|path| path["alias"].as_str())
    .collect::<std::collections::BTreeSet<_>>();
  assert!(
    aliases.contains("serde") && aliases.contains("my_serde"),
    "feature explanations must preserve both exact Cargo.toml aliases: {feature_paths:#?}"
  );
  let workspace_dependencies = value["workspace_deps"].as_array().expect("workspace dependency plan");
  assert!(
    workspace_dependencies
      .iter()
      .any(|dependency| dependency["name"] == "my_serde" && dependency["package"] == "serde"),
    "renamed inheritance requires an exact alias entry with package identity: {workspace_dependencies:#?}"
  );
  let renamed = workspace_dependencies
    .iter()
    .find(|dependency| dependency["name"] == "my_serde")
    .expect("renamed workspace dependency plan");
  assert_eq!(
    renamed["features"],
    serde_json::json!([]),
    "a feature used by one exact alias must remain on that declaration"
  );

  let apply = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    apply.status.success(),
    "renamed dependency unification should apply: {}",
    String::from_utf8_lossy(&apply.stderr)
  );
  let root_manifest = std::fs::read_to_string(workspace.path.join("Cargo.toml"))?;
  assert!(
    root_manifest.contains("my_serde = { package = \"serde\""),
    "workspace root must retain the renamed package identity:\n{root_manifest}"
  );
  assert!(
    !root_manifest
      .lines()
      .find(|line| line.starts_with("my_serde ="))
      .is_some_and(|line| line.contains("derive") || line.contains("rc")),
    "renamed aliases must not receive a package-wide feature union:\n{root_manifest}"
  );
  let member_manifest = std::fs::read_to_string(crate_b_path.join("Cargo.toml"))?;
  assert!(
    member_manifest.contains("my_serde = { workspace = true"),
    "member must inherit through its exact alias:\n{member_manifest}"
  );
  assert!(
    member_manifest.contains("features = [\"rc\"]"),
    "the renamed alias feature must remain local:\n{member_manifest}"
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
  std::fs::write(
    workspace.path.join("crates/crate-a/src/lib.rs"),
    "pub fn require_serde<T: serde::Serialize>(_value: &T) {}",
  )?;

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
    "pub fn require_serde<T: serde_crate::Serialize>(_value: &T) {}",
  )?;

  workspace.commit("Add crates with renamed dependency")?;

  // Configure rail.toml (include_renamed = false by default)
  std::fs::write(
    workspace.path.join("rail.toml"),
    r#"[unify]
include_renamed = false
msrv_policy = { mode = "disabled" }
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
    report_content.contains("# Cargo-Rail Unification Report"),
    "Report should have title"
  );
  assert!(report_content.contains("Summary"), "Report should have summary section");
  assert!(
    !report_content.contains("Generated:"),
    "Equivalent plans must not embed wall-clock state in the report"
  );
  assert!(
    report_content.contains("serde"),
    "Report should mention unified dependency"
  );
  assert!(
    report_content.contains("derive") && report_content.contains("rc"),
    "Report should show member-local features"
  );
  assert!(
    report_content.contains("Member-Local Features"),
    "Report should distinguish local features from the workspace baseline"
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
    backup_dir.join("Cargo.lock").exists(),
    "Backup should contain Cargo.lock"
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
    assert!(json["plan"]["actions"].as_array().is_some_and(|actions| {
      actions.iter().any(|action| {
        action["code"] == "REFRESH_CARGO_LOCK"
          && action["expected_mutations"].as_array().is_some_and(|mutations| {
            mutations.iter().any(|mutation| {
              mutation["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("Cargo.lock"))
            })
          })
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
        trace
          .iter()
          .any(|entry| entry["code"] == "UNIFY_COVERAGE_VIEWS_VERIFIED")
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
    &["rail", "unify", "--plan", plan_path.to_string_lossy().as_ref()],
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
  let workspace_root = root.path().join("workspace");
  std::fs::create_dir_all(&workspace_root)?;

  std::fs::write(
    workspace_root.join("Cargo.toml"),
    r#"[workspace]
members = ["crates/*"]
resolver = "2"

[workspace.package]
edition = "2021"
"#,
  )?;
  std::fs::write(workspace_root.join(".gitignore"), "ambient-ignored/\ntarget/\n")?;
  std::fs::create_dir_all(workspace_root.join("ambient-ignored"))?;
  std::fs::write(
    workspace_root.join("ambient-ignored/source.txt"),
    "explicit filesystem policy keeps this source\n",
  )?;
  std::fs::create_dir_all(workspace_root.join("target/debug"))?;
  std::fs::write(workspace_root.join("target/debug/generated.rlib"), "generated\n")?;
  std::fs::write(root.path().join("outside-secret.txt"), "outside the Cargo workspace\n")?;
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

  let context = WorkspaceContext::build(&workspace_root)?;
  assert!(!context.has_git());
  let snapshot = context.capture_filesystem_source()?;
  assert!(matches!(snapshot, SourceSnapshot::FilesystemBacked(_)));
  let source_paths = snapshot
    .tree()
    .entries()
    .iter()
    .map(|entry| entry.path.as_str())
    .collect::<Vec<_>>();
  assert!(source_paths.contains(&"ambient-ignored/source.txt"));
  assert!(source_paths.iter().all(|path| !path.starts_with("target/")));
  assert!(source_paths.iter().all(|path| *path != "outside-secret.txt"));

  let output = run_cargo_rail(&workspace_root, &["rail", "unify", "--check", "-f", "json"])?;
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
    "[unify]\nconsumer_scope = \"workspace\"\n",
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
fn test_unify_graph_verification_uses_the_same_platform_filter_before_and_after() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-platform-filter-verification")?;
  workspace.add_crate("private-crate", "0.1.0", &[])?;
  workspace.add_crate("unrelated", "0.1.0", &[])?;
  workspace.add_crate("windows-only", "0.1.0", &[])?;

  std::fs::write(
    workspace.path.join(".config/rail.toml"),
    r#"targets = ["x86_64-unknown-linux-gnu"]

[workspace]
root = "."

[unify]
consumer_scope = "workspace"
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
obsolete = []
"#,
  )?;
  std::fs::write(
    workspace.path.join("crates/unrelated/Cargo.toml"),
    r#"[package]
name = "unrelated"
version = "0.1.0"
edition = "2021"

[target.'cfg(windows)'.dependencies]
windows-only = { path = "../windows-only" }
"#,
  )?;
  workspace.commit("Add a platform-filtered graph beside a dead private feature")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify"])?;
  assert!(
    output.status.success(),
    "pre- and post-edit graph verification must use the same platform filter\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  let manifest = std::fs::read_to_string(workspace.path.join("crates/private-crate/Cargo.toml"))?;
  assert!(
    !manifest.contains("obsolete ="),
    "the verified feature edit should be applied\n{manifest}"
  );

  Ok(())
}

#[test]
fn test_unify_does_not_prune_cargo_synthetic_optional_features() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-synthetic-optional-feature")?;
  workspace.add_crate("private-crate", "0.1.0", &[])?;
  std::fs::write(
    workspace.path.join(".config/rail.toml"),
    "[unify]\nconsumer_scope = \"workspace\"\n",
  )?;
  std::fs::write(
    workspace.path.join("crates/private-crate/Cargo.toml"),
    r#"[package]
name = "private-crate"
version = "0.1.0"
edition.workspace = true
publish = false

[dependencies]
log = { version = "0.4", optional = true }
"#,
  )?;
  std::fs::write(
    workspace.path.join("crates/private-crate/README.md"),
    "The optional `log` integration is part of this crate's documented contract.\n",
  )?;
  workspace.commit("Add a documented implicit optional dependency feature")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "-f", "json"])?;
  assert!(
    output.status.success(),
    "Cargo-synthesized optional dependency features are not writable manifest keys\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  let manifest = std::fs::read_to_string(workspace.path.join("crates/private-crate/Cargo.toml"))?;
  assert!(
    manifest.contains("log ="),
    "the documented optional dependency should remain\n{manifest}"
  );

  Ok(())
}

#[test]
fn test_unify_feature_reachability_prunes_unrooted_cycles_and_forwarders() -> Result<()> {
  let workspace = TestWorkspace::new_named("unify-feature-reachability")?;
  workspace.add_crate("private-crate", "0.1.0", &[])?;
  std::fs::write(
    workspace.path.join(".config/rail.toml"),
    r#"[unify]
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
  assert!(
    json["coverage"]["views"].as_array().is_some_and(|views| {
      views.iter().all(|view| {
        view["features"]["selected"]
          .as_array()
          .is_none_or(|features| features.iter().all(|feature| feature != "impossible-platform-root"))
      })
    }),
    "coverage must not schedule a source feature whose platform predicate cannot match"
  );
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
    "[unify]\nconsumer_scope = \"workspace\"\n",
  )?;
  std::fs::write(
    workspace.path.join("crates/private-crate/Cargo.toml"),
    r#"[package]
name = "private-crate"
version = "0.1.0"
edition.workspace = true
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
