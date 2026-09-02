//! Integration tests for sync operations

#[cfg(unix)]
use crate::helpers::cargo_rail_command;
use crate::helpers::{TestWorkspace, git, run_cargo_rail};
use anyhow::Result;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
use tempfile::TempDir;

#[cfg(unix)]
fn snapshot_directory(root: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    fn visit(root: &Path, current: &Path, output: &mut Vec<(PathBuf, Vec<u8>)>) -> Result<()> {
        for entry in std::fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                visit(root, &path, output)?;
            } else {
                output.push((path.strip_prefix(root)?.to_path_buf(), std::fs::read(&path)?));
            }
        }
        Ok(())
    }
    let mut output = Vec::new();
    visit(root, root, &mut output)?;
    output.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(output)
}

#[cfg(unix)]
fn post_success_nth_git_failure_wrapper(
    workspace: &Path,
    trigger: &str,
    marker_name: &str,
    failure_occurrence: usize,
) -> Result<(std::ffi::OsString, PathBuf, String, String, usize)> {
    anyhow::ensure!(
        failure_occurrence > 0,
        "Git wrapper failure occurrence must be positive"
    );
    let real_git = Command::new("sh").args(["-c", "command -v git"]).output()?;
    anyhow::ensure!(real_git.status.success(), "could not resolve the real Git executable");
    let real_git = String::from_utf8(real_git.stdout)?.trim().to_string();
    let wrapper_dir = workspace.join(format!(".git/{marker_name}-bin"));
    std::fs::create_dir_all(&wrapper_dir)?;
    let wrapper = wrapper_dir.join("git");
    std::fs::write(
        &wrapper,
        r#"#!/bin/sh
inject=0
for argument in "$@"; do
  if [ "$argument" = "$CARGO_RAIL_TEST_GIT_TRIGGER" ]; then
    inject=1
  fi
done
if [ "$inject" = 1 ]; then
  count=0
  if [ -f "$CARGO_RAIL_TEST_GIT_MARKER" ]; then
    read -r count < "$CARGO_RAIL_TEST_GIT_MARKER"
  fi
  count=$((count + 1))
  printf '%s\n' "$count" > "$CARGO_RAIL_TEST_GIT_MARKER"
  if [ "$count" -eq "$CARGO_RAIL_TEST_GIT_FAILURE_OCCURRENCE" ]; then
    "$CARGO_RAIL_TEST_REAL_GIT" "$@"
    result=$?
    if [ "$result" -ne 0 ]; then
      exit "$result"
    fi
    exit 86
  fi
fi
exec "$CARGO_RAIL_TEST_REAL_GIT" "$@"
"#,
    )?;
    let mut permissions = std::fs::metadata(&wrapper)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&wrapper, permissions)?;
    let path = std::env::join_paths(
        std::iter::once(wrapper_dir).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
    )?;
    let marker = workspace.join(format!(".git/{marker_name}"));
    anyhow::ensure!(!trigger.is_empty(), "Git wrapper trigger must not be empty");
    Ok((path, marker, real_git, trigger.to_string(), failure_occurrence))
}

#[cfg(unix)]
fn apply_git_failure_environment(
    command: &mut Command,
    path: &std::ffi::OsStr,
    marker: &Path,
    real_git: &str,
    trigger: &str,
    failure_occurrence: usize,
) {
    command
        .env("PATH", path)
        .env("CARGO_RAIL_TEST_REAL_GIT", real_git)
        .env("CARGO_RAIL_TEST_GIT_TRIGGER", trigger)
        .env("CARGO_RAIL_TEST_GIT_FAILURE_OCCURRENCE", failure_occurrence.to_string())
        .env("CARGO_RAIL_TEST_GIT_MARKER", marker);
}

#[cfg(unix)]
fn receipt_prepared_effect_ids(stdout: &[u8]) -> Result<Vec<String>> {
    let stdout = String::from_utf8_lossy(stdout);
    let receipt_path = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Receipt: "))
        .ok_or_else(|| anyhow::anyhow!("sync output has no apply receipt"))?;
    let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(receipt_path)?)?;
    Ok(receipt["plan"]["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|action| action["payload"]["prepared_effects"].as_array().into_iter().flatten())
        .filter_map(|effect| effect["effect_id"].as_str().map(str::to_string))
        .collect())
}

/// Helper to set up a split scenario
fn setup_split_scenario(crate_name: &str) -> Result<(TestWorkspace, TempDir)> {
    let ws = TestWorkspace::new()?;
    ws.add_crate(crate_name, "0.1.0", &[])?;
    ws.commit(&format!("Initial {}", crate_name))?;

    let split_dir = TempDir::new()?;
    git(split_dir.path(), &["init", "--initial-branch=main"])?;
    let config = format!(
        r#"[crates.{0}.split]
remote = "{1}"
branch = "main"
mode = "single"
"#,
        crate_name,
        split_dir.path().display().to_string().replace('\\', "\\\\")
    );
    std::fs::write(ws.path.join("rail.toml"), config)?;

    // Perform initial split
    let split = run_cargo_rail(
        &ws.path,
        &["rail", "split", "run", crate_name, "--yes", "--allow-dirty"],
    )?;
    anyhow::ensure!(
        split.status.success(),
        "initial split failed: {}",
        String::from_utf8_lossy(&split.stderr)
    );

    Ok((ws, split_dir))
}

/// Helper to set up a combined split scenario with two crates.
fn setup_combined_split_scenario(group_name: &str, crate_a: &str, crate_b: &str) -> Result<(TestWorkspace, TempDir)> {
    let ws = TestWorkspace::new()?;
    ws.add_crate(crate_a, "0.1.0", &[])?;
    ws.add_crate(crate_b, "0.1.0", &[])?;
    ws.commit(&format!("Initial {} group", group_name))?;

    let split_dir = TempDir::new()?;
    git(split_dir.path(), &["init", "--initial-branch=main"])?;
    let config = format!(
        r#"[crates.{0}.split]
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

#[cfg(unix)]
#[test]
fn test_sync_to_remote_saved_plan_recovers_after_ref_cas() {
    let result: Result<()> = (|| {
        let (ws, target) = setup_split_scenario("prepared-sync-to")?;
        ws.modify_file("prepared-sync-to", "src/lib.rs", "pub fn recovered_to_remote() {}\n")?;
        ws.commit("Prepare mono-to-remote recovery")?;

        let checked = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "prepared-sync-to",
                "--to-remote",
                "--check",
                "--allow-dirty",
                "--json",
            ],
        )?;
        assert_eq!(checked.status.code(), Some(1));
        let saved_plan = ws.path.join(".git/prepared-sync-to-plan.json");
        std::fs::write(&saved_plan, &checked.stdout)?;

        let (path, marker, real_git, trigger, failure_occurrence) =
            post_success_nth_git_failure_wrapper(&ws.path, "update-ref", "prepared-sync-to-cas", 1)?;
        let mut command = cargo_rail_command(&ws.path)?;
        apply_git_failure_environment(&mut command, &path, &marker, &real_git, &trigger, failure_occurrence);
        let interrupted = command
            .args([
                "rail",
                "sync",
                "prepared-sync-to",
                "--to-remote",
                "--yes",
                "--allow-dirty",
                "--plan",
                saved_plan.to_str().unwrap(),
            ])
            .output()?;
        assert!(!interrupted.status.success());
        assert!(marker.exists(), "the post-CAS interruption fixture did not run");
        assert_ne!(
            std::fs::read_to_string(target.path().join("src/lib.rs"))?,
            "pub fn recovered_to_remote() {}\n",
            "the interruption must precede target path convergence"
        );

        let fresh = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "prepared-sync-to",
                "--to-remote",
                "--check",
                "--allow-dirty",
                "--json",
            ],
        )?;
        assert_eq!(
            fresh.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&fresh.stdout),
            String::from_utf8_lossy(&fresh.stderr)
        );
        let fresh: serde_json::Value = serde_json::from_slice(&fresh.stdout)?;
        let effect_id = fresh["crates"][0]["prepared_effects"][0]["effect_id"]
            .as_str()
            .expect("fresh sync plan must project the prepared target effect")
            .to_string();

        let recovered = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "prepared-sync-to",
                "--to-remote",
                "--yes",
                "--allow-dirty",
                "--plan",
                saved_plan.to_str().unwrap(),
            ],
        )?;
        assert!(
            recovered.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&recovered.stdout),
            String::from_utf8_lossy(&recovered.stderr)
        );
        assert_eq!(receipt_prepared_effect_ids(&recovered.stdout)?, [effect_id]);
        assert_eq!(
            std::fs::read_to_string(target.path().join("src/lib.rs"))?,
            "pub fn recovered_to_remote() {}\n"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_sync_to_remote_saved_plan_recovers_ordered_commit_effect_chain() {
    let result: Result<()> = (|| {
        let (ws, target) = setup_split_scenario("prepared-sync-chain")?;
        ws.modify_file(
            "prepared-sync-chain",
            "src/lib.rs",
            "pub fn first_recovered_commit() {}\n",
        )?;
        ws.commit("Prepare first chained sync commit")?;
        ws.modify_file(
            "prepared-sync-chain",
            "src/lib.rs",
            "pub fn second_recovered_commit() {}\n",
        )?;
        ws.commit("Prepare second chained sync commit")?;

        let checked = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "prepared-sync-chain",
                "--to-remote",
                "--check",
                "--allow-dirty",
                "--json",
            ],
        )?;
        assert_eq!(checked.status.code(), Some(1));
        let saved_plan = ws.path.join(".git/prepared-sync-chain-plan.json");
        std::fs::write(&saved_plan, &checked.stdout)?;

        let (path, marker, real_git, trigger, failure_occurrence) =
            post_success_nth_git_failure_wrapper(&ws.path, "update-ref", "prepared-sync-chain-second-cas", 2)?;
        let mut command = cargo_rail_command(&ws.path)?;
        apply_git_failure_environment(&mut command, &path, &marker, &real_git, &trigger, failure_occurrence);
        let interrupted = command
            .args([
                "rail",
                "sync",
                "prepared-sync-chain",
                "--to-remote",
                "--yes",
                "--allow-dirty",
                "--plan",
                saved_plan.to_str().unwrap(),
            ])
            .output()?;
        assert!(!interrupted.status.success());
        assert_eq!(std::fs::read_to_string(&marker)?.trim(), "2");
        assert_eq!(
            std::fs::read_to_string(target.path().join("src/lib.rs"))?,
            "pub fn first_recovered_commit() {}\n",
            "the second ref CAS must fail before its path materialization"
        );

        let fresh = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "prepared-sync-chain",
                "--to-remote",
                "--check",
                "--allow-dirty",
                "--json",
            ],
        )?;
        assert_eq!(
            fresh.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&fresh.stdout),
            String::from_utf8_lossy(&fresh.stderr)
        );
        let fresh: serde_json::Value = serde_json::from_slice(&fresh.stdout)?;
        let effect_ids = fresh["crates"][0]["prepared_effects"]
            .as_array()
            .expect("fresh sync plan must project the prepared effect chain")
            .iter()
            .map(|effect| {
                effect["effect_id"]
                    .as_str()
                    .expect("prepared effect has an identity")
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(effect_ids.len(), 2);

        let recovered = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "prepared-sync-chain",
                "--to-remote",
                "--yes",
                "--allow-dirty",
                "--plan",
                saved_plan.to_str().unwrap(),
            ],
        )?;
        assert!(
            recovered.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&recovered.stdout),
            String::from_utf8_lossy(&recovered.stderr)
        );
        assert_eq!(receipt_prepared_effect_ids(&recovered.stdout)?, effect_ids);
        assert_eq!(
            std::fs::read_to_string(target.path().join("src/lib.rs"))?,
            "pub fn second_recovered_commit() {}\n"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_sync_to_remote_saved_plan_recovers_after_exact_publication() {
    let result: Result<()> = (|| {
        let (ws, target) = setup_split_scenario("prepared-sync-publication")?;
        let remote_parent = TempDir::new()?;
        let target_name = target
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("temporary target name must be UTF-8");
        let remote = remote_parent.path().join(format!("{target_name}.git"));
        git(
            remote_parent.path(),
            &["init", "--bare", "--initial-branch=main", remote.to_str().unwrap()],
        )?;
        let remote_url = format!("file://{}", remote.display());
        git(target.path(), &["remote", "add", "origin", &remote_url])?;
        git(target.path(), &["push", "-u", "origin", "main"])?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[crates.prepared-sync-publication.split]\nremote = \"{remote_url}\"\nbranch = \"main\"\nmode = \"single\"\n"
            ),
        )?;
        ws.commit("Configure exact sync publication")?;
        ws.modify_file(
            "prepared-sync-publication",
            "src/lib.rs",
            "pub fn exactly_published() {}\n",
        )?;
        ws.commit("Prepare sync publication recovery")?;

        let checked = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "prepared-sync-publication",
                "--to-remote",
                "--check",
                "--allow-dirty",
                "--json",
            ],
        )?;
        assert_eq!(checked.status.code(), Some(1));
        let saved_plan = ws.path.join(".git/prepared-sync-publication-plan.json");
        std::fs::write(&saved_plan, &checked.stdout)?;

        let (path, marker, real_git, trigger, failure_occurrence) =
            post_success_nth_git_failure_wrapper(&ws.path, "push", "prepared-sync-publication-push", 1)?;
        let mut command = cargo_rail_command(&ws.path)?;
        apply_git_failure_environment(&mut command, &path, &marker, &real_git, &trigger, failure_occurrence);
        let interrupted = command
            .args([
                "rail",
                "sync",
                "prepared-sync-publication",
                "--to-remote",
                "--yes",
                "--allow-dirty",
                "--plan",
                saved_plan.to_str().unwrap(),
            ])
            .output()?;
        assert!(!interrupted.status.success());
        assert_eq!(std::fs::read_to_string(&marker)?.trim(), "1");
        let local_head = String::from_utf8(git(target.path(), &["rev-parse", "refs/heads/main"])?.stdout)?;
        let remote_head = String::from_utf8(
            git(
                remote_parent.path(),
                &["--git-dir", remote.to_str().unwrap(), "rev-parse", "refs/heads/main"],
            )?
            .stdout,
        )?;
        assert_eq!(
            local_head, remote_head,
            "the exact sync publication must complete before interruption"
        );

        let fresh = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "prepared-sync-publication",
                "--to-remote",
                "--check",
                "--allow-dirty",
                "--json",
            ],
        )?;
        assert_eq!(
            fresh.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&fresh.stdout),
            String::from_utf8_lossy(&fresh.stderr)
        );
        let fresh: serde_json::Value = serde_json::from_slice(&fresh.stdout)?;
        let effect_ids = fresh["crates"][0]["prepared_effects"]
            .as_array()
            .expect("fresh sync plan must project commit and publication effects")
            .iter()
            .map(|effect| {
                effect["effect_id"]
                    .as_str()
                    .expect("prepared effect has an identity")
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(effect_ids.len(), 2);

        let recovered = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "prepared-sync-publication",
                "--to-remote",
                "--yes",
                "--allow-dirty",
                "--plan",
                saved_plan.to_str().unwrap(),
            ],
        )?;
        assert!(
            recovered.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&recovered.stdout),
            String::from_utf8_lossy(&recovered.stderr)
        );
        assert_eq!(receipt_prepared_effect_ids(&recovered.stdout)?, effect_ids);
        assert_eq!(
            std::fs::read_to_string(target.path().join("src/lib.rs"))?,
            "pub fn exactly_published() {}\n"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_sync_from_remote_saved_plan_recovers_after_ref_cas() {
    let result: Result<()> = (|| {
        let (ws, target) = setup_split_scenario("prepared-sync-from")?;
        std::fs::write(target.path().join("src/lib.rs"), "pub fn recovered_from_remote() {}\n")?;
        git(target.path(), &["add", "src/lib.rs"])?;
        git(target.path(), &["commit", "-m", "Prepare remote-to-mono recovery"])?;

        let checked = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "prepared-sync-from",
                "--from-remote",
                "--check",
                "--allow-dirty",
                "--json",
            ],
        )?;
        assert_eq!(checked.status.code(), Some(1));
        let saved_plan = ws.path.join(".git/prepared-sync-from-plan.json");
        std::fs::write(&saved_plan, &checked.stdout)?;

        let (path, marker, real_git, trigger, failure_occurrence) =
            post_success_nth_git_failure_wrapper(&ws.path, "update-ref", "prepared-sync-from-cas", 1)?;
        let mut command = cargo_rail_command(&ws.path)?;
        apply_git_failure_environment(&mut command, &path, &marker, &real_git, &trigger, failure_occurrence);
        let interrupted = command
            .args([
                "rail",
                "sync",
                "prepared-sync-from",
                "--from-remote",
                "--yes",
                "--allow-dirty",
                "--plan",
                saved_plan.to_str().unwrap(),
            ])
            .output()?;
        assert!(!interrupted.status.success());
        assert!(marker.exists(), "the source post-CAS interruption fixture did not run");

        let fresh = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "prepared-sync-from",
                "--from-remote",
                "--check",
                "--allow-dirty",
                "--json",
            ],
        )?;
        assert_eq!(
            fresh.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&fresh.stdout),
            String::from_utf8_lossy(&fresh.stderr)
        );
        let fresh: serde_json::Value = serde_json::from_slice(&fresh.stdout)?;
        let effect_id = fresh["crates"][0]["prepared_effects"][0]["effect_id"]
            .as_str()
            .expect("fresh sync plan must project the prepared source effect")
            .to_string();

        let recovered = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "prepared-sync-from",
                "--from-remote",
                "--yes",
                "--allow-dirty",
                "--plan",
                saved_plan.to_str().unwrap(),
            ],
        )?;
        assert!(
            recovered.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&recovered.stdout),
            String::from_utf8_lossy(&recovered.stderr)
        );
        assert_eq!(receipt_prepared_effect_ids(&recovered.stdout)?, [effect_id]);
        assert_eq!(
            std::fs::read_to_string(ws.path.join("crates/prepared-sync-from/src/lib.rs"))?,
            "pub fn recovered_from_remote() {}\n"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
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
            log.contains("Rail-Origin: v2 source="),
            "Should have Rail-Origin trailer"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_rejects_hidden_target_index_state_before_ref_mutation() {
    let result: Result<()> = (|| {
        for (case, flag) in [
            ("assume-unchanged", "--assume-unchanged"),
            ("skip-worktree", "--skip-worktree"),
        ] {
            let (ws, target) = setup_split_scenario(&format!("sync-hidden-index-{case}"))?;
            let crate_name = format!("sync-hidden-index-{case}");
            ws.modify_file(&crate_name, "src/lib.rs", "pub fn hidden_index_change() {}\n")?;
            ws.commit("Prepare sync across hidden target index state")?;
            let head = git(target.path(), &["rev-parse", "refs/heads/main"])?.stdout;
            git(target.path(), &["update-index", flag, "--", "src/lib.rs"])?;

            let rejected = run_cargo_rail(
                &ws.path,
                &["rail", "sync", &crate_name, "--to-remote", "--yes", "--allow-dirty"],
            )?;
            anyhow::ensure!(
                !rejected.status.success(),
                "sync accepted the {case} target index state"
            );
            anyhow::ensure!(
                String::from_utf8_lossy(&rejected.stderr).contains("unsupported state flag"),
                "sync did not classify the {case} target index state:\n{}",
                String::from_utf8_lossy(&rejected.stderr)
            );
            anyhow::ensure!(
                git(target.path(), &["rev-parse", "refs/heads/main"])?.stdout == head,
                "sync changed the target ref after rejecting {case}"
            );
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_both_directions_preserve_large_messages_without_argument_payloads() {
    let result: Result<()> = (|| {
        let (ws, split_dir) = setup_split_scenario("large-message-sync")?;
        let message_dir = TempDir::new()?;
        let message_path = message_dir.path().join("message.txt");

        ws.modify_file(
            "large-message-sync",
            "src/lib.rs",
            "pub fn large_message_from_mono() {}\n",
        )?;
        git(&ws.path, &["add", "crates/large-message-sync/src/lib.rs"])?;
        let mono_marker = "ordinary-mono-to-target-large-message-";
        std::fs::write(&message_path, format!("{mono_marker}{}\n", "m".repeat(192 * 1024)))?;
        git(&ws.path, &["commit", "-F", message_path.to_str().unwrap()])?;
        let to_remote = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "large-message-sync",
                "--to-remote",
                "--yes",
                "--allow-dirty",
            ],
        )?;
        assert!(
            to_remote.status.success(),
            "{}",
            String::from_utf8_lossy(&to_remote.stderr)
        );
        assert!(String::from_utf8(git(split_dir.path(), &["log", "-1", "--format=%B"])?.stdout)?.contains(mono_marker));

        std::fs::write(
            split_dir.path().join("src/lib.rs"),
            "pub fn large_message_from_target() {}\n",
        )?;
        git(split_dir.path(), &["add", "src/lib.rs"])?;
        let target_marker = "ordinary-target-to-mono-large-message-";
        std::fs::write(&message_path, format!("{target_marker}{}\n", "r".repeat(192 * 1024)))?;
        git(split_dir.path(), &["commit", "-F", message_path.to_str().unwrap()])?;
        let from_remote = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "large-message-sync",
                "--from-remote",
                "--yes",
                "--allow-dirty",
            ],
        )?;
        assert!(
            from_remote.status.success(),
            "{}",
            String::from_utf8_lossy(&from_remote.stderr)
        );
        assert!(String::from_utf8(git(&ws.path, &["log", "-1", "--format=%B"])?.stdout)?.contains(target_marker));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_v025_predecessor_trailer_check_is_read_only_and_apply_does_not_replay() {
    let result: Result<()> = (|| {
        let (ws, split_dir) = setup_split_scenario("legacy-sync")?;
        ws.modify_file("legacy-sync", "src/lib.rs", "// already synchronized by v0.25")?;
        let source_commit = ws.commit("Legacy synchronized change")?;
        std::fs::write(split_dir.path().join("src/lib.rs"), "// already synchronized by v0.25")?;
        git(split_dir.path(), &["add", "src/lib.rs"])?;
        git(
            split_dir.path(),
            &[
                "commit",
                "-m",
                &format!("Legacy target change\n\nRail-Origin: mono@{source_commit}"),
            ],
        )?;
        let legacy_target = String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();
        git(
            split_dir.path(),
            &["remote", "add", "origin", "https://example.invalid/legacy-sync.git"],
        )?;
        let mono_head = String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();

        let check = run_cargo_rail(&ws.path, &["rail", "sync", "legacy-sync", "--check", "--json"])?;
        assert_eq!(
            check.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&check.stderr)
        );
        let check_json: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        assert_eq!(check_json["crates"][0]["pending_commits"], 0);
        assert_eq!(check_json["crates"][0]["pending_origin_migrations"], 1);
        let migration_digest = check_json["crates"][0]["origin_migration_digest"]
            .as_str()
            .expect("check must expose the exact origin migration digest");
        assert!(migration_digest.starts_with("sha256-"));
        assert_eq!(
            check_json["mutation_plan"]["actions"][0]["payload"]["origin_migration_count"],
            1
        );
        assert_eq!(
            check_json["mutation_plan"]["actions"][0]["payload"]["origin_migration_digest"],
            migration_digest
        );
        let authority = &check_json["mutation_plan"]["actions"][0]["payload"]["origin_authority"];
        assert_eq!(authority["direction"], "bidirectional");
        assert_eq!(authority["owner"], "legacy-sync");
        assert_eq!(authority["transform_version"], 1);
        assert_eq!(authority["pending_candidate_count"], 1);
        assert_eq!(authority["migration_digest"], migration_digest);
        assert_eq!(
            String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?.trim(),
            legacy_target
        );
        assert_eq!(
            String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?.trim(),
            mono_head
        );

        let plan_path = ws.path.join("sync-origin-authority-plan.json");
        std::fs::write(&plan_path, serde_json::to_vec_pretty(&check_json["mutation_plan"])?)?;
        git(
            split_dir.path(),
            &[
                "remote",
                "set-url",
                "origin",
                "https://example.invalid/drifted-legacy-sync.git",
            ],
        )?;
        let identity_drift = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "legacy-sync",
                "--plan",
                plan_path.to_str().unwrap(),
                "--allow-dirty",
            ],
        )?;
        assert!(!identity_drift.status.success());
        assert_eq!(
            String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?.trim(),
            legacy_target
        );
        assert_eq!(
            String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?.trim(),
            mono_head
        );
        git(
            split_dir.path(),
            &["remote", "set-url", "origin", "https://example.invalid/legacy-sync.git"],
        )?;

        let apply = run_cargo_rail(&ws.path, &["rail", "sync", "legacy-sync", "--yes", "--allow-dirty"])?;
        assert!(apply.status.success(), "{}", String::from_utf8_lossy(&apply.stderr));
        let migrated_head = String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();
        assert_ne!(migrated_head, legacy_target);
        assert_eq!(
            String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?.trim(),
            mono_head,
            "the migration evidence commit must not replay into the monorepo"
        );
        let message = String::from_utf8(git(split_dir.path(), &["log", "-1", "--format=%B"])?.stdout)?;
        assert!(message.contains("chore: migrate cargo-rail origin mappings"));
        assert!(message.contains(&format!("commit={source_commit}")));
        assert!(message.contains(&format!("target={legacy_target}")));
        assert!(
            String::from_utf8(git(split_dir.path(), &["log", "--format=%s"])?.stdout)?
                .lines()
                .all(|subject| subject != "Sparse unmapped ancestor S1"),
            "an actual later pair mapping proves sparse ancestors are already embodied and must not replay"
        );
        let migration_commits = String::from_utf8(
            git(
                split_dir.path(),
                &[
                    "log",
                    "--format=%H",
                    "--grep=^chore: migrate cargo-rail origin mappings$",
                ],
            )?
            .stdout,
        )?;
        assert_eq!(migration_commits.lines().count(), 1);

        let second = run_cargo_rail(&ws.path, &["rail", "sync", "legacy-sync", "--yes", "--allow-dirty"])?;
        assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
        assert_eq!(
            String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?.trim(),
            migrated_head
        );
        assert_eq!(
            String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?.trim(),
            mono_head
        );
        let migration_commits = String::from_utf8(
            git(
                split_dir.path(),
                &[
                    "log",
                    "--format=%H",
                    "--grep=^chore: migrate cargo-rail origin mappings$",
                ],
            )?
            .stdout,
        )?;
        assert_eq!(migration_commits.lines().count(), 1);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_v025_migration_does_not_hide_independent_remote_commit() {
    let result: Result<()> = (|| {
        let (ws, split_dir) = setup_split_scenario("legacy-remote-gap")?;
        ws.modify_file("legacy-remote-gap", "src/lib.rs", "// synchronized by v0.25")?;
        let source_commit = ws.commit("Legacy synchronized source")?;
        std::fs::write(split_dir.path().join("src/lib.rs"), "// synchronized by v0.25")?;
        git(split_dir.path(), &["add", "src/lib.rs"])?;
        git(
            split_dir.path(),
            &[
                "commit",
                "-m",
                &format!("Legacy mapped target\n\nRail-Origin: mono@{source_commit}"),
            ],
        )?;
        let mapped_target = String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();

        std::fs::write(
            split_dir.path().join("README.md"),
            "# Independent remote commit after the mapped target\n",
        )?;
        git(split_dir.path(), &["add", "README.md"])?;
        git(split_dir.path(), &["commit", "-m", "Independent remote commit R"])?;
        let remote_commit = String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();
        assert_ne!(mapped_target, remote_commit);

        let check = run_cargo_rail(&ws.path, &["rail", "sync", "legacy-remote-gap", "--check", "--json"])?;
        assert_eq!(check.status.code(), Some(1));
        let check_json: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        assert_eq!(check_json["crates"][0]["pending_commits"], 1);
        assert_eq!(check_json["crates"][0]["pending_origin_migrations"], 1);
        assert_eq!(
            String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?.trim(),
            remote_commit,
            "check must not publish the migration commit"
        );

        let apply = run_cargo_rail(
            &ws.path,
            &["rail", "sync", "legacy-remote-gap", "--yes", "--allow-dirty"],
        )?;
        assert!(apply.status.success(), "{}", String::from_utf8_lossy(&apply.stderr));
        let migration_head = String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();
        assert_ne!(migration_head, remote_commit);
        let migration_subject = String::from_utf8(git(split_dir.path(), &["log", "-1", "--format=%s"])?.stdout)?;
        assert_eq!(migration_subject.trim(), "chore: migrate cargo-rail origin mappings");
        assert_eq!(
            std::fs::read_to_string(ws.path.join("crates/legacy-remote-gap/README.md"))?,
            "# Independent remote commit after the mapped target\n",
            "the independent R commit below migration M must be replayed into mono"
        );
        let mono_head = String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();
        let mono_message = String::from_utf8(git(&ws.path, &["log", "-1", "--format=%B"])?.stdout)?;
        assert!(mono_message.contains("Independent remote commit R"));
        assert!(mono_message.contains(&format!("commit={remote_commit}")));
        let mono_history = String::from_utf8(git(&ws.path, &["log", "--all", "--format=%B"])?.stdout)?;
        assert_eq!(mono_history.matches(&format!("commit={remote_commit}")).count(), 1);

        let clean = run_cargo_rail(&ws.path, &["rail", "sync", "legacy-remote-gap", "--check", "--json"])?;
        assert_eq!(clean.status.code(), Some(0));
        let clean_json: serde_json::Value = serde_json::from_slice(&clean.stdout)?;
        assert_eq!(clean_json["crates"][0]["pending_commits"], 0);
        assert_eq!(clean_json["crates"][0]["pending_origin_migrations"], 0);

        let second = run_cargo_rail(
            &ws.path,
            &["rail", "sync", "legacy-remote-gap", "--yes", "--allow-dirty"],
        )?;
        assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
        assert_eq!(
            String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?.trim(),
            migration_head
        );
        assert_eq!(
            String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?.trim(),
            mono_head
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_sync_check_requires_preloaded_remote_objects_without_fetching() {
    let result: Result<()> = (|| {
        let (ws, split_dir) = setup_split_scenario("fetched-mapping")?;
        let target_head = String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();
        let target_index = std::fs::read(split_dir.path().join(".git/index"))?;
        let target_worktree = std::fs::read(split_dir.path().join("src/lib.rs"))?;
        let original_message = String::from_utf8(git(split_dir.path(), &["log", "-1", "--format=%B"])?.stdout)?;
        let original_trailer = original_message
            .lines()
            .find(|line| line.starts_with("Rail-Origin: v2 "))
            .expect("initial split must write a current origin trailer");
        let original_commit_field = original_trailer
            .split_whitespace()
            .find(|field| field.starts_with("commit="))
            .expect("current origin trailer must bind its source commit");

        let upstream_parent = TempDir::new()?;
        let target_name = split_dir
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("temporary target name must be UTF-8");
        let upstream = upstream_parent.path().join(format!("{target_name}.git"));
        git(
            upstream_parent.path(),
            &["init", "--bare", "--initial-branch=main", upstream.to_str().unwrap()],
        )?;
        git(
            split_dir.path(),
            &["remote", "add", "origin", upstream.to_str().unwrap()],
        )?;
        git(split_dir.path(), &["push", "-u", "origin", "main"])?;

        let remote_url = format!("file://{}", upstream.display());
        git(split_dir.path(), &["remote", "set-url", "origin", &remote_url])?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[crates.fetched-mapping.split]\nremote = \"{remote_url}\"\nbranch = \"main\"\nmode = \"single\"\n"
            ),
        )?;
        ws.commit("Use non-local fetch fixture")?;
        ws.modify_file(
            "fetched-mapping",
            "src/lib.rs",
            "// already represented by fetched target T",
        )?;
        let mapped_source = ws.commit("Source S already represented remotely")?;
        let mono_head = String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();

        let publisher = TempDir::new()?;
        git(publisher.path(), &["clone", upstream.to_str().unwrap(), "."])?;
        git(publisher.path(), &["config", "user.name", "Test User"])?;
        git(publisher.path(), &["config", "user.email", "test@example.com"])?;
        std::fs::write(
            publisher.path().join("src/lib.rs"),
            "// already represented by fetched target T",
        )?;
        git(publisher.path(), &["add", "src/lib.rs"])?;
        let mapped_trailer = original_trailer.replace(original_commit_field, &format!("commit={mapped_source}"));
        git(
            publisher.path(),
            &["commit", "-m", &format!("Fetched mapped target T\n\n{mapped_trailer}")],
        )?;
        let fetched_target = String::from_utf8(git(publisher.path(), &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();
        git(publisher.path(), &["push", "origin", "main"])?;
        assert_ne!(fetched_target, target_head);
        assert_eq!(
            String::from_utf8(git(split_dir.path(), &["rev-parse", "origin/main"])?.stdout)?.trim(),
            target_head,
            "fixture requires the local observation to be stale before sync"
        );

        let refs_before = git(split_dir.path(), &["for-each-ref", "--format=%(refname)=%(objectname)"])?.stdout;
        let objects_before = snapshot_directory(&split_dir.path().join(".git/objects"))?;
        let fetch_head_before = std::fs::read(split_dir.path().join(".git/FETCH_HEAD")).ok();
        assert_eq!(
            std::fs::read(split_dir.path().join(".git/index"))?,
            target_index,
            "fixture setup changed the target index before cargo-rail ran"
        );
        let check = run_cargo_rail(
            &ws.path,
            &["rail", "sync", "fetched-mapping", "--from-remote", "--check", "--json"],
        )?;
        assert!(!check.status.success());
        let diagnostic = format!(
            "{}{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
        assert!(
            diagnostic.contains("absent from the local target object view"),
            "{diagnostic}"
        );
        assert_eq!(
            String::from_utf8(git(split_dir.path(), &["rev-parse", "origin/main"])?.stdout)?.trim(),
            target_head,
            "read-only check must not update a stale tracking ref"
        );
        assert_eq!(
            git(split_dir.path(), &["for-each-ref", "--format=%(refname)=%(objectname)"])?.stdout,
            refs_before
        );
        assert_eq!(
            snapshot_directory(&split_dir.path().join(".git/objects"))?,
            objects_before
        );
        assert_eq!(
            std::fs::read(split_dir.path().join(".git/FETCH_HEAD")).ok(),
            fetch_head_before
        );
        assert_eq!(
            String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?.trim(),
            mono_head
        );
        assert_eq!(
            String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?.trim(),
            target_head
        );
        assert_eq!(std::fs::read(split_dir.path().join(".git/index"))?, target_index);
        assert_eq!(std::fs::read(split_dir.path().join("src/lib.rs"))?, target_worktree);
        let mono_messages = String::from_utf8(git(&ws.path, &["log", "--format=%B"])?.stdout)?;
        assert!(!mono_messages.contains(&format!("commit={fetched_target}")));

        git(
            split_dir.path(),
            &["fetch", "--no-tags", &remote_url, "refs/heads/main"],
        )?;
        let hydrated = run_cargo_rail(
            &ws.path,
            &["rail", "sync", "fetched-mapping", "--from-remote", "--check", "--json"],
        )?;
        assert!(
            hydrated.status.success(),
            "{}",
            String::from_utf8_lossy(&hydrated.stderr)
        );
        let hydrated_json: serde_json::Value = serde_json::from_slice(&hydrated.stdout)?;
        assert_eq!(hydrated_json["crates"][0]["pending_commits"], 0);
        assert_eq!(
            String::from_utf8(git(split_dir.path(), &["rev-parse", "origin/main"])?.stdout)?.trim(),
            target_head,
            "exact-object checks must not require or rewrite the tracking ref"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_mapping_frontier_excludes_only_mapped_ancestors_across_merges() {
    let result: Result<()> = (|| {
        let (ws, split_dir) = setup_split_scenario("legacy-merge-frontier")?;
        git(&ws.path, &["add", "rail.toml"])?;
        git(&ws.path, &["commit", "-m", "Add sync fixture configuration"])?;

        git(&ws.path, &["checkout", "-b", "unmapped-side"])?;
        std::fs::write(
            ws.path.join("crates/legacy-merge-frontier/src/side.rs"),
            "pub fn side_branch_value() -> u8 { 7 }\n",
        )?;
        git(&ws.path, &["add", "crates/legacy-merge-frontier/src/side.rs"])?;
        git(&ws.path, &["commit", "-m", "Unmapped side-branch U"])?;
        git(&ws.path, &["checkout", "main"])?;

        ws.modify_file("legacy-merge-frontier", "src/lib.rs", "// later mapped source S2")?;
        let mapped_source = ws.commit("Later mapped source S2")?;
        git(
            &ws.path,
            &["merge", "--no-ff", "unmapped-side", "-m", "Merge unmapped side branch"],
        )?;

        std::fs::write(split_dir.path().join("src/lib.rs"), "// later mapped source S2")?;
        git(split_dir.path(), &["add", "src/lib.rs"])?;
        git(
            split_dir.path(),
            &[
                "commit",
                "-m",
                &format!("Legacy mapped target T2\n\nRail-Origin: mono@{mapped_source}"),
            ],
        )?;

        let check = run_cargo_rail(
            &ws.path,
            &["rail", "sync", "legacy-merge-frontier", "--check", "--json"],
        )?;
        assert_eq!(check.status.code(), Some(1));
        let check_json: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        assert!(check_json["crates"][0]["pending_commits"].as_u64().unwrap() >= 1);
        assert_eq!(check_json["crates"][0]["pending_origin_migrations"], 1);

        let apply = run_cargo_rail(
            &ws.path,
            &["rail", "sync", "legacy-merge-frontier", "--yes", "--allow-dirty"],
        )?;
        assert!(apply.status.success(), "{}", String::from_utf8_lossy(&apply.stderr));
        assert_eq!(
            std::fs::read_to_string(split_dir.path().join("src/side.rs"))?,
            "pub fn side_branch_value() -> u8 { 7 }\n",
            "a side-branch commit not ancestral to mapped S2 must remain pending"
        );
        let target_subjects = String::from_utf8(git(split_dir.path(), &["log", "--format=%s"])?.stdout)?;
        assert!(
            target_subjects
                .lines()
                .any(|subject| subject == "Unmapped side-branch U")
        );
        assert!(
            target_subjects
                .lines()
                .all(|subject| subject != "Sparse linear ancestor S1"),
            "the mapped S2 frontier must suppress only its actual ancestors"
        );

        let clean = run_cargo_rail(
            &ws.path,
            &["rail", "sync", "legacy-merge-frontier", "--check", "--json"],
        )?;
        assert_eq!(clean.status.code(), Some(0));
        let clean_json: serde_json::Value = serde_json::from_slice(&clean.stdout)?;
        assert_eq!(clean_json["crates"][0]["pending_commits"], 0);
        assert_eq!(clean_json["crates"][0]["pending_origin_migrations"], 0);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_sync_rejects_selected_target_aliases_and_shared_remote_override() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::symlink;

        let ws = TestWorkspace::new_named("sync-duplicate-targets")?;
        ws.add_crate("sync-alias-a", "0.1.0", &[])?;
        ws.add_crate("sync-alias-b", "0.1.0", &[])?;
        ws.commit("Add sync duplicate-target fixtures")?;
        let targets = TempDir::new()?;
        let canonical = targets.path().join("canonical");
        let alias = targets.path().join("alias");
        std::fs::create_dir_all(&canonical)?;
        symlink(&canonical, &alias)?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[crates.sync-alias-a.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n\n[crates.sync-alias-b.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
                canonical.display().to_string().replace('\\', "\\\\"),
                alias.display().to_string().replace('\\', "\\\\"),
            ),
        )?;

        let aliased = run_cargo_rail(&ws.path, &["rail", "sync", "--all", "--to-remote", "--check"])?;
        assert!(!aliased.status.success());
        let diagnostic = String::from_utf8_lossy(&aliased.stderr);
        assert!(diagnostic.contains("sync-alias-a") && diagnostic.contains("sync-alias-b"));
        assert!(diagnostic.contains("overlapping target repositories"));
        assert!(!canonical.join(".git").exists());

        let distinct = targets.path().join("distinct");
        let shared = targets.path().join("shared-override");
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[crates.sync-alias-a.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n\n[crates.sync-alias-b.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
                canonical.display().to_string().replace('\\', "\\\\"),
                distinct.display().to_string().replace('\\', "\\\\"),
            ),
        )?;
        let shared_override = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "--all",
                "--remote",
                shared.to_str().unwrap(),
                "--to-remote",
                "--check",
            ],
        )?;
        assert!(!shared_override.status.success());
        let diagnostic = String::from_utf8_lossy(&shared_override.stderr);
        assert!(diagnostic.contains("sync-alias-a") && diagnostic.contains("sync-alias-b"));
        assert!(diagnostic.contains("--remote applies to every --all selection"));
        assert!(!shared.join(".git").exists());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_all_rejects_multiple_monorepo_mutators_before_observation() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("sync-all-source-head")?;
        ws.add_crate("from-a", "0.1.0", &[])?;
        ws.add_crate("from-b", "0.1.0", &[])?;
        let target_a = TempDir::new()?;
        let target_b = TempDir::new()?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[crates.from-a.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n\n[crates.from-b.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
                target_a.path().display().to_string().replace('\\', "\\\\"),
                target_b.path().display().to_string().replace('\\', "\\\\"),
            ),
        )?;
        let source_head = ws.commit("Add multi-source-head fixture")?;

        let output = run_cargo_rail(&ws.path, &["rail", "sync", "--all", "--from-remote", "--check"])?;
        assert!(!output.status.success());
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains("cannot combine multiple operations that mutate the monorepo"));
        assert!(diagnostic.contains("exact source HEAD"));
        assert_eq!(
            String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?.trim(),
            source_head
        );
        assert!(!target_a.path().join(".git").exists());
        assert!(!target_b.path().join(".git").exists());
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
        git(split_dir.path(), &["init", "--initial-branch=main"])?;
        let config = format!(
            r#"[crates.asset-sync.split]
remote = "{}"
branch = "main"
mode = "single"
include = ["NOTICE", "assets/**"]
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

        std::fs::create_dir_all(ws.path.join("assets"))?;
        std::fs::write(ws.path.join("assets/new.txt"), "added\n")?;
        ws.commit("Add asset selected by stable policy")?;
        run_cargo_rail(
            &ws.path,
            &["rail", "sync", "asset-sync", "--to-remote", "--yes", "--allow-dirty"],
        )?;
        assert_eq!(
            git(split_dir.path(), &["show", "HEAD:assets/new.txt"])?.stdout,
            b"added\n"
        );

        std::fs::rename(ws.path.join("assets/new.txt"), ws.path.join("assets/renamed.txt"))?;
        ws.commit("Rename asset selected by stable policy")?;
        run_cargo_rail(
            &ws.path,
            &["rail", "sync", "asset-sync", "--to-remote", "--yes", "--allow-dirty"],
        )?;
        assert!(!split_dir.path().join("assets/new.txt").exists());
        assert_eq!(
            git(split_dir.path(), &["show", "HEAD:assets/renamed.txt"])?.stdout,
            b"added\n"
        );

        std::fs::remove_file(ws.path.join("assets/renamed.txt"))?;
        ws.commit("Delete asset selected by stable policy")?;
        run_cargo_rail(
            &ws.path,
            &["rail", "sync", "asset-sync", "--to-remote", "--yes", "--allow-dirty"],
        )?;
        assert!(!split_dir.path().join("assets/renamed.txt").exists());

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

        std::fs::create_dir_all(split_dir.path().join("assets"))?;
        std::fs::write(split_dir.path().join("assets/from-remote.txt"), "remote asset\n")?;
        git(split_dir.path(), &["add", "assets/from-remote.txt"])?;
        git(split_dir.path(), &["commit", "-m", "Add policy asset in split"])?;
        run_cargo_rail(
            &ws.path,
            &["rail", "sync", "asset-sync", "--from-remote", "--yes", "--allow-dirty"],
        )?;
        assert_eq!(
            std::fs::read(ws.path.join("assets/from-remote.txt"))?,
            b"remote asset\n"
        );
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
            log.contains("Rail-Origin: v2 source="),
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
            "manual conflict must use exit code 1; stderr:\n{}",
            String::from_utf8_lossy(&conflicted.stderr)
        );
        let stdout = String::from_utf8_lossy(&conflicted.stdout);
        assert!(stdout.contains("Sync conflicted"), "stdout:\n{stdout}");

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
        let receipt = std::fs::read_dir(&receipts)
            .map_err(|error| anyhow::anyhow!("reading receipt directory '{}': {error}", receipts.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("sync-conflict-manual-conflict-lib-"))
            })
            .ok_or_else(|| anyhow::anyhow!("missing sync conflict receipt"))?;
        let receipt_json: serde_json::Value = serde_json::from_slice(&std::fs::read(&receipt)?)?;
        assert_eq!(receipt_json["status"], "conflicted");
        assert_eq!(receipt_json["conflicts"][0]["class"], "content");

        let mut interrupted = receipt_json;
        interrupted["status"] = serde_json::Value::String("materializing".to_string());
        std::fs::write(&receipt, serde_json::to_vec_pretty(&interrupted)?)?;
        std::fs::write(&conflicted_path, "pub fn value() -> &'static str {\n  \"mono\"\n}\n")?;
        let recovered = run_cargo_rail(&ws.path, &["rail", "sync", "--resume", receipt.to_str().unwrap()])?;
        assert_eq!(
            recovered.status.code(),
            Some(1),
            "materialization recovery must return to operator conflict resolution; stderr:\n{}",
            String::from_utf8_lossy(&recovered.stderr)
        );
        assert!(std::fs::read_to_string(&conflicted_path)?.contains("<<<<<<<"));
        let recovered_receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(&receipt)?)?;
        assert_eq!(recovered_receipt["status"], "conflicted");

        std::fs::write(
            &conflicted_path,
            "pub fn value() -> &'static str {\n  \"resolved\"\n}\n",
        )?;
        let target_head = String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();
        git(
            &ws.path,
            &[
                "notes",
                "--ref",
                "refs/notes/rail/manual-conflict-lib",
                "add",
                "-m",
                &target_head,
                &recovery_parent,
            ],
        )?;
        let blocked = run_cargo_rail(&ws.path, &["rail", "sync", "--resume", receipt.to_str().unwrap()])?;
        assert!(!blocked.status.success());
        assert!(String::from_utf8_lossy(&blocked.stderr).contains("unbound predecessor origin migration"));
        assert_eq!(
            String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?.trim(),
            target_head
        );
        assert_eq!(
            String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?.trim(),
            recovery_parent
        );
        git(
            &ws.path,
            &[
                "notes",
                "--ref",
                "refs/notes/rail/manual-conflict-lib",
                "remove",
                &recovery_parent,
            ],
        )?;
        git(
            split_dir.path(),
            &["commit", "--allow-empty", "-m", "Concurrent target authority drift"],
        )?;
        let drifted_target_head = String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();
        let authority_blocked = run_cargo_rail(&ws.path, &["rail", "sync", "--resume", receipt.to_str().unwrap()])?;
        assert!(!authority_blocked.status.success());
        assert!(
            String::from_utf8_lossy(&authority_blocked.stderr)
                .contains("mapping authority changed after the conflict receipt")
        );
        assert_eq!(
            String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?.trim(),
            drifted_target_head
        );
        assert_eq!(
            String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?.trim(),
            recovery_parent
        );
        git(split_dir.path(), &["reset", "--hard", &target_head])?;
        let resumed = run_cargo_rail(&ws.path, &["rail", "sync", "--resume", receipt.to_str().unwrap()])?;
        assert!(
            resumed.status.success(),
            "resume failed:\n{}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        let log = git(&ws.path, &["log", "-1", "--format=%B"])?;
        assert!(String::from_utf8_lossy(&log.stdout).contains("Rail-Origin: v2 source="));
        let receipt_json: serde_json::Value = serde_json::from_slice(&std::fs::read(&receipt)?)?;
        assert_eq!(receipt_json["status"], "resolved");
        assert!(
            !std::fs::read_to_string(&conflicted_path)
                .map_err(|error| anyhow::anyhow!("reading resolved path '{}': {error}", conflicted_path.display()))?
                .contains("<<<<<<<")
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_v025_conflict_receipt_with_existing_v1_history_migrates_before_resume() {
    let result: Result<()> = (|| {
        let (ws, split_dir) = setup_split_scenario("demo")?;
        let mono_repository = cargo_rail::git::mappings::repository_identity(&ws.path)?;
        let target_repository = cargo_rail::git::mappings::repository_identity(split_dir.path())?;

        std::fs::write(ws.path.join("UNOWNED.txt"), "does not change the split projection\n")?;
        let predecessor_source = ws.commit("Unowned predecessor source marker")?;
        let predecessor_trailer = format!(
            "Rail-Origin: v1 source={mono_repository} commit={predecessor_source} owner=64656d6f snapshot=v1-sha256-volatile-content transform=1"
        );
        git(
            split_dir.path(),
            &[
                "commit",
                "--allow-empty",
                "-m",
                &format!("Predecessor exact mapping\n\n{predecessor_trailer}"),
            ],
        )?;

        ws.modify_file("demo", "src/lib.rs", "pub fn value() -> &'static str { \"mono\" }\n")?;
        let expected_head = ws.commit("Conflicting monorepo edit")?;
        std::fs::write(
            split_dir.path().join("src/lib.rs"),
            "pub fn value() -> &'static str { \"split\" }\n",
        )?;
        git(split_dir.path(), &["add", "src/lib.rs"])?;
        git(split_dir.path(), &["commit", "-m", "Conflicting split edit"])?;
        let remote_commit = String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();

        let branch = "cargo-rail-sync-demo";
        git(&ws.path, &["checkout", "-b", branch])?;
        std::fs::write(
            ws.path.join("crates/demo/src/lib.rs"),
            "pub fn value() -> &'static str { \"resolved\" }\n",
        )?;

        let field = |format: &str| -> Result<String> {
            Ok(
                String::from_utf8(git(split_dir.path(), &["show", "-s", format, &remote_commit])?.stdout)?
                    .trim()
                    .to_string(),
            )
        };
        let mut receipt: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/compat/v0.25.0/sync/conflict-receipt-v2.json"))?;
        receipt["expected_head"] = expected_head.into();
        receipt["remote_commit"] = remote_commit.clone().into();
        receipt["branch"] = branch.into();
        receipt["message"] = format!(
            "Conflicting split edit\n\nRail-Origin: v1 source={target_repository} commit={remote_commit} owner=64656d6f snapshot=v1-sha256-volatile-content transform=1"
        )
        .into();
        receipt["author"] = field("--format=%an")?.into();
        receipt["author_email"] = field("--format=%ae")?.into();
        receipt["author_timestamp"] = field("--format=%at")?.parse::<i64>()?.into();
        receipt["author_timezone"] = field("--format=%ai")?
            .split_whitespace()
            .last()
            .unwrap_or("+0000")
            .into();
        receipt["committer"] = field("--format=%cn")?.into();
        receipt["committer_email"] = field("--format=%ce")?.into();
        receipt["committer_timestamp"] = field("--format=%ct")?.parse::<i64>()?.into();
        receipt["committer_timezone"] = field("--format=%ci")?
            .split_whitespace()
            .last()
            .unwrap_or("+0000")
            .into();

        let receipts = ws.path.join("target/cargo-rail/receipts");
        std::fs::create_dir_all(&receipts)?;
        let receipt_path = receipts.join("sync-conflict-demo-v025.json");
        std::fs::write(&receipt_path, serde_json::to_vec_pretty(&receipt)?)?;

        let resumed = run_cargo_rail(&ws.path, &["rail", "sync", "--resume", receipt_path.to_str().unwrap()])?;
        assert!(resumed.status.success(), "{}", String::from_utf8_lossy(&resumed.stderr));
        let upgraded: serde_json::Value = serde_json::from_slice(&std::fs::read(&receipt_path)?)?;
        assert_eq!(upgraded["schema_version"], 3);
        assert_eq!(upgraded["status"], "resolved");
        let target_log = String::from_utf8(git(split_dir.path(), &["log", "--format=%s%n%B"])?.stdout)?;
        assert!(target_log.contains("chore: migrate cargo-rail origin mappings"));
        assert!(target_log.contains("Rail-Origin: v2 source="));
        let mono_log = String::from_utf8(git(&ws.path, &["log", "-1", "--format=%B"])?.stdout)?;
        assert!(mono_log.contains("Rail-Origin: v2 source="));
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
            log.contains("Rail-Origin: v2 source="),
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

        // Normal output carries the material result; engine chatter is verbose-only.
        let stdout2 = String::from_utf8_lossy(&output2.stdout);
        assert!(
            stdout2.contains("Sync complete: 0 commits"),
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
        assert!(
            std::fs::read_to_string(split_dir.path().join("src/lib.rs"))?.contains("// Mono change"),
            "the frozen mono commit set must reach the remote after the remote phase"
        );
        assert_eq!(
            std::fs::read_to_string(ws.path.join("crates/bidir-idempotent-lib/README.md"))?,
            "# Updated from split repo",
            "the remote commit must reach mono before later synthesized mappings can hide it"
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

#[test]
fn test_sync_remote_phase_retry_preserves_pending_mono_ancestor() {
    let result: Result<()> = (|| {
        let (ws, split_dir) = setup_split_scenario("directional-frontier-retry")?;

        ws.modify_file(
            "directional-frontier-retry",
            "src/lib.rs",
            "pub fn mono_change() -> bool { true }\n",
        )?;
        ws.commit("Pending mono H")?;

        std::fs::write(split_dir.path().join("README.md"), "# Independent remote R\n")?;
        git(split_dir.path(), &["add", "README.md"])?;
        git(split_dir.path(), &["commit", "-m", "Independent remote R"])?;

        // This is the durable state left by a bidirectional operation that
        // completed its remote-to-mono phase and stopped before mono-to-remote.
        // The synthesized mono commit D is a child of pending H, but D -> R
        // proves only the target ancestry frontier.
        let from_remote = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "directional-frontier-retry",
                "--from-remote",
                "--yes",
                "--allow-dirty",
            ],
        )?;
        assert!(
            from_remote.status.success(),
            "remote phase should succeed. stderr: {}",
            String::from_utf8_lossy(&from_remote.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(ws.path.join("crates/directional-frontier-retry/README.md"))?,
            "# Independent remote R\n"
        );

        let split_before_retry = String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?;
        let to_remote = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "directional-frontier-retry",
                "--to-remote",
                "--yes",
                "--allow-dirty",
            ],
        )?;
        assert!(
            to_remote.status.success(),
            "fresh retry must publish pending H. stderr: {}",
            String::from_utf8_lossy(&to_remote.stderr)
        );
        assert!(
            std::fs::read_to_string(split_dir.path().join("src/lib.rs"))?.contains("mono_change"),
            "the source-side endpoint D must not exclude its pending ancestor H"
        );
        assert_ne!(
            String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?.trim(),
            split_before_retry.trim()
        );

        let split_subjects = String::from_utf8(git(split_dir.path(), &["log", "--format=%s"])?.stdout)?;
        assert_eq!(
            split_subjects.lines().filter(|line| *line == "Pending mono H").count(),
            1
        );
        assert_eq!(
            split_subjects
                .lines()
                .filter(|line| *line == "Independent remote R")
                .count(),
            1,
            "the exact synthesized D endpoint must not replay R back to its own repository"
        );
        let mono_subjects = String::from_utf8(git(&ws.path, &["log", "--format=%s", "--all"])?.stdout)?;
        assert_eq!(
            mono_subjects
                .lines()
                .filter(|line| *line == "Independent remote R")
                .count(),
            1
        );

        let before_noop = String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?;
        let noop = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "directional-frontier-retry",
                "--to-remote",
                "--yes",
                "--allow-dirty",
            ],
        )?;
        assert!(noop.status.success());
        assert_eq!(
            String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?.trim(),
            before_noop.trim(),
            "a second retry must be idempotent"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_bidirectional_same_path_divergence_conflicts_before_mono_to_remote_mutation() {
    let result: Result<()> = (|| {
        let (ws, split_dir) = setup_split_scenario("bidir-frontier-conflict")?;

        ws.modify_file(
            "bidir-frontier-conflict",
            "src/lib.rs",
            "pub fn value() -> &'static str { \"mono\" }\n",
        )?;
        let mono_commit = ws.commit("Divergent mono S")?;
        std::fs::write(
            split_dir.path().join("src/lib.rs"),
            "pub fn value() -> &'static str { \"remote\" }\n",
        )?;
        git(split_dir.path(), &["add", "src/lib.rs"])?;
        git(split_dir.path(), &["commit", "-m", "Divergent remote R"])?;
        let remote_commit = String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "sync", "bidir-frontier-conflict", "--yes", "--allow-dirty"],
        )?;
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stdout).contains("Sync conflicted"));
        assert_eq!(
            String::from_utf8(git(split_dir.path(), &["rev-parse", "HEAD"])?.stdout)?.trim(),
            remote_commit,
            "bidirectional sync must resolve remote R against the original frontier before publishing mono S"
        );
        assert_eq!(
            String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?.trim(),
            mono_commit,
            "unresolved divergence must not create a mono commit"
        );
        assert!(
            std::fs::read_to_string(ws.path.join("crates/bidir-frontier-conflict/src/lib.rs"))?.contains("<<<<<<<"),
            "same-path divergence must preserve manual conflict markers"
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
