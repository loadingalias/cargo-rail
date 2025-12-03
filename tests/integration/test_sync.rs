//! Integration tests for sync operations

use crate::helpers::{TestWorkspace, git, run_cargo_rail};
use anyhow::Result;
use tempfile::TempDir;

/// Helper to set up a split scenario
fn setup_split_scenario(crate_name: &str) -> Result<(TestWorkspace, TempDir)> {
  let ws = TestWorkspace::new()?;
  ws.add_crate(crate_name, "0.1.0", &[])?;
  ws.commit(&format!("Initial {}", crate_name))?;

  let split_dir = TempDir::new()?;
  let config = format!(
    r#"[workspace]
root = "."

[crates.{0}.split]
remote = "{1}"
branch = "main"
mode = "single"
paths = [{{ crate = "crates/{0}" }}]
"#,
    crate_name,
    split_dir.path().display().to_string().replace('\\', "\\\\")
  );
  std::fs::write(ws.path.join("rail.toml"), config)?;

  // Perform initial split
  run_cargo_rail(&ws.path, &["rail", "split", "run", crate_name])?;

  Ok((ws, split_dir))
}

#[test]
fn test_sync_to_remote_basic() -> Result<()> {
  let (ws, split_dir) = setup_split_scenario("mylib")?;

  // Make change in monorepo
  ws.modify_file("mylib", "src/lib.rs", "// Changed in mono")?;
  ws.commit("Update mylib in mono")?;

  // Sync to remote
  run_cargo_rail(&ws.path, &["rail", "sync", "mylib", "--to-remote"])?;

  // Verify change in split
  let split_content = std::fs::read_to_string(split_dir.path().join("src/lib.rs"))?;
  assert!(
    split_content.contains("// Changed in mono"),
    "Split should have the change from mono"
  );

  // Verify commit message includes Rail-Origin trailer
  let log_output = git(split_dir.path(), &["log", "-1", "--format=%B"])?;
  let log = String::from_utf8_lossy(&log_output.stdout);
  assert!(log.contains("Rail-Origin: mono@"), "Should have Rail-Origin trailer");

  Ok(())
}

#[test]
fn test_sync_from_remote_basic() -> Result<()> {
  let (ws, split_dir) = setup_split_scenario("mylib")?;

  // Make change in split repo
  std::fs::write(split_dir.path().join("src/lib.rs"), "// Changed in split")?;
  git(split_dir.path(), &["add", "."])?;
  git(split_dir.path(), &["commit", "-m", "Update in split"])?;

  // Sync from remote
  run_cargo_rail(&ws.path, &["rail", "sync", "mylib", "--from-remote"])?;

  // Verify change in monorepo (on PR branch, not original branch)
  let mono_content = std::fs::read_to_string(ws.path.join("crates/mylib/src/lib.rs"))?;
  assert!(
    mono_content.contains("// Changed in split"),
    "Mono should have the change from split"
  );

  // Verify commit message includes Rail-Origin trailer
  let log_output = git(&ws.path, &["log", "-1", "--format=%B"])?;
  let log = String::from_utf8_lossy(&log_output.stdout);
  assert!(log.contains("Rail-Origin: remote@"), "Should have Rail-Origin trailer");

  Ok(())
}

#[test]
fn test_sync_from_remote_creates_pr_branch() -> Result<()> {
  let (ws, split_dir) = setup_split_scenario("mylib")?;

  // Get original branch name
  let original_branch_output = git(&ws.path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
  let original_branch = String::from_utf8_lossy(&original_branch_output.stdout)
    .trim()
    .to_string();

  // Make change in split repo
  std::fs::write(split_dir.path().join("src/lib.rs"), "// Changed in split")?;
  git(split_dir.path(), &["add", "."])?;
  git(split_dir.path(), &["commit", "-m", "Test change in split"])?;

  // Sync from remote - should create PR branch
  run_cargo_rail(&ws.path, &["rail", "sync", "mylib", "--from-remote"])?;

  // Verify we're on a PR branch (not the original branch)
  let current_branch_output = git(&ws.path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
  let current_branch = String::from_utf8_lossy(&current_branch_output.stdout)
    .trim()
    .to_string();

  assert!(
    current_branch.starts_with("cargo-rail-sync-mylib-"),
    "Should be on a PR branch starting with cargo-rail-sync-mylib-, but got: {}",
    current_branch
  );
  assert_ne!(current_branch, original_branch, "Should not be on the original branch");

  // Verify the change is on the PR branch
  let mono_content = std::fs::read_to_string(ws.path.join("crates/mylib/src/lib.rs"))?;
  assert!(
    mono_content.contains("// Changed in split"),
    "Change should be on PR branch"
  );

  // Verify commit has Rail-Origin trailer
  let log_output = git(&ws.path, &["log", "-1", "--format=%B"])?;
  let log = String::from_utf8_lossy(&log_output.stdout);
  assert!(log.contains("Rail-Origin: remote@"), "Should have Rail-Origin trailer");

  // Switch back to original branch and verify it's unchanged
  git(&ws.path, &["checkout", &original_branch])?;
  let original_content = std::fs::read_to_string(ws.path.join("crates/mylib/src/lib.rs"))?;
  assert!(
    !original_content.contains("// Changed in split"),
    "Original branch should not have the change"
  );

  Ok(())
}

#[test]
fn test_sync_roundtrip_preserves_content() -> Result<()> {
  let (ws, split_dir) = setup_split_scenario("mylib")?;

  let original = "pub fn test() -> i32 { 42 }";

  // Set content in mono
  ws.modify_file("mylib", "src/lib.rs", original)?;
  ws.commit("Set test function")?;

  // Sync to split
  run_cargo_rail(&ws.path, &["rail", "sync", "mylib", "--to-remote"])?;

  // Verify in split
  let split_content = std::fs::read_to_string(split_dir.path().join("src/lib.rs"))?;
  assert_eq!(split_content, original, "Split should have original content");

  // Sync back from split (should be no-op, but creates PR branch)
  run_cargo_rail(&ws.path, &["rail", "sync", "mylib", "--from-remote"])?;

  // Verify still matches (on PR branch)
  let final_content = std::fs::read_to_string(ws.path.join("crates/mylib/src/lib.rs"))?;
  assert_eq!(final_content, original, "Content should be preserved after roundtrip");

  Ok(())
}

#[test]
fn test_sync_multiple_commits() -> Result<()> {
  let (ws, split_dir) = setup_split_scenario("mylib")?;

  // Make multiple commits in mono
  ws.modify_file("mylib", "src/lib.rs", "// Version 1")?;
  ws.commit("Update v1")?;

  ws.modify_file("mylib", "src/lib.rs", "// Version 2")?;
  ws.commit("Update v2")?;

  ws.modify_file("mylib", "src/lib.rs", "// Version 3")?;
  ws.commit("Update v3")?;

  // Sync all to remote
  run_cargo_rail(&ws.path, &["rail", "sync", "mylib", "--to-remote"])?;

  // Check that all commits are in split
  let log_output = git(split_dir.path(), &["log", "--oneline"])?;
  let log = String::from_utf8_lossy(&log_output.stdout);

  assert!(log.contains("Update v1"), "Should have v1 commit");
  assert!(log.contains("Update v2"), "Should have v2 commit");
  assert!(log.contains("Update v3"), "Should have v3 commit");

  // Verify final content
  let content = std::fs::read_to_string(split_dir.path().join("src/lib.rs"))?;
  assert_eq!(content, "// Version 3", "Should have final version");

  Ok(())
}

#[test]
fn test_sync_preserves_commit_order() -> Result<()> {
  let (ws, split_dir) = setup_split_scenario("mylib")?;

  // Create a specific sequence
  ws.modify_file("mylib", "README.md", "# Version 1\n")?;
  ws.commit("Update README v1")?;

  ws.modify_file("mylib", "src/lib.rs", "// Code v1")?;
  ws.commit("Update code v1")?;

  ws.modify_file("mylib", "README.md", "# Version 2\n")?;
  ws.commit("Update README v2")?;

  // Sync to split
  run_cargo_rail(&ws.path, &["rail", "sync", "mylib", "--to-remote"])?;

  // Get split commit history
  let log_output = git(split_dir.path(), &["log", "--reverse", "--format=%s"])?;
  let log_str = String::from_utf8_lossy(&log_output.stdout);
  let commits: Vec<&str> = log_str.lines().collect();

  // Find positions
  let pos1 = commits.iter().position(|s| s.contains("Update README v1"));
  let pos2 = commits.iter().position(|s| s.contains("Update code v1"));
  let pos3 = commits.iter().position(|s| s.contains("Update README v2"));

  assert!(
    pos1.is_some() && pos2.is_some() && pos3.is_some(),
    "All commits should be present"
  );
  assert!(pos1 < pos2, "README v1 should come before code v1");
  assert!(pos2 < pos3, "Code v1 should come before README v2");

  Ok(())
}

#[test]
fn test_sync_skips_already_synced_commits() -> Result<()> {
  let (ws, split_dir) = setup_split_scenario("mylib")?;

  // First sync
  ws.modify_file("mylib", "src/lib.rs", "// First change")?;
  ws.commit("First change")?;

  run_cargo_rail(&ws.path, &["rail", "sync", "mylib", "--to-remote"])?;

  // Get commit count
  let log1 = git(split_dir.path(), &["log", "--oneline"])?;
  let count1 = String::from_utf8_lossy(&log1.stdout).lines().count();

  // Second sync with new change
  ws.modify_file("mylib", "src/lib.rs", "// Second change")?;
  ws.commit("Second change")?;

  run_cargo_rail(&ws.path, &["rail", "sync", "mylib", "--to-remote"])?;

  // Get new commit count
  let log2 = git(split_dir.path(), &["log", "--oneline"])?;
  let count2 = String::from_utf8_lossy(&log2.stdout).lines().count();

  // Should have exactly one more commit
  assert_eq!(count2, count1 + 1, "Should add exactly one new commit");

  Ok(())
}

/// Test sync --strategy ours flag
#[test]
fn test_sync_strategy_ours() -> Result<()> {
  let (ws, _split_dir) = setup_split_scenario("strategy-lib")?;

  // Make a change in monorepo
  ws.modify_file("strategy-lib", "src/lib.rs", "// Monorepo change")?;
  ws.commit("Monorepo change")?;

  // Sync with --strategy ours
  let output = run_cargo_rail(
    &ws.path,
    &["rail", "sync", "strategy-lib", "--to-remote", "--strategy", "ours"],
  )?;

  assert!(
    output.status.success(),
    "sync --strategy ours should succeed. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

/// Test sync --strategy theirs flag
#[test]
fn test_sync_strategy_theirs() -> Result<()> {
  let (ws, _split_dir) = setup_split_scenario("theirs-lib")?;

  // Sync with --strategy theirs
  let output = run_cargo_rail(
    &ws.path,
    &["rail", "sync", "theirs-lib", "--to-remote", "--strategy", "theirs"],
  )?;

  assert!(
    output.status.success(),
    "sync --strategy theirs should succeed. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

/// Test sync --json output
#[test]
fn test_sync_json_output() -> Result<()> {
  let (ws, _split_dir) = setup_split_scenario("json-lib")?;

  // Run sync with --check and --json
  let output = run_cargo_rail(&ws.path, &["rail", "sync", "json-lib", "--check", "--json"])?;

  if output.status.success() {
    let stdout = String::from_utf8_lossy(&output.stdout);
    // If successful, should be valid JSON (or empty if no changes)
    if !stdout.trim().is_empty() {
      let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
      assert!(parsed.is_ok(), "JSON output should be valid. stdout: {}", stdout);
    }
  }

  Ok(())
}
