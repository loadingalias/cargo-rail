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
  run_cargo_rail(
    &ws.path,
    &["rail", "split", "run", crate_name, "--yes", "--allow-dirty"],
  )?;

  Ok((ws, split_dir))
}

#[test]
fn test_sync_to_remote_basic() -> Result<()> {
  let (ws, split_dir) = setup_split_scenario("mylib")?;

  // Make change in monorepo
  ws.modify_file("mylib", "src/lib.rs", "// Changed in mono")?;
  ws.commit("Update mylib in mono")?;

  // Sync to remote
  run_cargo_rail(
    &ws.path,
    &["rail", "sync", "mylib", "--to-remote", "--yes", "--allow-dirty"],
  )?;

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
  run_cargo_rail(
    &ws.path,
    &["rail", "sync", "mylib", "--from-remote", "--yes", "--allow-dirty"],
  )?;

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
  run_cargo_rail(
    &ws.path,
    &["rail", "sync", "mylib", "--from-remote", "--yes", "--allow-dirty"],
  )?;

  // Verify we're on a PR branch (not the original branch)
  let current_branch_output = git(&ws.path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
  let current_branch = String::from_utf8_lossy(&current_branch_output.stdout)
    .trim()
    .to_string();

  // Deterministic branch naming (no timestamp) for idempotency
  assert_eq!(
    current_branch, "cargo-rail-sync-mylib",
    "Should be on a PR branch named cargo-rail-sync-mylib, but got: {}",
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
  run_cargo_rail(
    &ws.path,
    &["rail", "sync", "mylib", "--to-remote", "--yes", "--allow-dirty"],
  )?;

  // Verify in split
  let split_content = std::fs::read_to_string(split_dir.path().join("src/lib.rs"))?;
  assert_eq!(split_content, original, "Split should have original content");

  // Sync back from split (should be no-op, but creates PR branch)
  run_cargo_rail(
    &ws.path,
    &["rail", "sync", "mylib", "--from-remote", "--yes", "--allow-dirty"],
  )?;

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
  run_cargo_rail(
    &ws.path,
    &["rail", "sync", "mylib", "--to-remote", "--yes", "--allow-dirty"],
  )?;

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
  run_cargo_rail(
    &ws.path,
    &["rail", "sync", "mylib", "--to-remote", "--yes", "--allow-dirty"],
  )?;

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

  run_cargo_rail(
    &ws.path,
    &["rail", "sync", "mylib", "--to-remote", "--yes", "--allow-dirty"],
  )?;

  // Get commit count
  let log1 = git(split_dir.path(), &["log", "--oneline"])?;
  let count1 = String::from_utf8_lossy(&log1.stdout).lines().count();

  // Second sync with new change
  ws.modify_file("mylib", "src/lib.rs", "// Second change")?;
  ws.commit("Second change")?;

  run_cargo_rail(
    &ws.path,
    &["rail", "sync", "mylib", "--to-remote", "--yes", "--allow-dirty"],
  )?;

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
    &[
      "rail",
      "sync",
      "strategy-lib",
      "--to-remote",
      "--strategy",
      "ours",
      "--yes",
      "--allow-dirty",
    ],
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
    &[
      "rail",
      "sync",
      "theirs-lib",
      "--to-remote",
      "--strategy",
      "theirs",
      "--yes",
      "--allow-dirty",
    ],
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

// ============================================================================
// Safety Rails Tests
// ============================================================================

/// Test that sync fails on dirty worktree without --allow-dirty
#[test]
fn test_sync_dirty_worktree_error() -> Result<()> {
  let (ws, _split_dir) = setup_split_scenario("dirty-sync-lib")?;

  // Make a change in mono that we want to sync
  ws.modify_file("dirty-sync-lib", "src/lib.rs", "// Changed in mono")?;
  ws.commit("Update in mono")?;

  // Make worktree dirty by adding an uncommitted file
  std::fs::write(ws.path.join("dirty.txt"), "uncommitted content")?;

  // Run sync WITHOUT --allow-dirty - should fail
  let output = run_cargo_rail(&ws.path, &["rail", "sync", "dirty-sync-lib", "--to-remote", "--yes"])?;

  assert!(
    !output.status.success(),
    "Sync should fail on dirty worktree without --allow-dirty"
  );

  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("uncommitted changes") || stderr.contains("dirty"),
    "Error should mention uncommitted changes. stderr: {}",
    stderr
  );

  Ok(())
}

/// Test that --allow-dirty bypasses the dirty worktree check for sync
#[test]
fn test_sync_allow_dirty_bypasses_check() -> Result<()> {
  let (ws, split_dir) = setup_split_scenario("allow-dirty-sync-lib")?;

  // Make a change in mono that we want to sync
  ws.modify_file("allow-dirty-sync-lib", "src/lib.rs", "// Changed in mono")?;
  ws.commit("Update in mono")?;

  // Make worktree dirty
  std::fs::write(ws.path.join("dirty.txt"), "uncommitted content")?;

  // Run sync WITH --allow-dirty - should succeed
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "sync",
      "allow-dirty-sync-lib",
      "--to-remote",
      "--yes",
      "--allow-dirty",
    ],
  )?;

  assert!(
    output.status.success(),
    "Sync should succeed with --allow-dirty. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  // Verify sync happened
  let split_content = std::fs::read_to_string(split_dir.path().join("src/lib.rs"))?;
  assert!(
    split_content.contains("// Changed in mono"),
    "Split should have the synced change"
  );

  Ok(())
}

/// Test that sync to remote is idempotent - running twice doesn't duplicate commits
#[test]
fn test_sync_to_remote_idempotent() -> Result<()> {
  let (ws, split_dir) = setup_split_scenario("sync-idempotent-lib")?;

  // Make a change in mono
  ws.modify_file("sync-idempotent-lib", "src/lib.rs", "// Synced change")?;
  ws.commit("Sync this change")?;

  // First sync to remote
  let output1 = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "sync",
      "sync-idempotent-lib",
      "--to-remote",
      "--yes",
      "--allow-dirty",
    ],
  )?;
  assert!(
    output1.status.success(),
    "First sync should succeed. stderr: {}",
    String::from_utf8_lossy(&output1.stderr)
  );

  // Get commit count after first sync
  let log1 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
  let commit_count1: usize = String::from_utf8_lossy(&log1.stdout).trim().parse()?;

  // Second sync (should be no-op)
  let output2 = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "sync",
      "sync-idempotent-lib",
      "--to-remote",
      "--yes",
      "--allow-dirty",
    ],
  )?;
  assert!(
    output2.status.success(),
    "Second sync should succeed. stderr: {}",
    String::from_utf8_lossy(&output2.stderr)
  );

  // Verify "no new commits" message
  let stdout2 = String::from_utf8_lossy(&output2.stdout);
  assert!(
    stdout2.contains("No new commits") || stdout2.contains("0 commits"),
    "Second sync should indicate nothing to sync. stdout: {}",
    stdout2
  );

  // Get commit count after second sync
  let log2 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
  let commit_count2: usize = String::from_utf8_lossy(&log2.stdout).trim().parse()?;

  // Verify no new commits were created
  assert_eq!(
    commit_count1, commit_count2,
    "Commit count should not change on second sync"
  );

  Ok(())
}

/// Test that sync from remote uses deterministic PR branch name (not timestamp-based)
#[test]
fn test_sync_from_remote_deterministic_branch() -> Result<()> {
  let (ws, split_dir) = setup_split_scenario("deterministic-branch-lib")?;

  // Make a change in the split repo
  std::fs::write(split_dir.path().join("src/lib.rs"), "// Changed in split repo")?;
  git(split_dir.path(), &["add", "."])?;
  git(split_dir.path(), &["commit", "-m", "Change from split"])?;

  // First sync from remote - should create PR branch
  let output1 = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "sync",
      "deterministic-branch-lib",
      "--from-remote",
      "--yes",
      "--allow-dirty",
    ],
  )?;
  assert!(
    output1.status.success(),
    "First sync from remote should succeed. stderr: {}",
    String::from_utf8_lossy(&output1.stderr)
  );

  // Check the PR branch name - should NOT contain timestamp
  let branches = git(&ws.path, &["branch"])?;
  let branches_str = String::from_utf8_lossy(&branches.stdout);

  // The branch should be "cargo-rail-sync-deterministic-branch-lib" (no timestamp)
  assert!(
    branches_str.contains("cargo-rail-sync-deterministic-branch-lib"),
    "Should have deterministic PR branch name. branches: {}",
    branches_str
  );

  // Count branches with our pattern - should be exactly one
  let branch_count = branches_str
    .lines()
    .filter(|b| b.contains("cargo-rail-sync-deterministic-branch-lib"))
    .count();
  assert_eq!(
    branch_count, 1,
    "Should have exactly one PR branch, got {}",
    branch_count
  );

  Ok(())
}

/// Test that sync from remote is idempotent - second run reuses existing branch
#[test]
fn test_sync_from_remote_idempotent() -> Result<()> {
  let (ws, split_dir) = setup_split_scenario("sync-from-idempotent-lib")?;

  // Commit rail.toml so it persists across branch switches
  git(&ws.path, &["add", "rail.toml"])?;
  git(&ws.path, &["commit", "-m", "Add rail.toml"])?;

  // Make a change in the split repo
  std::fs::write(
    split_dir.path().join("src/lib.rs"),
    "// Changed in split for idempotency test",
  )?;
  git(split_dir.path(), &["add", "."])?;
  git(split_dir.path(), &["commit", "-m", "Change for idempotency"])?;

  // First sync from remote
  let output1 = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "sync",
      "sync-from-idempotent-lib",
      "--from-remote",
      "--yes",
      "--allow-dirty",
    ],
  )?;
  assert!(
    output1.status.success(),
    "First sync should succeed. stderr: {}",
    String::from_utf8_lossy(&output1.stderr)
  );

  // Return to main branch for second run
  git(&ws.path, &["checkout", "main"])?;

  // Second sync from remote (should be no-op since commit already synced)
  let output2 = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "sync",
      "sync-from-idempotent-lib",
      "--from-remote",
      "--yes",
      "--allow-dirty",
    ],
  )?;
  assert!(
    output2.status.success(),
    "Second sync should succeed. stderr: {}",
    String::from_utf8_lossy(&output2.stderr)
  );

  // Verify "already up-to-date" or similar message
  let stdout2 = String::from_utf8_lossy(&output2.stdout);
  assert!(
    stdout2.contains("already up-to-date") || stdout2.contains("No new commits") || stdout2.contains("0 commits"),
    "Second sync should indicate nothing to sync. stdout: {}",
    stdout2
  );

  // Count PR branches - should still be exactly one (not two)
  let branches = git(&ws.path, &["branch"])?;
  let branches_str = String::from_utf8_lossy(&branches.stdout);
  let branch_count = branches_str
    .lines()
    .filter(|b| b.contains("cargo-rail-sync-sync-from-idempotent-lib"))
    .count();
  assert_eq!(
    branch_count, 1,
    "Should still have exactly one PR branch after two syncs. branches: {}",
    branches_str
  );

  Ok(())
}

/// Test that bidirectional sync is idempotent
#[test]
fn test_sync_bidirectional_idempotent() -> Result<()> {
  let (ws, split_dir) = setup_split_scenario("bidir-idempotent-lib")?;

  // Commit rail.toml so it persists across branch switches
  git(&ws.path, &["add", "rail.toml"])?;
  git(&ws.path, &["commit", "-m", "Add rail.toml"])?;

  // Make a change in mono
  ws.modify_file("bidir-idempotent-lib", "src/lib.rs", "// Mono change")?;
  ws.commit("Change from mono")?;

  // Make a change in split repo
  std::fs::write(split_dir.path().join("README.md"), "# Updated from split repo")?;
  git(split_dir.path(), &["add", "."])?;
  git(split_dir.path(), &["commit", "-m", "README from split"])?;

  // First bidirectional sync
  let output1 = run_cargo_rail(
    &ws.path,
    &["rail", "sync", "bidir-idempotent-lib", "--yes", "--allow-dirty"],
  )?;
  assert!(
    output1.status.success(),
    "First bidirectional sync should succeed. stderr: {}",
    String::from_utf8_lossy(&output1.stderr)
  );

  // Get commit counts
  let _mono_log1 = git(&ws.path, &["rev-list", "--count", "HEAD"])?;

  let split_log1 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
  let split_count1: usize = String::from_utf8_lossy(&split_log1.stdout).trim().parse()?;

  // Return to main branch if on PR branch
  let current = git(&ws.path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
  let current_branch = String::from_utf8_lossy(&current.stdout).trim().to_string();
  if current_branch.starts_with("cargo-rail-sync") {
    git(&ws.path, &["checkout", "main"])?;
  }

  // Second bidirectional sync (should be no-op on both sides)
  let output2 = run_cargo_rail(
    &ws.path,
    &["rail", "sync", "bidir-idempotent-lib", "--yes", "--allow-dirty"],
  )?;
  assert!(
    output2.status.success(),
    "Second bidirectional sync should succeed. stderr: {}",
    String::from_utf8_lossy(&output2.stderr)
  );

  // Verify "no changes" or similar message
  let stdout2 = String::from_utf8_lossy(&output2.stdout);
  assert!(
    stdout2.contains("No changes") || stdout2.contains("No new commits") || stdout2.contains("0 commits"),
    "Second sync should indicate nothing to sync. stdout: {}",
    stdout2
  );

  // Get commit counts after second sync
  let split_log2 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
  let split_count2: usize = String::from_utf8_lossy(&split_log2.stdout).trim().parse()?;

  // Split repo should not have new commits
  assert_eq!(
    split_count1, split_count2,
    "Split repo commit count should not change on second sync"
  );

  Ok(())
}

/// Test sync from remote when PR branch already exists (second sync adds to existing branch)
#[test]
fn test_sync_from_remote_pr_branch_exists_adds_commits() -> Result<()> {
  let (ws, split_dir) = setup_split_scenario("pr-branch-add-lib")?;

  // Add .gitignore to prevent untracked file conflicts during branch switching
  std::fs::write(ws.path.join(".gitignore"), "Cargo.lock\ntarget/\n")?;
  git(&ws.path, &["add", ".gitignore"])?;

  // Commit rail.toml and .gitignore so they persist across branch switches
  git(&ws.path, &["add", "rail.toml"])?;
  git(&ws.path, &["commit", "-m", "Add rail.toml and .gitignore"])?;

  // Make first change in split repo
  std::fs::write(split_dir.path().join("src/lib.rs"), "// First from split")?;
  git(split_dir.path(), &["add", "."])?;
  git(split_dir.path(), &["commit", "-m", "First change from split"])?;

  // First sync - creates PR branch
  let output1 = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "sync",
      "pr-branch-add-lib",
      "--from-remote",
      "--yes",
      "--allow-dirty",
    ],
  )?;
  assert!(
    output1.status.success(),
    "First sync should succeed. stderr: {}",
    String::from_utf8_lossy(&output1.stderr)
  );

  // Record state after first sync
  let pr_branch_name = "cargo-rail-sync-pr-branch-add-lib";
  let pr_head_after_first = git(&ws.path, &["rev-parse", pr_branch_name])?;
  let pr_sha_first = String::from_utf8_lossy(&pr_head_after_first.stdout).trim().to_string();

  // Go back to main
  git(&ws.path, &["checkout", "main"])?;

  // Make second change in split repo
  std::fs::write(split_dir.path().join("src/lib.rs"), "// Second from split")?;
  git(split_dir.path(), &["add", "."])?;
  git(split_dir.path(), &["commit", "-m", "Second change from split"])?;

  // Second sync - should add to existing PR branch
  let output2 = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "sync",
      "pr-branch-add-lib",
      "--from-remote",
      "--yes",
      "--allow-dirty",
    ],
  )?;
  assert!(
    output2.status.success(),
    "Second sync should succeed. stderr: {}",
    String::from_utf8_lossy(&output2.stderr)
  );

  // Verify we're on the PR branch
  let current = git(&ws.path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
  let current_branch = String::from_utf8_lossy(&current.stdout).trim().to_string();
  assert_eq!(current_branch, pr_branch_name, "Should be on the existing PR branch");

  // Verify the new content from split is there
  let content = std::fs::read_to_string(ws.path.join("crates/pr-branch-add-lib/src/lib.rs"))?;
  assert!(
    content.contains("Second from split"),
    "Should have the second content from split"
  );

  // Verify the branch has moved (new commit added)
  let pr_head_after_second = git(&ws.path, &["rev-parse", pr_branch_name])?;
  let pr_sha_second = String::from_utf8_lossy(&pr_head_after_second.stdout).trim().to_string();
  assert_ne!(
    pr_sha_first, pr_sha_second,
    "PR branch HEAD should have changed after second sync"
  );

  // Verify there's still only one PR branch (not a new one created)
  let branches = git(&ws.path, &["branch"])?;
  let branches_str = String::from_utf8_lossy(&branches.stdout);
  let branch_count = branches_str
    .lines()
    .filter(|b| b.contains("cargo-rail-sync-pr-branch-add-lib"))
    .count();
  assert_eq!(branch_count, 1, "Should still have exactly one PR branch");

  Ok(())
}

/// Test sync to remote is idempotent even with multiple runs in succession
#[test]
fn test_sync_to_remote_multiple_runs_idempotent() -> Result<()> {
  let (ws, split_dir) = setup_split_scenario("multi-run-lib")?;

  // Make a single change
  ws.modify_file("multi-run-lib", "src/lib.rs", "// Single change")?;
  ws.commit("Single change")?;

  // Run sync 3 times in succession
  for i in 1..=3 {
    let output = run_cargo_rail(
      &ws.path,
      &["rail", "sync", "multi-run-lib", "--to-remote", "--yes", "--allow-dirty"],
    )?;
    assert!(
      output.status.success(),
      "Sync run {} should succeed. stderr: {}",
      i,
      String::from_utf8_lossy(&output.stderr)
    );
  }

  // Verify split repo has exactly the right number of commits (initial + 1 change)
  let log = git(split_dir.path(), &["log", "--oneline"])?;
  let log_str = String::from_utf8_lossy(&log.stdout);

  // Should have initial commit + the "Single change" commit synced once
  // (Plus potentially auxiliary files commit from initial split)
  // The key assertion is that running 3 times doesn't create 3 copies
  let single_change_count = log_str.lines().filter(|l| l.contains("Single change")).count();
  assert_eq!(
    single_change_count, 1,
    "Should have exactly one 'Single change' commit, not multiple. Log:\n{}",
    log_str
  );

  Ok(())
}
