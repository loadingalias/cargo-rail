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

[[splits]]
name = "mylib"
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

[[splits]]
name = "mylib"
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

[[splits]]
name = "lib-a"
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

[[splits]]
name = "lib-core"
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
fn test_split_release_flow_creates_tag_and_changelog() -> Result<()> {
  // Split a crate, then run release in the split repo to ensure tagging/changelog works.
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-release", "0.1.0", &[])?;
  ws.commit("Add lib-release")?;

  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[[splits]]
name = "lib-release"
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
    &[
      "rail",
      "release",
      "publish",
      "--all",
      "--bump",
      "patch",
      "--execute",
      "--skip-publish",
    ],
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
