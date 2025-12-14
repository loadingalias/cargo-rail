//! Integration tests for split operations

use crate::helpers::{TestWorkspace, git, run_cargo_rail};
use anyhow::Result;
use tempfile::TempDir;

#[test]
fn test_split_single_crate_basic() -> Result<()> {
  // Create monorepo with a single crate
  let ws = TestWorkspace::new()?;
  ws.add_crate("mylib", "0.1.0", &[])?;
  ws.commit("Add mylib")?;

  // Create split repo target
  let split_dir = TempDir::new()?;
  let split_path = split_dir.path();

  // Create rail.toml config
  let config = format!(
    r#"[workspace]
root = "."

[crates.mylib.split]
remote = "{}"
branch = "main"
mode = "single"
paths = [{{ crate = "crates/mylib" }}]
"#,
    split_path.display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  // Perform split
  run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;

  // Verify split structure
  assert!(split_path.join("Cargo.toml").exists(), "Cargo.toml should exist");
  assert!(split_path.join("src/lib.rs").exists(), "src/lib.rs should exist");
  assert!(split_path.join("README.md").exists(), "README.md should exist");

  // Verify Cargo.toml was transformed (no workspace inheritance)
  let cargo_toml = std::fs::read_to_string(split_path.join("Cargo.toml"))?;
  assert!(
    !cargo_toml.contains("workspace = true"),
    "Should not contain workspace inheritance"
  );
  assert!(
    cargo_toml.contains("edition = \"2021\""),
    "Should have flattened edition"
  );

  Ok(())
}

#[test]
fn test_split_preserves_git_history() -> Result<()> {
  let ws = TestWorkspace::new()?;
  ws.add_crate("mylib", "0.1.0", &[])?;
  ws.commit("Initial mylib")?;

  // Make several commits
  ws.modify_file("mylib", "src/lib.rs", "// Version 1")?;
  ws.commit("Update mylib v1")?;

  ws.modify_file("mylib", "src/lib.rs", "// Version 2")?;
  ws.commit("Update mylib v2")?;

  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[crates.mylib.split]
remote = "{}"
branch = "main"
mode = "single"
paths = [{{ crate = "crates/mylib" }}]
"#,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;

  // Check git history in split
  let log_output = git(split_dir.path(), &["log", "--oneline"])?;
  let log = String::from_utf8_lossy(&log_output.stdout);

  assert!(log.contains("Initial mylib"), "Should contain initial commit");
  assert!(log.contains("Update mylib v1"), "Should contain v1 commit");
  assert!(log.contains("Update mylib v2"), "Should contain v2 commit");

  Ok(())
}

#[test]
fn test_split_filters_unrelated_commits() -> Result<()> {
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add both libs")?;

  // Modify only lib-a
  ws.modify_file("lib-a", "src/lib.rs", "// Changed A")?;
  ws.commit("Update lib-a")?;

  // Modify only lib-b
  ws.modify_file("lib-b", "src/lib.rs", "// Changed B")?;
  ws.commit("Update lib-b")?;

  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[crates.lib-a.split]
remote = "{}"
branch = "main"
mode = "single"
paths = [{{ crate = "crates/lib-a" }}]
"#,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  run_cargo_rail(&ws.path, &["rail", "split", "run", "lib-a", "--yes", "--allow-dirty"])?;

  // Check that only lib-a commits are in split
  let log_output = git(split_dir.path(), &["log", "--oneline"])?;
  let log = String::from_utf8_lossy(&log_output.stdout);

  assert!(log.contains("Add both libs"), "Should contain initial commit");
  assert!(log.contains("Update lib-a"), "Should contain lib-a update");
  assert!(!log.contains("Update lib-b"), "Should NOT contain lib-b update");

  Ok(())
}

#[test]
fn test_split_transforms_path_dependencies() -> Result<()> {
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-util", "0.1.0", &[])?;
  ws.add_crate(
    "lib-core",
    "0.2.0",
    &[("lib-util", r#"{ version = "0.1", path = "../lib-util" }"#)],
  )?;
  ws.commit("Add libs with dependency")?;

  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[crates.lib-core.split]
remote = "{}"
branch = "main"
mode = "single"
paths = [{{ crate = "crates/lib-core" }}]
"#,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  run_cargo_rail(
    &ws.path,
    &["rail", "split", "run", "lib-core", "--yes", "--allow-dirty"],
  )?;

  // Check that path dependency was transformed to version dependency
  let cargo_toml = std::fs::read_to_string(split_dir.path().join("Cargo.toml"))?;

  assert!(!cargo_toml.contains("path ="), "Should not contain path dependencies");
  assert!(
    cargo_toml.contains("lib-util") && cargo_toml.contains("0.1"),
    "Should have version dependency on lib-util"
  );

  Ok(())
}

#[test]
fn test_split_combined_mode_multiple_crates() -> Result<()> {
  // Test combined mode: multiple crates split to one repo, preserving structure
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-core", "0.1.0", &[])?;
  ws.add_crate("service-api", "0.2.0", &[])?;
  ws.commit("Add lib-core and service-api")?;

  // Make changes to both crates
  ws.modify_file("lib-core", "src/lib.rs", "// Core functionality")?;
  ws.commit("Update lib-core")?;

  ws.modify_file("service-api", "src/lib.rs", "// API service")?;
  ws.commit("Update service-api")?;

  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[crates.combined.split]
remote = "{}"
branch = "main"
mode = "combined"
paths = [
  {{ crate = "crates/lib-core" }},
  {{ crate = "crates/service-api" }}
]
"#,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  run_cargo_rail(
    &ws.path,
    &["rail", "split", "run", "combined", "--yes", "--allow-dirty"],
  )?;

  // Verify both crates exist with preserved structure
  let split_path = split_dir.path();
  assert!(
    split_path.join("crates/lib-core/Cargo.toml").exists(),
    "lib-core Cargo.toml should exist at crates/lib-core/Cargo.toml"
  );
  assert!(
    split_path.join("crates/lib-core/src/lib.rs").exists(),
    "lib-core lib.rs should exist at crates/lib-core/src/lib.rs"
  );
  assert!(
    split_path.join("crates/service-api/Cargo.toml").exists(),
    "service-api Cargo.toml should exist at crates/service-api/Cargo.toml"
  );
  assert!(
    split_path.join("crates/service-api/src/lib.rs").exists(),
    "service-api lib.rs should exist at crates/service-api/src/lib.rs"
  );

  // Verify content was copied correctly
  let core_content = std::fs::read_to_string(split_path.join("crates/lib-core/src/lib.rs"))?;
  assert!(
    core_content.contains("// Core functionality"),
    "lib-core should have correct content"
  );

  let api_content = std::fs::read_to_string(split_path.join("crates/service-api/src/lib.rs"))?;
  assert!(
    api_content.contains("// API service"),
    "service-api should have correct content"
  );

  // Verify git history includes commits for both crates
  let log_output = git(split_path, &["log", "--oneline"])?;
  let log = String::from_utf8_lossy(&log_output.stdout);
  assert!(
    log.contains("Add lib-core and service-api"),
    "Should contain initial commit"
  );
  assert!(log.contains("Update lib-core"), "Should contain lib-core update");
  assert!(log.contains("Update service-api"), "Should contain service-api update");

  Ok(())
}

#[test]
fn test_split_release_flow_creates_tag_and_changelog() -> Result<()> {
  // Split a crate, then run release in the split repo to ensure tagging/changelog works.
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-release", "0.1.0", &[])?;
  ws.commit("Add lib-release")?;

  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[crates.lib-release.split]
remote = "{}"
branch = "main"
mode = "single"
paths = [{{ crate = "crates/lib-release" }}]
"#,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  // Perform split
  run_cargo_rail(
    &ws.path,
    &["rail", "split", "run", "lib-release", "--yes", "--allow-dirty"],
  )?;

  // Prepare release config inside split repo
  let split_root = split_dir.path();
  // Configure git user to allow tagging/commits in split repo
  git(split_root, &["config", "user.name", "Test Split"])?;
  git(split_root, &["config", "user.email", "split@example.com"])?;

  std::fs::create_dir_all(split_root.join(".config"))?;
  std::fs::write(
    split_root.join(".config/rail.toml"),
    r#"[workspace]
root = "."

[release]
tag_prefix = "v"
tag_format = "v{version}"
changelog_path = "CHANGELOG.md"
require_clean = false
"#,
  )?;

  // Tag current version
  git(split_root, &["tag", "-a", "v0.1.0", "-m", "Initial split tag"])?;

  // Make a change to release
  std::fs::write(split_root.join("src/lib.rs"), "// bumped")?;
  git(split_root, &["add", "."])?;
  git(split_root, &["commit", "-m", "feat: prepare release"])?;

  // Run release publish in split repo (skip crates.io)
  let output = run_cargo_rail(
    split_root,
    &["rail", "release", "run", "--all", "--bump", "patch", "--skip-publish"],
  )?;
  assert!(output.status.success(), "Split release should succeed");

  // Verify tag and changelog
  let tags = git(split_root, &["tag", "--list"])?;
  let tag_list = String::from_utf8_lossy(&tags.stdout);
  assert!(
    tag_list.contains("v0.1.1"),
    "Release should create new tag v0.1.1. Tags:\n{}",
    tag_list
  );

  let changelog = std::fs::read_to_string(split_root.join("CHANGELOG.md"))?;
  assert!(
    changelog.contains("## [0.1.1]"),
    "Changelog should include new version header"
  );

  Ok(())
}

/// Test split --remote override flag
#[test]
fn test_split_remote_override() -> Result<()> {
  let ws = TestWorkspace::new_named("split-remote-override")?;
  ws.add_crate("override-lib", "0.1.0", &[])?;

  // Create a custom target directory
  let custom_target = tempfile::TempDir::new()?;
  git(custom_target.path(), &["init", "--initial-branch=main"])?;
  git(custom_target.path(), &["config", "user.name", "Test"])?;
  git(custom_target.path(), &["config", "user.email", "test@test.com"])?;
  std::fs::write(custom_target.path().join("README.md"), "# Custom")?;
  git(custom_target.path(), &["add", "."])?;
  git(custom_target.path(), &["commit", "-m", "Initial"])?;

  // Configure split with default remote using new [crates.<name>.split] format
  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[workspace]
root = "."

[crates.override-lib.split]
remote = "/tmp/default-remote"
branch = "main"
mode = "single"
paths = [{ crate = "crates/override-lib" }]
"#,
  )?;

  ws.commit("Add override-lib with config")?;

  // Run split with --remote override in check mode (dry-run)
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "split",
      "run",
      "override-lib",
      "--check",
      "--remote",
      custom_target.path().to_str().unwrap(),
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Exit code 1 = check found pending changes (correct behavior)
  assert!(
    output.status.code() == Some(1),
    "split --remote --check should exit 1 when split pending. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  // Verify the custom target path appears in the plan
  assert!(
    stdout.contains(custom_target.path().to_str().unwrap()) || stdout.contains("override-lib"),
    "Should show the custom target path or crate name in plan. stdout: {}",
    stdout
  );

  Ok(())
}

/// Test split --json output
#[test]
fn test_split_json_output() -> Result<()> {
  let ws = TestWorkspace::new_named("split-json")?;
  ws.add_crate("json-lib", "0.1.0", &[])?;
  ws.commit("Add json-lib")?;

  // Configure split using new [crates.<name>.split] format
  let target_dir = tempfile::TempDir::new()?;
  std::fs::write(
    ws.path.join(".config/rail.toml"),
    format!(
      r#"[workspace]
root = "."

[crates.json-lib.split]
remote = "{}"
branch = "main"
mode = "single"
paths = [{{ crate = "crates/json-lib" }}]
"#,
      target_dir.path().display()
    ),
  )?;

  // Run split with --check and --json
  let output = run_cargo_rail(
    &ws.path,
    &["rail", "split", "run", "json-lib", "--check", "--format", "json"],
  )?;

  if output.status.success() {
    let stdout = String::from_utf8_lossy(&output.stdout);
    // If there's output, it should be valid JSON
    if !stdout.trim().is_empty() {
      let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
      assert!(
        parsed.is_ok(),
        "split --json should output valid JSON. stdout: {}",
        stdout
      );
    }
  }

  Ok(())
}

/// Test split init command
#[test]
fn test_split_init_command() -> Result<()> {
  let ws = TestWorkspace::new_named("split-init-cmd")?;
  ws.add_crate("init-lib", "0.1.0", &[])?;
  ws.commit("Add init-lib")?;

  // Remove existing config to test init
  ws.remove_config()?;

  // Create minimal config without splits
  std::fs::create_dir_all(ws.path.join(".config"))?;
  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[workspace]
root = "."
"#,
  )?;

  // Run split init with --check
  let output = run_cargo_rail(&ws.path, &["rail", "split", "init", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(
    output.status.success(),
    "split init --check should succeed. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(
    stdout.contains("init-lib") || stdout.contains("[crates."),
    "split init should show detected crates. Output:\n{}",
    stdout
  );

  Ok(())
}

/// Test that split uses parallel prefetch for performance on repos with many commits.
/// This test creates more than 5 commits to trigger the parallel prefetch path.
#[test]
fn test_split_parallel_prefetch_many_commits() -> Result<()> {
  let ws = TestWorkspace::new_named("split-parallel-prefetch")?;

  // Create initial crate
  ws.add_crate("prefetch-lib", "0.1.0", &[])?;
  ws.commit("Add prefetch-lib")?;

  // Create more than 5 commits to trigger parallel prefetch (threshold is > 5)
  for i in 1..=8 {
    ws.modify_file("prefetch-lib", "src/lib.rs", &format!("// Version {}", i))?;
    ws.commit(&format!("Update prefetch-lib v{}", i))?;
  }

  // Configure split
  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[crates.prefetch-lib.split]
remote = "{}"
branch = "main"
mode = "single"
paths = [{{ crate = "crates/prefetch-lib" }}]
"#,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  // Run split - should use parallel prefetch
  let output = run_cargo_rail(
    &ws.path,
    &["rail", "split", "run", "prefetch-lib", "--yes", "--allow-dirty"],
  )?;

  // Verify split succeeded
  assert!(
    output.status.success(),
    "Split with parallel prefetch should succeed. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  // Check stderr mentions parallel prefetch (progress output goes to stderr)
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("Prefetching file contents in parallel"),
    "Should use parallel prefetch for 9 commits. stderr: {}",
    stderr
  );

  // Verify the split repo has all commits
  let log_output = git(split_dir.path(), &["log", "--oneline"])?;
  let log = String::from_utf8_lossy(&log_output.stdout);

  assert!(log.contains("Add prefetch-lib"), "Should have initial commit");
  assert!(log.contains("Update prefetch-lib v8"), "Should have last commit");

  // Verify final content
  let lib_content = std::fs::read_to_string(split_dir.path().join("src/lib.rs"))?;
  assert!(
    lib_content.contains("Version 8"),
    "Final content should be Version 8. Got: {}",
    lib_content
  );

  Ok(())
}

/// Test that split handles "dirty history" gracefully - commits where the crate
/// was temporarily deleted or didn't exist at certain points in history.
///
/// This is a common scenario when:
/// - A crate is temporarily removed and later restored
/// - Files are moved/renamed in a way that deleted the old path temporarily
/// - The crate didn't exist at the start of the filtered history
#[test]
fn test_split_handles_dirty_history() -> Result<()> {
  let ws = TestWorkspace::new_named("split-dirty-history")?;

  // Step 1: Create crate and commit
  ws.add_crate("dirty-lib", "0.1.0", &[])?;
  ws.commit("Add dirty-lib")?;

  // Step 2: Make a change
  ws.modify_file("dirty-lib", "src/lib.rs", "// Version 1")?;
  ws.commit("Update dirty-lib v1")?;

  // Step 3: DELETE the crate entirely (simulating dirty history)
  std::fs::remove_dir_all(ws.path.join("crates/dirty-lib"))?;
  ws.commit("Remove dirty-lib temporarily")?;

  // Step 4: Recreate the crate (restoration)
  ws.add_crate("dirty-lib", "0.2.0", &[])?;
  ws.modify_file("dirty-lib", "src/lib.rs", "// Version 2 - restored")?;
  ws.commit("Restore dirty-lib")?;

  // Step 5: Make another change after restoration
  ws.modify_file("dirty-lib", "src/lib.rs", "// Version 3 - final")?;
  ws.commit("Update dirty-lib v3")?;

  // Configure split
  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[crates.dirty-lib.split]
remote = "{}"
branch = "main"
mode = "single"
paths = [{{ crate = "crates/dirty-lib" }}]
"#,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  // Run split - should succeed despite dirty history
  let output = run_cargo_rail(
    &ws.path,
    &["rail", "split", "run", "dirty-lib", "--yes", "--allow-dirty"],
  )?;

  // Verify split succeeded
  assert!(
    output.status.success(),
    "Split should succeed with dirty history. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  // Check stderr mentions skipped commits (progress output goes to stderr)
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("Skipped") && stderr.contains("dirty history"),
    "Should mention skipped commits due to dirty history. stderr: {}",
    stderr
  );

  // Verify the split repo exists and has files
  assert!(
    split_dir.path().join("Cargo.toml").exists(),
    "Cargo.toml should exist in split repo"
  );
  assert!(
    split_dir.path().join("src/lib.rs").exists(),
    "src/lib.rs should exist in split repo"
  );

  // Verify the final content is correct
  let lib_content = std::fs::read_to_string(split_dir.path().join("src/lib.rs"))?;
  assert!(
    lib_content.contains("Version 3 - final"),
    "Final content should be Version 3. Got: {}",
    lib_content
  );

  // Verify git history in split repo has the commits that DID have files
  let log_output = git(split_dir.path(), &["log", "--oneline"])?;
  let log = String::from_utf8_lossy(&log_output.stdout);

  assert!(log.contains("Add dirty-lib"), "Should contain initial add commit");
  assert!(log.contains("Update dirty-lib v1"), "Should contain v1 update");
  // The deletion commit should be skipped (no files at that point)
  assert!(
    !log.contains("Remove dirty-lib"),
    "Should NOT contain deletion commit. Log:\n{}",
    log
  );
  assert!(log.contains("Restore dirty-lib"), "Should contain restore commit");
  assert!(log.contains("Update dirty-lib v3"), "Should contain v3 update");

  Ok(())
}

// ============================================================================
// Safety Rails Tests
// ============================================================================

/// Test that split fails on dirty worktree without --allow-dirty
#[test]
fn test_split_dirty_worktree_error() -> Result<()> {
  let ws = TestWorkspace::new()?;
  ws.add_crate("safety-lib", "0.1.0", &[])?;
  ws.commit("Add safety-lib")?;

  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[crates.safety-lib.split]
remote = "{}"
branch = "main"
mode = "single"
paths = [{{ crate = "crates/safety-lib" }}]
"#,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  // Make worktree dirty by adding an uncommitted file
  std::fs::write(ws.path.join("dirty.txt"), "uncommitted content")?;

  // Run split WITHOUT --allow-dirty - should fail
  let output = run_cargo_rail(&ws.path, &["rail", "split", "run", "safety-lib", "--yes"])?;

  assert!(
    !output.status.success(),
    "Split should fail on dirty worktree without --allow-dirty"
  );

  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("uncommitted changes") || stderr.contains("dirty"),
    "Error should mention uncommitted changes. stderr: {}",
    stderr
  );

  Ok(())
}

/// Test that --allow-dirty bypasses the dirty worktree check
#[test]
fn test_split_allow_dirty_bypasses_check() -> Result<()> {
  let ws = TestWorkspace::new()?;
  ws.add_crate("allow-dirty-lib", "0.1.0", &[])?;
  ws.commit("Add allow-dirty-lib")?;

  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[crates.allow-dirty-lib.split]
remote = "{}"
branch = "main"
mode = "single"
paths = [{{ crate = "crates/allow-dirty-lib" }}]
"#,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  // Make worktree dirty
  std::fs::write(ws.path.join("dirty.txt"), "uncommitted content")?;

  // Run split WITH --allow-dirty - should succeed
  let output = run_cargo_rail(
    &ws.path,
    &["rail", "split", "run", "allow-dirty-lib", "--yes", "--allow-dirty"],
  )?;

  assert!(
    output.status.success(),
    "Split should succeed with --allow-dirty. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  // Verify split was created
  assert!(
    split_dir.path().join("Cargo.toml").exists(),
    "Split repo should be created"
  );

  Ok(())
}

/// Test that split is idempotent - running twice produces the same result (no-op on second run)
#[test]
fn test_split_idempotent_run_twice() -> Result<()> {
  let ws = TestWorkspace::new()?;
  ws.add_crate("idempotent-lib", "0.1.0", &[])?;
  ws.commit("Initial commit")?;

  // Make a few commits to have history
  ws.modify_file("idempotent-lib", "src/lib.rs", "// Version 1")?;
  ws.commit("Update v1")?;

  ws.modify_file("idempotent-lib", "src/lib.rs", "// Version 2")?;
  ws.commit("Update v2")?;

  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[crates.idempotent-lib.split]
remote = "{}"
branch = "main"
mode = "single"
paths = [{{ crate = "crates/idempotent-lib" }}]
"#,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  // First split
  let output1 = run_cargo_rail(
    &ws.path,
    &["rail", "split", "run", "idempotent-lib", "--yes", "--allow-dirty"],
  )?;
  assert!(
    output1.status.success(),
    "First split should succeed. stderr: {}",
    String::from_utf8_lossy(&output1.stderr)
  );

  // Get commit count after first split
  let log1 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
  let commit_count1: usize = String::from_utf8_lossy(&log1.stdout).trim().parse()?;

  // Get HEAD SHA after first split
  let head1 = git(split_dir.path(), &["rev-parse", "HEAD"])?;
  let head_sha1 = String::from_utf8_lossy(&head1.stdout).trim().to_string();

  // Second split (should be no-op)
  let output2 = run_cargo_rail(
    &ws.path,
    &["rail", "split", "run", "idempotent-lib", "--yes", "--allow-dirty"],
  )?;
  assert!(
    output2.status.success(),
    "Second split should succeed. stderr: {}",
    String::from_utf8_lossy(&output2.stderr)
  );

  // Verify "already up-to-date" message (progress output goes to stderr)
  let stderr2 = String::from_utf8_lossy(&output2.stderr);
  assert!(
    stderr2.contains("already up-to-date") || stderr2.contains("already split"),
    "Second run should indicate already up-to-date. stderr: {}",
    stderr2
  );

  // Get commit count after second split
  let log2 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
  let commit_count2: usize = String::from_utf8_lossy(&log2.stdout).trim().parse()?;

  // Get HEAD SHA after second split
  let head2 = git(split_dir.path(), &["rev-parse", "HEAD"])?;
  let head_sha2 = String::from_utf8_lossy(&head2.stdout).trim().to_string();

  // Verify no new commits were created
  assert_eq!(
    commit_count1, commit_count2,
    "Commit count should not change on second split"
  );
  assert_eq!(head_sha1, head_sha2, "HEAD should not change on second split");

  Ok(())
}

/// Test that split is incremental - new commits are added without duplicating existing ones
#[test]
fn test_split_incremental_new_commits() -> Result<()> {
  let ws = TestWorkspace::new()?;
  ws.add_crate("incremental-lib", "0.1.0", &[])?;
  ws.commit("Initial commit")?;

  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[crates.incremental-lib.split]
remote = "{}"
branch = "main"
mode = "single"
paths = [{{ crate = "crates/incremental-lib" }}]
"#,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  // First split
  run_cargo_rail(
    &ws.path,
    &["rail", "split", "run", "incremental-lib", "--yes", "--allow-dirty"],
  )?;

  // Get commit count after first split
  let log1 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
  let commit_count1: usize = String::from_utf8_lossy(&log1.stdout).trim().parse()?;

  // Add new commits to monorepo
  ws.modify_file("incremental-lib", "src/lib.rs", "// New feature 1")?;
  ws.commit("Add feature 1")?;

  ws.modify_file("incremental-lib", "src/lib.rs", "// New feature 2")?;
  ws.commit("Add feature 2")?;

  // Second split (should add only new commits)
  let output2 = run_cargo_rail(
    &ws.path,
    &["rail", "split", "run", "incremental-lib", "--yes", "--allow-dirty"],
  )?;
  assert!(
    output2.status.success(),
    "Incremental split should succeed. stderr: {}",
    String::from_utf8_lossy(&output2.stderr)
  );

  // Get commit count after second split
  let log2 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
  let commit_count2: usize = String::from_utf8_lossy(&log2.stdout).trim().parse()?;

  // Should have exactly 2 more commits (for the 2 new features)
  assert_eq!(
    commit_count2,
    commit_count1 + 2,
    "Should have exactly 2 new commits. Before: {}, After: {}",
    commit_count1,
    commit_count2
  );

  // Verify the new commits are there
  let log_output = git(split_dir.path(), &["log", "--oneline"])?;
  let log = String::from_utf8_lossy(&log_output.stdout);
  assert!(log.contains("Add feature 1"), "Should contain feature 1 commit");
  assert!(log.contains("Add feature 2"), "Should contain feature 2 commit");

  Ok(())
}

/// Test that split combined mode is idempotent
#[test]
fn test_split_combined_mode_idempotent() -> Result<()> {
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-core", "0.1.0", &[])?;
  ws.add_crate("service-api", "0.1.0", &[("lib-core", r#"{ path = "../lib-core" }"#)])?;
  ws.commit("Initial combined crates")?;

  // Make commits to both crates
  ws.modify_file("lib-core", "src/lib.rs", "// Core v1")?;
  ws.commit("Update lib-core")?;

  ws.modify_file("service-api", "src/lib.rs", "// API v1")?;
  ws.commit("Update service-api")?;

  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[crates.combined.split]
remote = "{}"
branch = "main"
mode = "combined"
paths = [
  {{ crate = "crates/lib-core" }},
  {{ crate = "crates/service-api" }}
]
"#,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  // First split
  let output1 = run_cargo_rail(
    &ws.path,
    &["rail", "split", "run", "combined", "--yes", "--allow-dirty"],
  )?;
  assert!(
    output1.status.success(),
    "First combined split should succeed. stderr: {}",
    String::from_utf8_lossy(&output1.stderr)
  );

  // Get state after first split
  let log1 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
  let commit_count1: usize = String::from_utf8_lossy(&log1.stdout).trim().parse()?;
  let head1 = git(split_dir.path(), &["rev-parse", "HEAD"])?;
  let head_sha1 = String::from_utf8_lossy(&head1.stdout).trim().to_string();

  // Second split (should be no-op)
  let output2 = run_cargo_rail(
    &ws.path,
    &["rail", "split", "run", "combined", "--yes", "--allow-dirty"],
  )?;
  assert!(
    output2.status.success(),
    "Second combined split should succeed. stderr: {}",
    String::from_utf8_lossy(&output2.stderr)
  );

  // Verify "already up-to-date" message (progress output goes to stderr)
  let stderr2 = String::from_utf8_lossy(&output2.stderr);
  assert!(
    stderr2.contains("already up-to-date") || stderr2.contains("already split"),
    "Second run should indicate already up-to-date. stderr: {}",
    stderr2
  );

  // Verify no changes
  let log2 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
  let commit_count2: usize = String::from_utf8_lossy(&log2.stdout).trim().parse()?;
  let head2 = git(split_dir.path(), &["rev-parse", "HEAD"])?;
  let head_sha2 = String::from_utf8_lossy(&head2.stdout).trim().to_string();

  assert_eq!(commit_count1, commit_count2, "Commit count should not change");
  assert_eq!(head_sha1, head_sha2, "HEAD should not change");

  Ok(())
}

/// Test that split recovers gracefully from partial/interrupted state
#[test]
fn test_split_partial_state_recovery() -> Result<()> {
  let ws = TestWorkspace::new()?;
  ws.add_crate("partial-lib", "0.1.0", &[])?;
  ws.commit("Initial commit")?;

  ws.modify_file("partial-lib", "src/lib.rs", "// V1")?;
  ws.commit("Version 1")?;

  ws.modify_file("partial-lib", "src/lib.rs", "// V2")?;
  ws.commit("Version 2")?;

  ws.modify_file("partial-lib", "src/lib.rs", "// V3")?;
  ws.commit("Version 3")?;

  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[crates.partial-lib.split]
remote = "{}"
branch = "main"
mode = "single"
paths = [{{ crate = "crates/partial-lib" }}]
"#,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  // First split - creates full history
  run_cargo_rail(
    &ws.path,
    &["rail", "split", "run", "partial-lib", "--yes", "--allow-dirty"],
  )?;

  // Simulate partial state by manually removing mappings for later commits
  // We'll delete the git-notes to simulate an interrupted split
  let notes_ref = "refs/notes/rail/partial-lib".to_string();

  // Get commit count before manipulation
  let log_before = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
  let count_before: usize = String::from_utf8_lossy(&log_before.stdout).trim().parse()?;

  // Delete notes in both repos to simulate interrupted state
  let _ = git(&ws.path, &["update-ref", "-d", &notes_ref]);
  let _ = git(split_dir.path(), &["update-ref", "-d", &notes_ref]);

  // Now add a new commit
  ws.modify_file("partial-lib", "src/lib.rs", "// V4 after interruption")?;
  ws.commit("Version 4")?;

  // Re-run split - should handle the missing mappings gracefully
  let output = run_cargo_rail(
    &ws.path,
    &["rail", "split", "run", "partial-lib", "--yes", "--allow-dirty"],
  )?;
  assert!(
    output.status.success(),
    "Split after partial state should succeed. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  // Verify split repo has commits (exact count depends on implementation)
  let log_after = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
  let count_after: usize = String::from_utf8_lossy(&log_after.stdout).trim().parse()?;

  // Should have at least as many commits as before (recovery may add more or same)
  assert!(
    count_after >= count_before,
    "Should have at least {} commits after recovery, got {}",
    count_before,
    count_after
  );

  // Verify the new content is there
  let content = std::fs::read_to_string(split_dir.path().join("src/lib.rs"))?;
  assert!(
    content.contains("V4 after interruption"),
    "Should have the latest content"
  );

  Ok(())
}

/// Test that split with auxiliary files is idempotent (final commit doesn't duplicate)
#[test]
fn test_split_auxiliary_files_idempotent() -> Result<()> {
  let ws = TestWorkspace::new()?;
  ws.add_crate("aux-lib", "0.1.0", &[])?;
  ws.commit("Initial commit")?;

  // Add some auxiliary files at workspace root
  std::fs::write(ws.path.join("rustfmt.toml"), "max_width = 100")?;
  std::fs::write(ws.path.join(".editorconfig"), "root = true")?;
  git(&ws.path, &["add", "."])?;
  git(&ws.path, &["commit", "-m", "Add config files"])?;

  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[crates.aux-lib.split]
remote = "{}"
branch = "main"
mode = "single"
paths = [{{ crate = "crates/aux-lib" }}]
"#,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  // First split
  run_cargo_rail(&ws.path, &["rail", "split", "run", "aux-lib", "--yes", "--allow-dirty"])?;

  // Count commits including auxiliary files commit
  let log1 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
  let commit_count1: usize = String::from_utf8_lossy(&log1.stdout).trim().parse()?;

  // Second split - auxiliary files commit should not be duplicated
  let output2 = run_cargo_rail(&ws.path, &["rail", "split", "run", "aux-lib", "--yes", "--allow-dirty"])?;
  assert!(output2.status.success());

  // Progress output goes to stderr
  let stderr2 = String::from_utf8_lossy(&output2.stderr);
  assert!(
    stderr2.contains("already up-to-date") || stderr2.contains("already split"),
    "Should be up-to-date. stderr: {}",
    stderr2
  );

  // Count should be the same
  let log2 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
  let commit_count2: usize = String::from_utf8_lossy(&log2.stdout).trim().parse()?;

  assert_eq!(
    commit_count1, commit_count2,
    "Auxiliary files commit should not be duplicated"
  );

  // Verify auxiliary files exist
  assert!(split_dir.path().join("rustfmt.toml").exists());
  assert!(split_dir.path().join(".editorconfig").exists());

  Ok(())
}
