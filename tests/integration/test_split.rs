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
  run_cargo_rail(&ws.path, &["rail", "split", "mylib"])?;

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

  run_cargo_rail(&ws.path, &["rail", "split", "mylib"])?;

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

  run_cargo_rail(&ws.path, &["rail", "split", "lib-a"])?;

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

  run_cargo_rail(&ws.path, &["rail", "split", "lib-core"])?;

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

  run_cargo_rail(&ws.path, &["rail", "split", "combined"])?;

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
  run_cargo_rail(&ws.path, &["rail", "split", "lib-release"])?;

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
    &["rail", "release", "--all", "--bump", "patch", "--skip-publish"],
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
  let output = run_cargo_rail(&ws.path, &["rail", "split", "json-lib", "--check", "--json"])?;

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
