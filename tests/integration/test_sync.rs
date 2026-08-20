//! Integration tests for sync operations

use crate::helpers::{TestWorkspace, git, run_cargo_rail};
use anyhow::Result;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Command;
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

/// Helper to set up a combined split scenario with two crates.
fn setup_combined_split_scenario(group_name: &str, crate_a: &str, crate_b: &str) -> Result<(TestWorkspace, TempDir)> {
    let ws = TestWorkspace::new()?;
    ws.add_crate(crate_a, "0.1.0", &[])?;
    ws.add_crate(crate_b, "0.1.0", &[])?;
    ws.commit(&format!("Initial {} group", group_name))?;

    let split_dir = TempDir::new()?;
    let config = format!(
        r#"[workspace]
root = "."

[crates.{0}.split]
remote = "{1}"
branch = "main"
mode = "combined"
members = ["{2}", "{3}"]
"#,
        group_name,
        split_dir.path().display().to_string().replace('\\', "\\\\"),
        crate_a,
        crate_b
    );
    std::fs::write(ws.path.join("rail.toml"), config)?;

    run_cargo_rail(
        &ws.path,
        &["rail", "split", "run", group_name, "--yes", "--allow-dirty"],
    )?;

    Ok((ws, split_dir))
}

#[test]
fn test_sync_requires_explicit_confirmation_non_interactive() {
    let result: Result<()> = (|| {
        let (ws, _split_dir) = setup_split_scenario("confirm-sync")?;

        ws.modify_file("confirm-sync", "src/lib.rs", "// change requiring sync")?;
        ws.commit("Update confirm-sync in mono")?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "sync", "confirm-sync", "--to-remote", "--allow-dirty"],
        )?;
        assert!(
            !output.status.success(),
            "sync should fail without --yes/--plan in non-interactive mode"
        );

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("explicit confirmation") && stderr.contains("--yes") && stderr.contains("--plan"),
            "safety gate message missing expected guidance.\nstderr:\n{}",
            stderr
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_to_remote_basic() {
    let result: Result<()> = (|| {
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
        assert!(
            log.contains("Rail-Origin: v1 source="),
            "Should have Rail-Origin trailer"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_sync_to_remote_preserves_exact_tree_and_commit_metadata() {
    let result: Result<()> = (|| {
        let (ws, split_dir) = setup_split_scenario("exact-sync")?;
        let crate_root = ws.path.join("crates/exact-sync");
        std::fs::create_dir_all(crate_root.join("bin"))?;
        let tool = crate_root.join("bin/tool");
        std::fs::write(&tool, "#!/bin/sh\necho exact\n")?;
        let mut permissions = std::fs::metadata(&tool)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&tool, permissions)?;
        std::os::unix::fs::symlink("../bin/tool", crate_root.join("src/tool-link"))?;
        std::fs::rename(crate_root.join("README.md"), crate_root.join("GUIDE.md"))?;
        git(&ws.path, &["add", "-A"])?;
        let status = Command::new("git")
            .current_dir(&ws.path)
            .env("GIT_AUTHOR_NAME", "Exact Author")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0530")
            .env("GIT_COMMITTER_NAME", "Exact Committer")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_DATE", "1700000123 -0700")
            .args(["commit", "-m", "Preserve exact Git state"])
            .status()?;
        assert!(status.success());

        let metadata_format = "%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI";
        let source_metadata = git(
            &ws.path,
            &["show", "-s", &format!("--format={metadata_format}"), "HEAD"],
        )?;
        run_cargo_rail(
            &ws.path,
            &["rail", "sync", "exact-sync", "--to-remote", "--yes", "--allow-dirty"],
        )?;
        let target_metadata = git(
            split_dir.path(),
            &["show", "-s", &format!("--format={metadata_format}"), "HEAD"],
        )?;
        assert_eq!(source_metadata.stdout, target_metadata.stdout);

        let executable = git(split_dir.path(), &["ls-tree", "HEAD", "bin/tool"])?;
        assert!(String::from_utf8_lossy(&executable.stdout).starts_with("100755 blob "));
        let symlink = git(split_dir.path(), &["ls-tree", "HEAD", "src/tool-link"])?;
        assert!(String::from_utf8_lossy(&symlink.stdout).starts_with("120000 blob "));
        assert_eq!(
            std::fs::read_link(split_dir.path().join("src/tool-link"))?,
            PathBuf::from("../bin/tool")
        );
        assert!(!split_dir.path().join("README.md").exists());
        assert!(split_dir.path().join("GUIDE.md").exists());

        let source_blob = git(&ws.path, &["rev-parse", "HEAD:crates/exact-sync/bin/tool"])?;
        let target_blob = git(split_dir.path(), &["rev-parse", "HEAD:bin/tool"])?;
        assert_eq!(source_blob.stdout, target_blob.stdout);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_explicit_non_cargo_asset_roundtrips_at_its_owned_path() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("asset-sync", "0.1.0", &[])?;
        std::fs::write(ws.path.join("NOTICE"), "initial\n")?;
        ws.commit("Add crate and explicit asset")?;
        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.asset-sync.split]
remote = "{}"
branch = "main"
mode = "single"
include = ["NOTICE"]
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;
        run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "asset-sync", "--yes", "--allow-dirty"],
        )?;
        assert_eq!(git(split_dir.path(), &["show", "HEAD:NOTICE"])?.stdout, b"initial\n");
        git(split_dir.path(), &["diff", "--quiet", "--", "NOTICE"])?;

        std::fs::write(ws.path.join("NOTICE"), "from mono\n")?;
        ws.commit("Update explicit asset in mono")?;
        run_cargo_rail(
            &ws.path,
            &["rail", "sync", "asset-sync", "--to-remote", "--yes", "--allow-dirty"],
        )?;
        assert_eq!(git(split_dir.path(), &["show", "HEAD:NOTICE"])?.stdout, b"from mono\n");
        git(split_dir.path(), &["diff", "--quiet", "--", "NOTICE"])?;

        std::fs::write(split_dir.path().join("NOTICE"), "from split\n")?;
        git(split_dir.path(), &["add", "NOTICE"])?;
        git(split_dir.path(), &["commit", "-m", "Update explicit asset in split"])?;
        run_cargo_rail(
            &ws.path,
            &["rail", "sync", "asset-sync", "--from-remote", "--yes", "--allow-dirty"],
        )?;
        assert_eq!(git(&ws.path, &["show", "HEAD:NOTICE"])?.stdout, b"from split\n");
        git(&ws.path, &["diff", "--quiet", "--", "NOTICE"])?;
        assert!(!ws.path.join("crates/asset-sync/NOTICE").exists());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_preserves_committed_source_paths_named_target() {
    let result: Result<()> = (|| {
        let (ws, split_dir) = setup_split_scenario("target-named-source")?;
        std::fs::write(ws.path.join(".gitignore"), "ignored-state/\n")?;
        let source = ws.path.join("crates/target-named-source/docs/target/example.txt");
        std::fs::create_dir_all(source.parent().expect("source fixture must have a parent"))?;
        std::fs::write(&source, "intentional source\n")?;
        ws.commit("Add target-named source path")?;

        run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "target-named-source",
                "--to-remote",
                "--yes",
                "--allow-dirty",
            ],
        )?;

        assert_eq!(
            git(split_dir.path(), &["show", "HEAD:docs/target/example.txt"])?.stdout,
            b"intentional source\n"
        );
        git(split_dir.path(), &["diff", "--quiet", "--", "docs/target/example.txt"])?;
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_from_remote_basic() {
    let result: Result<()> = (|| {
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
        assert!(
            log.contains("Rail-Origin: v1 source="),
            "Should have Rail-Origin trailer"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_manual_conflict_stops_before_commit_and_resumes_from_receipt() {
    let result: Result<()> = (|| {
        let (ws, split_dir) = setup_split_scenario("manual-conflict-lib")?;

        ws.modify_file(
            "manual-conflict-lib",
            "src/lib.rs",
            "pub fn value() -> &'static str {\n  \"mono\"\n}\n",
        )?;
        ws.commit("Conflicting monorepo edit")?;
        let recovery_parent = git(&ws.path, &["rev-parse", "HEAD"])?;
        let recovery_parent = String::from_utf8_lossy(&recovery_parent.stdout).trim().to_string();

        std::fs::write(
            split_dir.path().join("src/lib.rs"),
            "pub fn value() -> &'static str {\n  \"split\"\n}\n",
        )?;
        git(split_dir.path(), &["add", "src/lib.rs"])?;
        git(split_dir.path(), &["commit", "-m", "Conflicting split edit"])?;

        let conflicted = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "manual-conflict-lib",
                "--from-remote",
                "--yes",
                "--allow-dirty",
            ],
        )?;
        assert_eq!(
            conflicted.status.code(),
            Some(1),
            "manual conflict must use exit code 1"
        );
        let stdout = String::from_utf8_lossy(&conflicted.stdout);
        assert!(stdout.contains("sync conflicted"), "stdout:\n{stdout}");

        let head = git(&ws.path, &["rev-parse", "HEAD"])?;
        assert_eq!(
            String::from_utf8_lossy(&head.stdout).trim(),
            recovery_parent,
            "unresolved content must never be committed"
        );
        let conflicted_path = ws.path.join("crates/manual-conflict-lib/src/lib.rs");
        let content = std::fs::read_to_string(&conflicted_path)?;
        assert!(
            content.contains("<<<<<<<"),
            "manual worktree should preserve merge markers"
        );

        let receipts = ws.path.join("target/cargo-rail/receipts");
        let receipt = std::fs::read_dir(&receipts)?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("sync-conflict-manual-conflict-lib-"))
            })
            .ok_or_else(|| anyhow::anyhow!("missing sync conflict receipt"))?;
        let receipt_json: serde_json::Value = serde_json::from_slice(&std::fs::read(&receipt)?)?;
        assert_eq!(receipt_json["status"], "conflicted");
        assert_eq!(receipt_json["conflicts"][0]["class"], "content");

        std::fs::write(
            &conflicted_path,
            "pub fn value() -> &'static str {\n  \"resolved\"\n}\n",
        )?;
        let resumed = run_cargo_rail(&ws.path, &["rail", "sync", "--resume", receipt.to_str().unwrap()])?;
        assert!(
            resumed.status.success(),
            "resume failed:\n{}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        let log = git(&ws.path, &["log", "-1", "--format=%B"])?;
        assert!(String::from_utf8_lossy(&log.stdout).contains("Rail-Origin: v1 source="));
        let receipt_json: serde_json::Value = serde_json::from_slice(&std::fs::read(&receipt)?)?;
        assert_eq!(receipt_json["status"], "resolved");
        assert!(!std::fs::read_to_string(conflicted_path)?.contains("<<<<<<<"));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_from_remote_creates_pr_branch() {
    let result: Result<()> = (|| {
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
        assert!(
            log.contains("Rail-Origin: v1 source="),
            "Should have Rail-Origin trailer"
        );

        // Switch back to original branch and verify it's unchanged
        git(&ws.path, &["checkout", &original_branch])?;
        let original_content = std::fs::read_to_string(ws.path.join("crates/mylib/src/lib.rs"))?;
        assert!(
            !original_content.contains("// Changed in split"),
            "Original branch should not have the change"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_roundtrip_preserves_content() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_multiple_commits() {
    let result: Result<()> = (|| {
        let (ws, split_dir) = setup_split_scenario("mylib")?;

        // Make multiple commits in mono
        ws.modify_file("mylib", "src/lib.rs", "// Version 1")?;
        let object_id_like_path = "a".repeat(40);
        ws.modify_file("mylib", &object_id_like_path, "valid Git filename")?;
        ws.commit("Update v1")?;

        ws.modify_file("mylib", "src/lib.rs", "// Version 2")?;
        ws.commit("Update v2")?;

        ws.modify_file("mylib", "src/lib.rs", "// Version 3")?;
        ws.commit("Update v3")?;

        // Sync all to remote
        let diagnostics_dir = TempDir::new()?;
        let diagnostics = diagnostics_dir.path().join("sync.json");
        run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "--diagnostics-file",
                diagnostics.to_str().unwrap(),
                "sync",
                "mylib",
                "--to-remote",
                "--yes",
                "--allow-dirty",
            ],
        )?;
        let counters: serde_json::Value = serde_json::from_slice(&std::fs::read(diagnostics)?)?;
        assert_eq!(counters["git_path_change_reads"], 3);
        assert_eq!(counters["git_path_change_batches"], 1);

        // Check that all commits are in split
        let log_output = git(split_dir.path(), &["log", "--oneline"])?;
        let log = String::from_utf8_lossy(&log_output.stdout);

        assert!(log.contains("Update v1"), "Should have v1 commit");
        assert!(log.contains("Update v2"), "Should have v2 commit");
        assert!(log.contains("Update v3"), "Should have v3 commit");

        // Verify final content
        let content = std::fs::read_to_string(split_dir.path().join("src/lib.rs"))?;
        assert_eq!(content, "// Version 3", "Should have final version");
        assert_eq!(
            std::fs::read_to_string(split_dir.path().join(object_id_like_path))?,
            "valid Git filename"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_preserves_commit_order() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_skips_already_synced_commits() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

/// Test sync --strategy ours flag
#[test]
fn test_sync_strategy_ours() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

/// Test sync --strategy theirs flag
#[test]
fn test_sync_strategy_theirs() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

/// Test sync --check JSON clean and pending classification.
#[test]
fn test_sync_json_output() {
    let result: Result<()> = (|| {
        let (ws, _split_dir) = setup_split_scenario("json-lib")?;

        let output = run_cargo_rail(&ws.path, &["rail", "sync", "json-lib", "--check", "--json"])?;
        assert_eq!(output.status.code(), Some(0));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
        assert!(parsed.is_ok(), "JSON output should be valid. stdout: {}", stdout);
        let json = parsed?;
        assert_eq!(json["schema_version"], serde_json::json!(1));
        assert_eq!(json["command"], serde_json::json!("sync"));
        assert_eq!(json["mode"], serde_json::json!("check"));
        assert_eq!(json["result"], serde_json::json!("clean"));
        assert_eq!(json["exit_code"], serde_json::json!(0));
        assert_eq!(json["crates"][0]["pending"], false);

        ws.modify_file("json-lib", "src/lib.rs", "// pending")?;
        ws.commit("Update json-lib")?;
        let pending = run_cargo_rail(
            &ws.path,
            &["rail", "sync", "json-lib", "--to-remote", "--check", "--json"],
        )?;
        assert_eq!(pending.status.code(), Some(1));
        let pending_json: serde_json::Value = serde_json::from_slice(&pending.stdout)?;
        assert_eq!(pending_json["result"], "pending_changes");
        assert_eq!(pending_json["exit_code"], 1);
        assert_eq!(pending_json["crates"][0]["pending"], true);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_combined_mode_syncs_all_configured_paths() {
    let result: Result<()> = (|| {
        let (ws, split_dir) = setup_combined_split_scenario("combined-sync", "combo-a", "combo-b")?;

        ws.modify_file("combo-a", "src/lib.rs", "// Changed combo-a")?;
        ws.modify_file("combo-b", "src/lib.rs", "// Changed combo-b")?;
        ws.commit("Update both combined crates")?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "sync", "combined-sync", "--to-remote", "--yes", "--allow-dirty"],
        )?;
        assert!(
            output.status.success(),
            "combined sync should succeed. stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let a_content = std::fs::read_to_string(split_dir.path().join("crates/combo-a/src/lib.rs"))?;
        let b_content = std::fs::read_to_string(split_dir.path().join("crates/combo-b/src/lib.rs"))?;
        assert!(
            a_content.contains("// Changed combo-a"),
            "combined sync should include changes from first configured path"
        );
        assert!(
            b_content.contains("// Changed combo-b"),
            "combined sync should include changes from second configured path"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_combined_mode_rejects_remote_paths_outside_ownership() {
    let result: Result<()> = (|| {
        let (ws, split_dir) = setup_combined_split_scenario("combined-boundary", "boundary-a", "boundary-b")?;
        std::fs::write(split_dir.path().join("outside.txt"), "not owned\n")?;
        git(split_dir.path(), &["add", "outside.txt"])?;
        git(
            split_dir.path(),
            &["commit", "-m", "Attempt out-of-scope combined change"],
        )?;

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "combined-boundary",
                "--from-remote",
                "--yes",
                "--allow-dirty",
            ],
        )?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("outside combined split ownership"));
        assert!(!ws.path.join("outside.txt").exists());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

// Safety Rails Tests

/// Test that sync fails on dirty worktree without --allow-dirty
#[test]
fn test_sync_dirty_worktree_error() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

/// Test that --allow-dirty bypasses the dirty worktree check for sync
#[test]
fn test_sync_allow_dirty_bypasses_check() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

/// Test that sync to remote is idempotent - running twice doesn't duplicate commits
#[test]
fn test_sync_to_remote_idempotent() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

/// Test that sync from remote uses deterministic PR branch name (not timestamp-based)
#[test]
fn test_sync_from_remote_deterministic_branch() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

/// Test that sync from remote is idempotent - second run reuses existing branch
#[test]
fn test_sync_from_remote_idempotent() {
    let result: Result<()> = (|| {
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

        // Verify "already up-to-date" or similar message (progress output goes to stderr)
        let stderr2 = String::from_utf8_lossy(&output2.stderr);
        assert!(
            stderr2.contains("already up-to-date")
                || stderr2.contains("No new commits")
                || stderr2.contains("0 commits"),
            "Second sync should indicate nothing to sync. stderr: {}",
            stderr2
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
    })();
    super::helpers::finish_test(result);
}

/// Test that bidirectional sync is idempotent
#[test]
fn test_sync_bidirectional_idempotent() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

/// Test sync from remote when PR branch already exists (second sync adds to existing branch)
#[test]
fn test_sync_from_remote_pr_branch_exists_adds_commits() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

/// Test sync to remote is idempotent even with multiple runs in succession
#[test]
fn test_sync_to_remote_multiple_runs_idempotent() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}
