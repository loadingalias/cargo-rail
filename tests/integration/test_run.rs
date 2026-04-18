//! Integration tests for `cargo rail run` command
//!
//! Tests the smart test runner with change detection

use crate::helpers::{TestWorkspace, git, run_cargo_rail};
use anyhow::Result;

#[test]
fn test_runner_basic_change_detection() -> Result<()> {
  // Setup workspace with two crates
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.commit("Add lib-a and lib-b")?;

  // Create baseline
  git(&ws.path, &["branch", "baseline"])?;

  // Modify lib-a source
  ws.modify_file("lib-a", "src/lib.rs", "pub fn modified() -> u32 { 42 }")?;
  ws.commit("Modify lib-a")?;

  // Run test with change detection
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline"])?;
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(output.status.success(), "Run command should succeed");
  assert!(
    stderr.contains("testing") && stderr.contains("crates"),
    "Should invoke runner"
  );
  assert!(
    stderr.contains("lib-a") && stderr.contains("lib-b"),
    "Should include dependent crates"
  );

  Ok(())
}

#[test]
fn test_runner_no_changes() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  // Create baseline
  git(&ws.path, &["branch", "baseline"])?;

  // Run test with no changes
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should skip all tests
  assert!(
    stdout.contains("no test targets"),
    "Should skip tests when no changes. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_docs_only_change() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Modify only README
  ws.modify_file("lib-a", "README.md", "# Updated Documentation\n")?;
  ws.commit("Update README")?;

  // Run test
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Documentation-only changes might still trigger tests depending on implementation
  // The key is that it should be detected and handled appropriately
  assert!(
    output.status.success(),
    "Run command should succeed. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_ci_only_change_skips_tests() -> Result<()> {
  let ws = TestWorkspace::new_named("test-ci-only")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  git(&ws.path, &["branch", "baseline"])?;

  std::fs::create_dir_all(ws.path.join(".github/workflows"))?;
  std::fs::write(ws.path.join(".github/workflows/ci.yml"), "name: CI\n")?;
  ws.commit("ci change only")?;

  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "Run command should succeed");
  assert!(
    stdout.contains("no test targets"),
    "CI-only changes should not trigger crate test execution. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_rejects_infra_surface() -> Result<()> {
  let ws = TestWorkspace::new_named("run-reject-infra-surface")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  let output = run_cargo_rail(&ws.path, &["rail", "run", "--surface", "infra"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  let combined = format!("{stdout}\n{stderr}");

  assert!(!output.status.success(), "infra surface should be rejected");
  assert!(
    combined.contains("planner output") && combined.contains("build|test|bench|docs"),
    "expected planner-output rejection. Output:\n{}",
    combined
  );

  Ok(())
}

#[test]
fn test_runner_transitive_dependencies() -> Result<()> {
  // Setup: lib-a <- lib-b <- lib-c (chain)
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.add_crate("lib-c", "0.1.0", &[("lib-b", r#"{ path = "../lib-b" }"#)])?;
  ws.commit("Add dependency chain")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Modify lib-a (root of chain)
  ws.modify_file("lib-a", "src/lib.rs", "pub fn chain_changed() {}")?;
  ws.commit("Modify lib-a")?;

  // Run test
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline"])?;
  let stderr = String::from_utf8_lossy(&output.stderr);

  // All three should be tested (lib-a changed, lib-b and lib-c depend on it)
  assert!(
    stderr.contains("lib-a"),
    "Should test lib-a (directly changed). Output:\n{}",
    stderr
  );
  assert!(
    stderr.contains("lib-b"),
    "Should test lib-b (depends on lib-a). Output:\n{}",
    stderr
  );
  assert!(
    stderr.contains("lib-c"),
    "Should test lib-c (transitive dependent). Output:\n{}",
    stderr
  );

  Ok(())
}

#[test]
fn test_runner_isolated_change() -> Result<()> {
  // Setup: two independent crates
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add independent crates")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Modify only lib-a
  ws.modify_file("lib-a", "src/lib.rs", "pub fn isolated_change() {}")?;
  ws.commit("Modify lib-a only")?;

  // Run test
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline"])?;
  let stderr = String::from_utf8_lossy(&output.stderr);

  // Should test only lib-a, not lib-b
  assert!(
    stderr.contains("lib-a"),
    "Should test lib-a (changed). Output:\n{}",
    stderr
  );
  assert!(
    !stderr.contains("lib-b"),
    "Should NOT list lib-b as affected. Output:\n{}",
    stderr
  );

  Ok(())
}

#[test]
fn test_runner_with_explain() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  git(&ws.path, &["branch", "baseline"])?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn explained() {}")?;
  ws.commit("Modify lib-a")?;

  // Run with --explain flag
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline", "--explain"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show detailed explanation
  assert!(
    stdout.contains("surfaces:") || stdout.contains("why:") || stdout.contains("explain:"),
    "Should show detailed explanation with --explain. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_auto_detect_base_ref() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  // Create a base branch if it does not exist.
  let existing_base_branch = git(&ws.path, &["branch", "--list", "base-branch"])?;
  if String::from_utf8_lossy(&existing_base_branch.stdout).trim().is_empty() {
    git(&ws.path, &["branch", "base-branch"])?;
  }
  git(&ws.path, &["checkout", "-b", "feature-branch"])?;

  ws.modify_file(
    "lib-a",
    "src/lib.rs",
    r#"
    pub fn feature_work() {}
    #[cfg(test)]
    mod tests {
        #[test]
        fn test_feature_work() {
            super::feature_work();
        }
    }
    "#,
  )?;
  ws.commit("Feature work")?;

  // Run without --since (should auto-detect base ref or use HEAD)
  let output = run_cargo_rail(&ws.path, &["rail", "run"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should successfully run (whether it detects changes or not is okay)
  assert!(
    output.status.success(),
    "Should successfully handle auto-detect. Output:\n{}\nStderr:\n{}",
    stdout,
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

#[test]
fn test_runner_config_file_changes() -> Result<()> {
  // Test that Cargo.toml changes are detected
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Modify Cargo.toml (add a comment or metadata)
  let cargo_toml = std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?;
  std::fs::write(
    ws.path.join("crates/lib-a/Cargo.toml"),
    format!("# Modified\n{}", cargo_toml),
  )?;
  ws.commit("Modify Cargo.toml")?;

  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline"])?;
  let stderr = String::from_utf8_lossy(&output.stderr);

  // Config changes should trigger testing
  assert!(
    stderr.contains("lib-a"),
    "Cargo.toml changes should trigger testing. Output:\n{}",
    stderr
  );

  Ok(())
}

#[test]
fn test_runner_test_file_changes() -> Result<()> {
  // Test that test file changes are detected
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Add an integration test
  std::fs::create_dir_all(ws.path.join("crates/lib-a/tests"))?;
  std::fs::write(
    ws.path.join("crates/lib-a/tests/integration_test.rs"),
    "#[test]\nfn new_test() { assert!(true); }",
  )?;
  ws.commit("Add integration test")?;

  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline"])?;
  let stderr = String::from_utf8_lossy(&output.stderr);

  // Test file changes should trigger testing
  assert!(
    stderr.contains("lib-a"),
    "Test file changes should trigger testing. Output:\n{}",
    stderr
  );

  Ok(())
}

/// Test --all flag runs all tests regardless of changes
#[test]
fn test_runner_all_flag() -> Result<()> {
  let ws = TestWorkspace::new_named("test-all")?;
  ws.add_crate("all-a", "0.1.0", &[])?;
  ws.add_crate("all-b", "0.1.0", &[])?;
  ws.commit("Add crates")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Run with --all flag (skip change detection)
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--all"])?;
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(output.status.success(), "test --all should succeed");
  assert!(
    stderr.contains("testing") || stderr.contains("all-a") || stderr.contains("all-b"),
    "Should run runs for all crates. Output:\n{}",
    stderr
  );

  Ok(())
}

/// Test --skip-nextest flag forces use of cargo test
#[test]
fn test_runner_skip_nextest_flag() -> Result<()> {
  let ws = TestWorkspace::new_named("test-skip-nextest")?;
  ws.add_crate("nextest-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Modify crate
  ws.modify_file("nextest-crate", "src/lib.rs", "pub fn test_fn() { }")?;
  ws.commit("Modify crate")?;

  // Run with --skip-nextest flag
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline", "--skip-nextest"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  // Should succeed and use cargo test (not nextest)
  // The output should mention cargo test or not mention nextest in the runner selection
  assert!(
    output.status.success(),
    "test --skip-nextest should succeed. stderr: {}",
    stderr
  );

  // When nextest is disabled, it should use cargo test
  // The absence of "nextest" in output confirms this (or presence of "cargo test")
  let combined = format!("{}{}", stdout, stderr);
  assert!(
    !combined.contains("cargo nextest") || combined.contains("cargo test"),
    "Should use cargo test not nextest. Output:\n{}",
    combined
  );

  Ok(())
}

/// Test --all combined with --skip-nextest
#[test]
fn test_runner_all_skip_nextest() -> Result<()> {
  let ws = TestWorkspace::new_named("test-all-skip-nextest")?;
  ws.add_crate("combo-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  // Run with both flags
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--all", "--skip-nextest"])?;

  assert!(
    output.status.success(),
    "test --all --skip-nextest should succeed. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

#[test]
fn test_runner_build_surface_uses_planner_selected_packages() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-build-selected-packages")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add crates")?;

  git(&ws.path, &["branch", "baseline"])?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed_for_build() {}")?;
  ws.commit("Modify lib-a")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--since",
      "baseline",
      "--surface",
      "build",
      "--dry-run",
      "--print-cmd",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "run should succeed. Output:\n{}", stdout);
  assert!(
    stdout.contains("build: cargo check -p lib-a"),
    "build should target selected crate(s). Output:\n{}",
    stdout
  );
  assert!(
    !stdout.contains(" -p lib-b"),
    "build should not include unaffected crates. Output:\n{}",
    stdout
  );
  assert!(
    !stdout.contains("build: cargo check --workspace"),
    "partial selection should not use --workspace. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_build_surface_ignore_bin_crates_filters_spawned_command() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-build-ignore-bin")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  std::fs::create_dir_all(ws.path.join("crates/bin-only/src"))?;
  std::fs::write(
    ws.path.join("crates/bin-only/Cargo.toml"),
    r#"[package]
name = "bin-only"
version = "0.1.0"
edition.workspace = true
license.workspace = true
authors.workspace = true

[[bin]]
name = "bin-only"
path = "src/main.rs"

[dependencies]
"#,
  )?;
  std::fs::write(ws.path.join("crates/bin-only/src/main.rs"), "fn main() {}\n")?;
  ws.commit("Add lib and bin-only crate")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--surface",
      "build",
      "--ignore-bin-crates",
      "--dry-run",
      "--print-cmd",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "run should succeed. Output:\n{}", stdout);
  assert!(
    stdout.contains("build: cargo check -p lib-a"),
    "ignore-bin-crates should keep non-bin crates in build command. Output:\n{}",
    stdout
  );
  assert!(
    !stdout.contains("bin-only"),
    "ignore-bin-crates should remove bin-only crates from build command. Output:\n{}",
    stdout
  );
  assert!(
    !stdout.contains("build: cargo check --workspace"),
    "ignore-bin-crates should force package-scoped build. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_build_surface_global_change_uses_workspace_scope() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-build-workspace-scope")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add crates")?;

  git(&ws.path, &["branch", "baseline"])?;
  std::fs::write(
    ws.path.join("rust-toolchain.toml"),
    "[toolchain]\nchannel = \"stable\"\n",
  )?;
  ws.commit("Add toolchain file")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--since",
      "baseline",
      "--surface",
      "build",
      "--dry-run",
      "--print-cmd",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "run should succeed. Output:\n{}", stdout);
  assert!(
    stdout.contains("build: cargo check --workspace"),
    "global build scope should use workspace execution. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_uses_config_default_profile() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-default-profile")?;
  ws.add_crate("profile-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[run]
default_profile = "ci"
"#,
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "run", "--all", "--dry-run", "--print-cmd"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "run should succeed");
  assert!(
    stdout.contains("build: cargo check --workspace"),
    "config default ci profile should include build. Output:\n{}",
    stdout
  );
  assert!(stdout.contains("test:"), "ci profile should include test");

  Ok(())
}

#[test]
fn test_runner_profile_flag_overrides_config_default() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-profile-overrides-default")?;
  ws.add_crate("profile-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[run]
default_profile = "local"
"#,
  )?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--profile",
      "nightly",
      "--dry-run",
      "--print-cmd",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "run should succeed");
  assert!(
    stdout.contains("build: cargo check --workspace"),
    "nightly profile should include build. Output:\n{}",
    stdout
  );
  assert!(
    stdout.contains("docs: cargo doc --workspace --no-deps"),
    "nightly profile should include docs. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_surface_flag_overrides_profile_selection() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-surface-overrides-profile")?;
  ws.add_crate("profile-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[run]
default_profile = "ci"
"#,
  )?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "run", "--all", "--surface", "docs", "--dry-run", "--print-cmd"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "run should succeed");
  assert!(
    stdout.contains("docs: cargo doc --workspace --no-deps"),
    "explicit surface should execute docs. Output:\n{}",
    stdout
  );
  assert!(
    !stdout.contains("build: cargo check --workspace"),
    "explicit surface should bypass default ci profile. Output:\n{}",
    stdout
  );
  assert!(!stdout.contains("test:"), "explicit surface should not include test");

  Ok(())
}

#[test]
fn test_runner_workflow_maps_to_profile() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-workflow-profile")?;
  ws.add_crate("profile-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[run.workflow]
commit = "ci"
"#,
  )?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--workflow",
      "commit",
      "--dry-run",
      "--print-cmd",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "run should succeed");
  assert!(
    stdout.contains("build: cargo check --workspace"),
    "workflow->profile mapping should include build. Output:\n{}",
    stdout
  );
  assert!(
    stdout.contains("test:"),
    "workflow->profile mapping should include test. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_profile_run_args_token_substitution() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-profile-token-substitution")?;
  ws.add_crate("profile-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[run.profile.docs_custom]
surfaces = ["docs"]
run_args = ["--manifest-path", "{workspace_root}/Cargo.toml", "{cargo_args}", "--quiet"]
"#,
  )?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--profile",
      "docs_custom",
      "--dry-run",
      "--print-cmd",
      "--",
      "--color",
      "never",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let manifest_path = ws.path.join("Cargo.toml");
  let stdout_normalized = stdout.replace('\\', "/");
  let manifest_path_normalized = manifest_path.display().to_string().replace('\\', "/");

  assert!(output.status.success(), "run should succeed");
  assert!(
    stdout.contains("docs: cargo doc --workspace --no-deps --manifest-path"),
    "docs command should include profile args. Output:\n{}",
    stdout
  );
  assert!(
    stdout_normalized.contains(&manifest_path_normalized),
    "workspace_root token should expand to absolute path. Output:\n{}",
    stdout
  );
  assert!(
    stdout.contains("--color never --quiet"),
    "cargo_args token should splice CLI args before trailing profile args. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_workspace_root_applies_to_spawned_subprocesses() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-workspace-root-cwd")?;
  ws.add_crate("cwd-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  let outside_cwd = ws.path.parent().expect("temp workspace should have parent directory");
  let workspace_root = ws.path.display().to_string();
  let args = [
    "rail",
    "--workspace-root",
    workspace_root.as_str(),
    "run",
    "--all",
    "--surface",
    "build",
    "--print-cmd",
  ];
  let output = run_cargo_rail(outside_cwd, &args)?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(
    output.status.success(),
    "run should succeed from outside workspace when --workspace-root is set. stdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(
    stdout.contains("build: cargo check --workspace"),
    "build command should execute via run surface. Output:\n{}",
    stdout
  );

  Ok(())
}
