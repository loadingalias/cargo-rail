//! Integration tests for split operations

#[cfg(unix)]
use crate::helpers::cargo_rail_command;
use crate::helpers::{TestWorkspace, file_url, git, run_cargo_rail};
use anyhow::Result;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
use tempfile::TempDir;

fn initialized_split_target(branch: &str) -> Result<TempDir> {
    let target = TempDir::new()?;
    git(target.path(), &["init", "--initial-branch", branch])?;
    Ok(target)
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum SplitRecoveryBoundary {
    PackPersisted,
    RefCas,
    PartialMaterialization,
    JournalCompleted,
    Published,
}

#[cfg(unix)]
impl SplitRecoveryBoundary {
    fn fault_name(self) -> &'static str {
        match self {
            Self::PackPersisted => "pack",
            Self::RefCas | Self::PartialMaterialization => "ref-cas",
            Self::JournalCompleted => "journal-completed",
            Self::Published => "published",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::PackPersisted => "pack-persisted",
            Self::RefCas => "ref-cas",
            Self::PartialMaterialization => "partial-materialization",
            Self::JournalCompleted => "journal-completed",
            Self::Published => "published",
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum SplitRetryMode {
    Fresh,
    SavedPlan,
}

#[cfg(unix)]
impl SplitRetryMode {
    fn label(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::SavedPlan => "saved-plan",
        }
    }
}

#[cfg(unix)]
fn regular_files(directory: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut files = std::fs::read_dir(directory)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|actual| actual == extension))
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

#[cfg(unix)]
fn split_recovery_fault_wrapper(
    workspace: &Path,
    effect_root: &Path,
    boundary: SplitRecoveryBoundary,
) -> Result<(std::ffi::OsString, PathBuf, String)> {
    use std::os::unix::fs::PermissionsExt as _;

    let real_git = Command::new("sh").args(["-c", "command -v git"]).output()?;
    anyhow::ensure!(real_git.status.success(), "could not resolve the real Git executable");
    let real_git = String::from_utf8(real_git.stdout)?.trim().to_string();
    let wrapper_dir = workspace.join(format!(".git/split-recovery-{}-bin", boundary.label()));
    std::fs::create_dir_all(&wrapper_dir)?;
    let wrapper = wrapper_dir.join("git");
    std::fs::write(
        &wrapper,
        r#"#!/bin/sh
fault=0
case "$CARGO_RAIL_TEST_SPLIT_BOUNDARY" in
  pack)
    pack=0
    active=0
    completed=0
    for entry in "$CARGO_RAIL_TEST_SPLIT_EFFECT_ROOT"/objects/*.pack; do
      if [ -f "$entry" ]; then pack=1; fi
    done
    for entry in "$CARGO_RAIL_TEST_SPLIT_EFFECT_ROOT"/active/*.json; do
      if [ -f "$entry" ]; then active=1; fi
    done
    for entry in "$CARGO_RAIL_TEST_SPLIT_EFFECT_ROOT"/completed/*.json; do
      if [ -f "$entry" ]; then completed=1; fi
    done
    if [ "$pack" = 1 ] && [ "$active" = 0 ] && [ "$completed" = 0 ]; then fault=1; fi
    ;;
  ref-cas)
    for argument in "$@"; do
      if [ "$argument" = "update-ref" ]; then fault=1; fi
    done
    ;;
  journal-completed)
    completed=0
    active=0
    for entry in "$CARGO_RAIL_TEST_SPLIT_EFFECT_ROOT"/completed/*.json; do
      if [ -f "$entry" ]; then completed=1; fi
    done
    for entry in "$CARGO_RAIL_TEST_SPLIT_EFFECT_ROOT"/active/*.json; do
      if [ -f "$entry" ]; then active=1; fi
    done
    if [ "$completed" = 1 ] && [ "$active" = 0 ]; then fault=1; fi
    ;;
  published)
    for argument in "$@"; do
      if [ "$argument" = "push" ]; then fault=1; fi
    done
    ;;
esac
if [ "$fault" = 1 ] && [ ! -f "$CARGO_RAIL_TEST_SPLIT_MARKER" ]; then
  case "$CARGO_RAIL_TEST_SPLIT_BOUNDARY" in
    ref-cas|published)
      "$CARGO_RAIL_TEST_REAL_GIT" "$@"
      result=$?
      if [ "$result" -ne 0 ]; then exit "$result"; fi
      ;;
  esac
  : > "$CARGO_RAIL_TEST_SPLIT_MARKER"
  exit 86
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
    let marker = workspace.join(format!(".git/split-recovery-{}-injected", boundary.label()));
    anyhow::ensure!(
        !effect_root.as_os_str().is_empty(),
        "prepared effect root must be named"
    );
    Ok((path, marker, real_git))
}

#[cfg(unix)]
fn apply_split_recovery_fault_environment(
    command: &mut Command,
    path: &std::ffi::OsStr,
    marker: &Path,
    real_git: &str,
    effect_root: &Path,
    boundary: SplitRecoveryBoundary,
) {
    command
        .env("PATH", path)
        .env("CARGO_RAIL_TEST_REAL_GIT", real_git)
        .env("CARGO_RAIL_TEST_SPLIT_MARKER", marker)
        .env("CARGO_RAIL_TEST_SPLIT_EFFECT_ROOT", effect_root)
        .env("CARGO_RAIL_TEST_SPLIT_BOUNDARY", boundary.fault_name());
}

#[cfg(unix)]
fn split_receipt_effect_ids(stdout: &[u8]) -> Result<Vec<String>> {
    let stdout = String::from_utf8_lossy(stdout);
    let receipt_path = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Receipt: "))
        .ok_or_else(|| anyhow::anyhow!("split output has no apply receipt"))?;
    let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(receipt_path)?)?;
    receipt["trace"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["code"] == "SPLIT_GIT_EFFECT_COMPLETED")
        .map(|entry| {
            let audit: serde_json::Value = serde_json::from_str(
                entry["message"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("split Git-effect trace has no audit payload"))?,
            )?;
            audit["effect_id"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("split Git-effect audit has no effect identity"))
        })
        .collect()
}

#[cfg(unix)]
fn exercise_split_recovery_boundary(boundary: SplitRecoveryBoundary, retry_mode: SplitRetryMode) -> Result<()> {
    let case = format!("{}-{}", boundary.label(), retry_mode.label());
    let ws = TestWorkspace::new_named(&format!("split-recovery-{case}"))?;
    ws.add_crate("mylib", "0.1.0", &[])?;
    ws.commit("Add prepared recovery fixture")?;

    let workspace_parent = ws.path.parent().expect("test workspace must have a parent");
    let target = tempfile::Builder::new()
        .prefix(&format!("cargo-rail-{case}-target-"))
        .tempdir_in(workspace_parent)?;
    git(target.path(), &["init", "--initial-branch=main"])?;

    let mut publication_remote = None;
    let remote = if matches!(boundary, SplitRecoveryBoundary::Published) {
        let remote_parent = TempDir::new()?;
        let target_name = target
            .path()
            .file_name()
            .expect("temporary split target has a file name")
            .to_string_lossy();
        let bare = remote_parent.path().join(format!("{target_name}.git"));
        git(
            remote_parent.path(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                bare.to_str().expect("bare remote path is UTF-8"),
            ],
        )?;
        let remote = format!("file://{}", bare.display());
        git(target.path(), &["remote", "add", "origin", &remote])?;
        publication_remote = Some((remote_parent, bare));
        remote
    } else {
        target.path().display().to_string().replace('\\', "\\\\")
    };
    std::fs::write(
        ws.path.join("rail.toml"),
        format!("[crates.mylib.split]\nremote = \"{remote}\"\nbranch = \"main\"\nmode = \"single\"\n"),
    )?;
    ws.commit("Configure prepared recovery fixture")?;

    let checked = run_cargo_rail(
        &ws.path,
        &[
            "rail",
            "split",
            "run",
            "mylib",
            "--check",
            "--allow-dirty",
            "-f",
            "json",
        ],
    )?;
    anyhow::ensure!(
        checked.status.code() == Some(1),
        "{case}: initial check failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&checked.stdout),
        String::from_utf8_lossy(&checked.stderr)
    );
    let saved_plan = ws.path.join(format!(".git/split-recovery-{case}-plan.json"));
    std::fs::write(&saved_plan, &checked.stdout)?;

    let effect_root = target.path().join(".git/cargo-rail/effects-v1");
    let (path, marker, real_git) = split_recovery_fault_wrapper(&ws.path, &effect_root, boundary)?;
    let mut interrupted_command = cargo_rail_command(&ws.path)?;
    apply_split_recovery_fault_environment(
        &mut interrupted_command,
        &path,
        &marker,
        &real_git,
        &effect_root,
        boundary,
    );
    let interrupted = interrupted_command
        .args([
            "rail",
            "split",
            "run",
            "mylib",
            "--yes",
            "--allow-dirty",
            "--plan",
            saved_plan.to_str().expect("saved split plan path is UTF-8"),
        ])
        .output()?;
    anyhow::ensure!(
        !interrupted.status.success(),
        "{case}: injected interruption unexpectedly succeeded"
    );
    anyhow::ensure!(
        marker.exists(),
        "{case}: the requested durable-boundary fault did not run"
    );

    let active = regular_files(&effect_root.join("active"), "json")?;
    let completed = regular_files(&effect_root.join("completed"), "json")?;
    let packs = regular_files(&effect_root.join("objects"), "pack")?;
    anyhow::ensure!(packs.len() == 1, "{case}: expected one retained prepared pack");
    let interrupted_effect_id = packs[0]
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("{case}: retained pack has no UTF-8 effect identity"))?
        .to_string();
    match boundary {
        SplitRecoveryBoundary::PackPersisted => {
            anyhow::ensure!(
                active.is_empty() && completed.is_empty(),
                "{case}: pack-only state gained a journal"
            );
            anyhow::ensure!(
                !target.path().join(".git/refs/heads/main").exists(),
                "{case}: pack-only interruption changed the branch ref"
            );
        }
        SplitRecoveryBoundary::RefCas | SplitRecoveryBoundary::PartialMaterialization => {
            anyhow::ensure!(
                active.len() == 1 && completed.is_empty(),
                "{case}: expected one active journal"
            );
            let journal: serde_json::Value = serde_json::from_slice(&std::fs::read(&active[0])?)?;
            anyhow::ensure!(journal["phase"] == "prepared", "{case}: ref-CAS journal advanced early");
            anyhow::ensure!(
                journal["effect_id"] == interrupted_effect_id,
                "{case}: journal and retained pack identities differ"
            );
            anyhow::ensure!(
                !target.path().join("Cargo.toml").exists(),
                "{case}: ref-CAS interruption materialized every target path"
            );
            if matches!(boundary, SplitRecoveryBoundary::PartialMaterialization) {
                std::fs::create_dir_all(target.path().join("src"))?;
                std::fs::copy(
                    ws.path.join("crates/mylib/src/lib.rs"),
                    target.path().join("src/lib.rs"),
                )?;
                anyhow::ensure!(
                    target.path().join("src/lib.rs").exists() && !target.path().join("README.md").exists(),
                    "{case}: partial materialization fixture did not retain mixed path images"
                );
            }
        }
        SplitRecoveryBoundary::JournalCompleted => {
            anyhow::ensure!(
                active.is_empty() && completed.len() == 1,
                "{case}: terminal journal was not completed"
            );
            let journal: serde_json::Value = serde_json::from_slice(&std::fs::read(&completed[0])?)?;
            anyhow::ensure!(
                journal["phase"] == "local_applied",
                "{case}: terminal journal has the wrong phase"
            );
            anyhow::ensure!(
                journal["effect_id"] == interrupted_effect_id,
                "{case}: completed journal and retained pack identities differ"
            );
            anyhow::ensure!(
                target.path().join("Cargo.toml").exists(),
                "{case}: completed effect lacks its paths"
            );
        }
        SplitRecoveryBoundary::Published => {
            anyhow::ensure!(
                active.len() == 1 && completed.is_empty(),
                "{case}: expected one active publication"
            );
            let journal: serde_json::Value = serde_json::from_slice(&std::fs::read(&active[0])?)?;
            anyhow::ensure!(
                journal["phase"] == "local_applied",
                "{case}: publication journal has the wrong phase"
            );
            anyhow::ensure!(
                journal["effect_id"] == interrupted_effect_id,
                "{case}: publication journal and retained pack identities differ"
            );
            let (_, bare) = publication_remote
                .as_ref()
                .expect("publication fixture has a bare remote");
            let local = git(target.path(), &["rev-parse", "refs/heads/main"])?;
            let remote = git(
                bare.parent().expect("bare remote has a parent"),
                &[
                    "--git-dir",
                    bare.to_str().expect("bare remote path is UTF-8"),
                    "rev-parse",
                    "refs/heads/main",
                ],
            )?;
            anyhow::ensure!(
                local.stdout == remote.stdout,
                "{case}: exact publication did not reach the remote"
            );
        }
    }

    if !matches!(boundary, SplitRecoveryBoundary::PackPersisted) {
        let fresh = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "split",
                "run",
                "mylib",
                "--check",
                "--allow-dirty",
                "-f",
                "json",
            ],
        )?;
        anyhow::ensure!(
            fresh.status.code() == Some(1),
            "{case}: fresh recovery planning did not remain pending\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&fresh.stdout),
            String::from_utf8_lossy(&fresh.stderr)
        );
        let fresh: serde_json::Value = serde_json::from_slice(&fresh.stdout)?;
        anyhow::ensure!(
            fresh["crates"][0]["prepared_effects"]
                .as_array()
                .is_some_and(|effects| effects
                    .iter()
                    .any(|effect| effect["effect_id"] == interrupted_effect_id)),
            "{case}: fresh planning did not bind the interrupted effect"
        );
    }

    let mut recovery_command = cargo_rail_command(&ws.path)?;
    recovery_command.args(["rail", "split", "run", "mylib", "--yes", "--allow-dirty"]);
    if matches!(retry_mode, SplitRetryMode::SavedPlan) {
        recovery_command.args(["--plan", saved_plan.to_str().expect("saved split plan path is UTF-8")]);
    }
    let recovered = recovery_command.output()?;
    anyhow::ensure!(
        recovered.status.success(),
        "{case}: recovery failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&recovered.stdout),
        String::from_utf8_lossy(&recovered.stderr)
    );
    anyhow::ensure!(
        split_receipt_effect_ids(&recovered.stdout)? == [interrupted_effect_id],
        "{case}: recovery completed a different or additional prepared effect"
    );
    anyhow::ensure!(regular_files(&effect_root.join("active"), "json")?.is_empty());
    anyhow::ensure!(regular_files(&effect_root.join("completed"), "json")?.is_empty());
    anyhow::ensure!(regular_files(&effect_root.join("objects"), "pack")?.is_empty());
    anyhow::ensure!(
        std::fs::read(target.path().join("src/lib.rs"))? == std::fs::read(ws.path.join("crates/mylib/src/lib.rs"))?,
        "{case}: recovered worktree does not contain the exact prepared bytes"
    );
    let head = String::from_utf8(git(target.path(), &["rev-parse", "HEAD"])?.stdout)?;
    let count = String::from_utf8(git(target.path(), &["rev-list", "--count", "HEAD"])?.stdout)?;
    anyhow::ensure!(
        count.trim() == "1",
        "{case}: recovery created more than one target commit"
    );
    anyhow::ensure!(
        String::from_utf8(git(target.path(), &["log", "--format=%s"])?.stdout)?
            .lines()
            .filter(|subject| *subject == "Add prepared recovery fixture")
            .count()
            == 1,
        "{case}: recovery did not converge the exact prepared source commit once"
    );
    anyhow::ensure!(
        String::from_utf8(git(target.path(), &["status", "--porcelain"])?.stdout)?.is_empty(),
        "{case}: recovered target is dirty"
    );

    if let Some((_, bare)) = publication_remote.as_ref() {
        anyhow::ensure!(
            git(
                bare.parent().expect("bare remote has a parent"),
                &[
                    "--git-dir",
                    bare.to_str().expect("bare remote path is UTF-8"),
                    "rev-parse",
                    "refs/heads/main",
                ],
            )?
            .stdout
                == head.as_bytes(),
            "{case}: recovered remote ref differs from the exact local result"
        );
    }

    let no_op = run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;
    anyhow::ensure!(
        no_op.status.success(),
        "{case}: idempotent retry failed: {}",
        String::from_utf8_lossy(&no_op.stderr)
    );
    anyhow::ensure!(git(target.path(), &["rev-parse", "HEAD"])?.stdout == head.as_bytes());
    anyhow::ensure!(git(target.path(), &["rev-list", "--count", "HEAD"])?.stdout == count.as_bytes());
    Ok(())
}

#[cfg(unix)]
macro_rules! split_recovery_case {
    ($name:ident, $boundary:expr, $retry_mode:expr) => {
        #[test]
        fn $name() {
            super::helpers::finish_test(exercise_split_recovery_boundary($boundary, $retry_mode));
        }
    };
}

#[cfg(unix)]
split_recovery_case!(
    test_split_recovery_after_pack_persistence_with_fresh_plan,
    SplitRecoveryBoundary::PackPersisted,
    SplitRetryMode::Fresh
);
#[cfg(unix)]
split_recovery_case!(
    test_split_recovery_after_pack_persistence_with_saved_plan,
    SplitRecoveryBoundary::PackPersisted,
    SplitRetryMode::SavedPlan
);
#[cfg(unix)]
split_recovery_case!(
    test_split_recovery_after_ref_cas_with_fresh_plan,
    SplitRecoveryBoundary::RefCas,
    SplitRetryMode::Fresh
);
#[cfg(unix)]
split_recovery_case!(
    test_split_recovery_after_ref_cas_with_saved_plan,
    SplitRecoveryBoundary::RefCas,
    SplitRetryMode::SavedPlan
);
#[cfg(unix)]
split_recovery_case!(
    test_split_recovery_after_partial_materialization_with_fresh_plan,
    SplitRecoveryBoundary::PartialMaterialization,
    SplitRetryMode::Fresh
);
#[cfg(unix)]
split_recovery_case!(
    test_split_recovery_after_partial_materialization_with_saved_plan,
    SplitRecoveryBoundary::PartialMaterialization,
    SplitRetryMode::SavedPlan
);
#[cfg(unix)]
split_recovery_case!(
    test_split_recovery_after_journal_completion_with_fresh_plan,
    SplitRecoveryBoundary::JournalCompleted,
    SplitRetryMode::Fresh
);
#[cfg(unix)]
split_recovery_case!(
    test_split_recovery_after_journal_completion_with_saved_plan,
    SplitRecoveryBoundary::JournalCompleted,
    SplitRetryMode::SavedPlan
);
#[cfg(unix)]
split_recovery_case!(
    test_split_recovery_after_publication_with_fresh_plan,
    SplitRecoveryBoundary::Published,
    SplitRetryMode::Fresh
);
#[cfg(unix)]
split_recovery_case!(
    test_split_recovery_after_publication_with_saved_plan,
    SplitRecoveryBoundary::Published,
    SplitRetryMode::SavedPlan
);

#[cfg(unix)]
struct IncrementalSplitFixture {
    ws: TestWorkspace,
    target: TempDir,
    saved_plan: PathBuf,
    baseline_head: String,
    baseline_index: Vec<u8>,
    baseline_lib: Vec<u8>,
    sentinel: PathBuf,
}

#[cfg(unix)]
fn setup_incremental_split_fixture(case: &str) -> Result<IncrementalSplitFixture> {
    let ws = TestWorkspace::new_named(case)?;
    ws.add_crate("mylib", "0.1.0", &[])?;
    ws.commit("Add branch-bound split fixture")?;
    let target = initialized_split_target("main")?;
    std::fs::write(
        ws.path.join("rail.toml"),
        format!(
            "[crates.mylib.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
            target.path().display().to_string().replace('\\', "\\\\")
        ),
    )?;
    ws.commit("Configure branch-bound split fixture")?;
    let initial = run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;
    anyhow::ensure!(
        initial.status.success(),
        "initial split failed: {}",
        String::from_utf8_lossy(&initial.stderr)
    );
    let baseline_head = String::from_utf8(git(target.path(), &["rev-parse", "refs/heads/main"])?.stdout)?
        .trim()
        .to_string();
    git(target.path(), &["branch", "other", &baseline_head])?;
    let baseline_index = std::fs::read(target.path().join(".git/index"))?;
    let baseline_lib = std::fs::read(target.path().join("src/lib.rs"))?;
    let sentinel = ws.path.join(format!(".git/{case}-outside-target-sentinel"));
    std::fs::write(&sentinel, b"unrelated\n")?;

    ws.modify_file("mylib", "src/lib.rs", "pub fn journaled_result() -> bool { true }\n")?;
    ws.commit("Prepare branch-bound split result")?;
    let checked = run_cargo_rail(
        &ws.path,
        &[
            "rail",
            "split",
            "run",
            "mylib",
            "--check",
            "--allow-dirty",
            "-f",
            "json",
        ],
    )?;
    anyhow::ensure!(
        checked.status.code() == Some(1),
        "incremental split check did not plan work"
    );
    let saved_plan = ws.path.join(format!(".git/{case}-plan.json"));
    std::fs::write(&saved_plan, &checked.stdout)?;
    Ok(IncrementalSplitFixture {
        ws,
        target,
        saved_plan,
        baseline_head,
        baseline_index,
        baseline_lib,
        sentinel,
    })
}

#[cfg(unix)]
fn branch_boundary_wrapper(
    workspace: &Path,
    target: &Path,
    switch_after_cas: bool,
) -> Result<(std::ffi::OsString, PathBuf, String)> {
    use std::os::unix::fs::PermissionsExt as _;

    let real_git = Command::new("sh").args(["-c", "command -v git"]).output()?;
    anyhow::ensure!(real_git.status.success());
    let real_git = String::from_utf8(real_git.stdout)?.trim().to_string();
    let label = if switch_after_cas { "after-cas" } else { "before-cas" };
    let wrapper_dir = workspace.join(format!(".git/branch-boundary-{label}-bin"));
    std::fs::create_dir_all(&wrapper_dir)?;
    let wrapper = wrapper_dir.join("git");
    std::fs::write(
        &wrapper,
        r#"#!/bin/sh
cas=0
for argument in "$@"; do
  if [ "$argument" = "update-ref" ]; then cas=1; fi
done
if [ "$cas" = 1 ] && [ ! -f "$CARGO_RAIL_TEST_BRANCH_MARKER" ]; then
  if [ "$CARGO_RAIL_TEST_BRANCH_TIMING" = "before" ]; then
    : > "$CARGO_RAIL_TEST_BRANCH_MARKER"
    exit 86
  fi
  "$CARGO_RAIL_TEST_REAL_GIT" "$@"
  result=$?
  if [ "$result" -ne 0 ]; then exit "$result"; fi
  "$CARGO_RAIL_TEST_REAL_GIT" -C "$CARGO_RAIL_TEST_BRANCH_TARGET" symbolic-ref HEAD refs/heads/other
  result=$?
  if [ "$result" -ne 0 ]; then exit "$result"; fi
  : > "$CARGO_RAIL_TEST_BRANCH_MARKER"
  exit 0
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
    let marker = workspace.join(format!(".git/branch-boundary-{label}-injected"));
    anyhow::ensure!(!target.as_os_str().is_empty());
    Ok((path, marker, real_git))
}

#[cfg(unix)]
fn run_branch_bound_split(
    fixture: &IncrementalSplitFixture,
    path: &std::ffi::OsStr,
    marker: &Path,
    real_git: &str,
    timing: &str,
) -> Result<std::process::Output> {
    Ok(cargo_rail_command(&fixture.ws.path)?
        .env("PATH", path)
        .env("CARGO_RAIL_TEST_REAL_GIT", real_git)
        .env("CARGO_RAIL_TEST_BRANCH_MARKER", marker)
        .env("CARGO_RAIL_TEST_BRANCH_TARGET", fixture.target.path())
        .env("CARGO_RAIL_TEST_BRANCH_TIMING", timing)
        .args([
            "rail",
            "split",
            "run",
            "mylib",
            "--yes",
            "--allow-dirty",
            "--plan",
            fixture.saved_plan.to_str().expect("saved split plan path is UTF-8"),
        ])
        .output()?)
}

#[cfg(unix)]
fn assert_branch_switch_preserved_unrelated_state(fixture: &IncrementalSplitFixture) -> Result<()> {
    anyhow::ensure!(
        String::from_utf8(git(fixture.target.path(), &["symbolic-ref", "HEAD"])?.stdout)?.trim() == "refs/heads/other"
    );
    anyhow::ensure!(
        String::from_utf8(git(fixture.target.path(), &["rev-parse", "refs/heads/other"])?.stdout)?.trim()
            == fixture.baseline_head
    );
    anyhow::ensure!(std::fs::read(fixture.target.path().join(".git/index"))? == fixture.baseline_index);
    anyhow::ensure!(std::fs::read(fixture.target.path().join("src/lib.rs"))? == fixture.baseline_lib);
    anyhow::ensure!(String::from_utf8(git(fixture.target.path(), &["status", "--porcelain"])?.stdout)?.is_empty());
    anyhow::ensure!(std::fs::read(&fixture.sentinel)? == b"unrelated\n");
    Ok(())
}

#[cfg(unix)]
#[test]
fn test_split_branch_switch_before_ref_cas_stops_without_mutation() {
    let result: Result<()> = (|| {
        let fixture = setup_incremental_split_fixture("split-branch-before-cas")?;
        let (path, marker, real_git) = branch_boundary_wrapper(&fixture.ws.path, fixture.target.path(), false)?;
        let interrupted = run_branch_bound_split(&fixture, &path, &marker, &real_git, "before")?;
        anyhow::ensure!(!interrupted.status.success() && marker.exists());
        anyhow::ensure!(
            String::from_utf8(git(fixture.target.path(), &["rev-parse", "refs/heads/main"])?.stdout)?.trim()
                == fixture.baseline_head,
            "the pre-CAS interruption changed the bound branch"
        );

        git(fixture.target.path(), &["symbolic-ref", "HEAD", "refs/heads/other"])?;
        let blocked = run_cargo_rail(
            &fixture.ws.path,
            &[
                "rail",
                "split",
                "run",
                "mylib",
                "--yes",
                "--allow-dirty",
                "--plan",
                fixture.saved_plan.to_str().unwrap(),
            ],
        )?;
        anyhow::ensure!(!blocked.status.success());
        let diagnostic = String::from_utf8_lossy(&blocked.stderr);
        anyhow::ensure!(
            diagnostic.contains("symbolic HEAD") || diagnostic.contains("no longer matches the terminal old/result"),
            "unexpected diagnostic: {diagnostic}"
        );
        anyhow::ensure!(
            String::from_utf8(git(fixture.target.path(), &["rev-parse", "refs/heads/main"])?.stdout)?.trim()
                == fixture.baseline_head
        );
        assert_branch_switch_preserved_unrelated_state(&fixture)
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_split_branch_switch_after_ref_cas_stops_before_materialization() {
    let result: Result<()> = (|| {
        let fixture = setup_incremental_split_fixture("split-branch-after-cas")?;
        let (path, marker, real_git) = branch_boundary_wrapper(&fixture.ws.path, fixture.target.path(), true)?;
        let blocked = run_branch_bound_split(&fixture, &path, &marker, &real_git, "after")?;
        anyhow::ensure!(!blocked.status.success() && marker.exists());
        let diagnostic = String::from_utf8_lossy(&blocked.stderr);
        anyhow::ensure!(
            diagnostic.contains("symbolic HEAD"),
            "unexpected diagnostic: {diagnostic}"
        );
        anyhow::ensure!(
            String::from_utf8(git(fixture.target.path(), &["rev-parse", "refs/heads/main"])?.stdout)?.trim()
                != fixture.baseline_head,
            "the fixture did not switch branches after the prepared ref CAS"
        );
        assert_branch_switch_preserved_unrelated_state(&fixture)
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
struct SplitPublicationFixture {
    ws: TestWorkspace,
    target: TempDir,
    _remote_parent: TempDir,
    bare_remote: PathBuf,
    saved_plan: PathBuf,
    baseline_remote_head: String,
    substitute_oid: String,
    sentinel: PathBuf,
}

#[cfg(unix)]
fn setup_split_publication_fixture(case: &str) -> Result<SplitPublicationFixture> {
    let ws = TestWorkspace::new_named(case)?;
    ws.add_crate("mylib", "0.1.0", &[])?;
    ws.commit("Add exact-publication fixture")?;
    let workspace_parent = ws.path.parent().expect("test workspace must have a parent");
    let target = tempfile::Builder::new()
        .prefix(&format!("cargo-rail-{case}-target-"))
        .tempdir_in(workspace_parent)?;
    git(target.path(), &["init", "--initial-branch=main"])?;
    std::fs::write(
        ws.path.join("rail.toml"),
        format!(
            "[crates.mylib.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
            target.path().display().to_string().replace('\\', "\\\\")
        ),
    )?;
    ws.commit("Configure local publication fixture")?;
    let initial = run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;
    anyhow::ensure!(initial.status.success(), "initial split failed");

    let local_baseline = String::from_utf8(git(target.path(), &["rev-parse", "refs/heads/main"])?.stdout)?
        .trim()
        .to_string();
    git(target.path(), &["branch", "other", &local_baseline])?;

    let remote_parent = TempDir::new()?;
    let target_name = target
        .path()
        .file_name()
        .expect("temporary split target has a file name")
        .to_string_lossy();
    let bare_remote = remote_parent.path().join(format!("{target_name}.git"));
    git(
        remote_parent.path(),
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            bare_remote.to_str().expect("bare remote path is UTF-8"),
        ],
    )?;
    let remote_url = format!("file://{}", bare_remote.display());
    git(target.path(), &["remote", "add", "origin", &remote_url])?;
    git(target.path(), &["push", "origin", "main"])?;
    let baseline_remote_head = String::from_utf8(
        git(
            remote_parent.path(),
            &[
                "--git-dir",
                bare_remote.to_str().expect("bare remote path is UTF-8"),
                "rev-parse",
                "refs/heads/main",
            ],
        )?
        .stdout,
    )?
    .trim()
    .to_string();
    anyhow::ensure!(baseline_remote_head == local_baseline);

    std::fs::write(
        ws.path.join("rail.toml"),
        format!("[crates.mylib.split]\nremote = \"{remote_url}\"\nbranch = \"main\"\nmode = \"single\"\n"),
    )?;
    ws.commit("Configure exact publication")?;
    ws.modify_file(
        "mylib",
        "src/lib.rs",
        "pub fn journaled_publication_result() -> bool { true }\n",
    )?;
    ws.commit("Prepare exact publication result")?;
    let checked = run_cargo_rail(
        &ws.path,
        &[
            "rail",
            "split",
            "run",
            "mylib",
            "--check",
            "--allow-dirty",
            "-f",
            "json",
        ],
    )?;
    anyhow::ensure!(checked.status.code() == Some(1));
    let saved_plan = ws.path.join(format!(".git/{case}-plan.json"));
    std::fs::write(&saved_plan, &checked.stdout)?;
    let sentinel = ws.path.join(format!(".git/{case}-publication-sentinel"));
    std::fs::write(&sentinel, b"unrelated-publication-state\n")?;
    Ok(SplitPublicationFixture {
        ws,
        target,
        _remote_parent: remote_parent,
        bare_remote,
        saved_plan,
        substitute_oid: baseline_remote_head.clone(),
        baseline_remote_head,
        sentinel,
    })
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum PublicationFault {
    SwitchHeadBeforePush,
    StopBeforePush,
}

#[cfg(unix)]
fn publication_boundary_wrapper(
    fixture: &SplitPublicationFixture,
    fault: PublicationFault,
) -> Result<(std::ffi::OsString, PathBuf, String)> {
    use std::os::unix::fs::PermissionsExt as _;

    let real_git = Command::new("sh").args(["-c", "command -v git"]).output()?;
    anyhow::ensure!(real_git.status.success());
    let real_git = String::from_utf8(real_git.stdout)?.trim().to_string();
    let label = match fault {
        PublicationFault::SwitchHeadBeforePush => "switch-head",
        PublicationFault::StopBeforePush => "stop-before-push",
    };
    let wrapper_dir = fixture.ws.path.join(format!(".git/publication-{label}-bin"));
    std::fs::create_dir_all(&wrapper_dir)?;
    let wrapper = wrapper_dir.join("git");
    std::fs::write(
        &wrapper,
        r#"#!/bin/sh
trigger=0
case "$CARGO_RAIL_TEST_PUBLICATION_FAULT" in
  stop-before-push)
    for argument in "$@"; do
      if [ "$argument" = "push" ]; then trigger=1; fi
    done
    ;;
  switch-head)
    ls_remote=0
    publication_range=0
    for argument in "$@"; do
      if [ "$argument" = "ls-remote" ]; then ls_remote=1; fi
      if [ "$argument" = "rev-list" ]; then publication_range=$((publication_range + 1)); fi
      if [ "$argument" = "--reverse" ]; then publication_range=$((publication_range + 1)); fi
    done
    if [ "$ls_remote" = 1 ]; then
      for journal in "$CARGO_RAIL_TEST_PUBLICATION_EFFECT_ROOT"/active/*.json; do
        if [ -f "$journal" ] && grep -q '"phase": "local_applied"' "$journal"; then
          "$CARGO_RAIL_TEST_REAL_GIT" "$@"
          result=$?
          if [ "$result" -ne 0 ]; then exit "$result"; fi
          : > "$CARGO_RAIL_TEST_PUBLICATION_MARKER.armed"
          exit 0
        fi
      done
    fi
    if [ "$publication_range" = 2 ] && [ -f "$CARGO_RAIL_TEST_PUBLICATION_MARKER.armed" ]; then trigger=1; fi
    ;;
esac
if [ "$trigger" = 1 ] && [ ! -f "$CARGO_RAIL_TEST_PUBLICATION_MARKER" ]; then
  if [ "$CARGO_RAIL_TEST_PUBLICATION_FAULT" = "switch-head" ]; then
    "$CARGO_RAIL_TEST_REAL_GIT" "$@"
    result=$?
    if [ "$result" -ne 0 ]; then exit "$result"; fi
    "$CARGO_RAIL_TEST_REAL_GIT" -C "$CARGO_RAIL_TEST_PUBLICATION_TARGET" symbolic-ref HEAD refs/heads/other
    result=$?
    if [ "$result" -ne 0 ]; then exit "$result"; fi
    : > "$CARGO_RAIL_TEST_PUBLICATION_MARKER"
    exit 0
  fi
  : > "$CARGO_RAIL_TEST_PUBLICATION_MARKER"
  exit 86
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
    let marker = fixture.ws.path.join(format!(".git/publication-{label}-injected"));
    Ok((path, marker, real_git))
}

#[cfg(unix)]
fn run_publication_fault(fixture: &SplitPublicationFixture, fault: PublicationFault) -> Result<std::process::Output> {
    let (path, marker, real_git) = publication_boundary_wrapper(fixture, fault)?;
    let fault_name = match fault {
        PublicationFault::SwitchHeadBeforePush => "switch-head",
        PublicationFault::StopBeforePush => "stop-before-push",
    };
    let output = cargo_rail_command(&fixture.ws.path)?
        .env("PATH", path)
        .env("CARGO_RAIL_TEST_REAL_GIT", real_git)
        .env("CARGO_RAIL_TEST_PUBLICATION_MARKER", &marker)
        .env("CARGO_RAIL_TEST_PUBLICATION_TARGET", fixture.target.path())
        .env(
            "CARGO_RAIL_TEST_PUBLICATION_EFFECT_ROOT",
            fixture.target.path().join(".git/cargo-rail/effects-v1"),
        )
        .env("CARGO_RAIL_TEST_PUBLICATION_FAULT", fault_name)
        .args([
            "rail",
            "split",
            "run",
            "mylib",
            "--yes",
            "--allow-dirty",
            "--plan",
            fixture.saved_plan.to_str().expect("saved split plan path is UTF-8"),
        ])
        .output()?;
    anyhow::ensure!(marker.exists(), "publication fault did not reach its boundary");
    Ok(output)
}

#[cfg(unix)]
fn bare_main(fixture: &SplitPublicationFixture) -> Result<String> {
    Ok(String::from_utf8(
        git(
            fixture.bare_remote.parent().expect("bare remote has a parent"),
            &[
                "--git-dir",
                fixture.bare_remote.to_str().expect("bare remote path is UTF-8"),
                "rev-parse",
                "refs/heads/main",
            ],
        )?
        .stdout,
    )?
    .trim()
    .to_string())
}

#[cfg(unix)]
fn snapshot_files(root: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    fn visit(root: &Path, current: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) -> Result<()> {
        for entry in std::fs::read_dir(current)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                visit(root, &entry.path(), files)?;
            } else {
                files.push((
                    entry.path().strip_prefix(root)?.to_path_buf(),
                    std::fs::read(entry.path())?,
                ));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

#[cfg(unix)]
#[test]
fn test_split_publication_rejects_concurrent_symbolic_head_substitution() {
    let result: Result<()> = (|| {
        let fixture = setup_split_publication_fixture("split-publication-head-substitution")?;
        let blocked = run_publication_fault(&fixture, PublicationFault::SwitchHeadBeforePush)?;
        anyhow::ensure!(!blocked.status.success());
        let diagnostic = String::from_utf8_lossy(&blocked.stderr);
        anyhow::ensure!(
            diagnostic.contains("symbolic HEAD"),
            "unexpected diagnostic: {diagnostic}"
        );
        let desired = String::from_utf8(git(fixture.target.path(), &["rev-parse", "refs/heads/main"])?.stdout)?
            .trim()
            .to_string();
        anyhow::ensure!(desired != fixture.substitute_oid);
        anyhow::ensure!(
            String::from_utf8(git(fixture.target.path(), &["rev-parse", "HEAD"])?.stdout)?.trim()
                == fixture.substitute_oid
        );
        anyhow::ensure!(bare_main(&fixture)? == fixture.baseline_remote_head);
        let remote_objects = String::from_utf8(
            git(
                fixture.bare_remote.parent().unwrap(),
                &["--git-dir", fixture.bare_remote.to_str().unwrap(), "rev-list", "--all"],
            )?
            .stdout,
        )?;
        anyhow::ensure!(!remote_objects.lines().any(|oid| oid == desired));
        anyhow::ensure!(std::fs::read(&fixture.sentinel)? == b"unrelated-publication-state\n");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_split_publication_preserves_remote_third_state() {
    let result: Result<()> = (|| {
        let fixture = setup_split_publication_fixture("split-publication-remote-third-state")?;
        let interrupted = run_publication_fault(&fixture, PublicationFault::StopBeforePush)?;
        anyhow::ensure!(!interrupted.status.success());
        anyhow::ensure!(bare_main(&fixture)? == fixture.baseline_remote_head);
        let desired = String::from_utf8(git(fixture.target.path(), &["rev-parse", "refs/heads/main"])?.stdout)?
            .trim()
            .to_string();

        let publisher = TempDir::new()?;
        git(publisher.path(), &["clone", fixture.bare_remote.to_str().unwrap(), "."])?;
        git(publisher.path(), &["config", "user.name", "Concurrent Publisher"])?;
        git(publisher.path(), &["config", "user.email", "publisher@example.com"])?;
        std::fs::write(publisher.path().join("REMOTE-THIRD.txt"), b"preserve me\n")?;
        git(publisher.path(), &["add", "REMOTE-THIRD.txt"])?;
        git(publisher.path(), &["commit", "-m", "Concurrent remote third state"])?;
        git(publisher.path(), &["push", "origin", "main"])?;
        let third = bare_main(&fixture)?;
        anyhow::ensure!(third != fixture.baseline_remote_head && third != desired);
        git(
            fixture.target.path(),
            &["fetch", "--no-tags", "origin", "refs/heads/main"],
        )?;
        anyhow::ensure!(
            String::from_utf8(git(fixture.target.path(), &["rev-parse", "refs/heads/main"])?.stdout)?.trim() == desired
        );
        let remote_before = snapshot_files(&fixture.bare_remote)?;

        let blocked = run_cargo_rail(
            &fixture.ws.path,
            &[
                "rail",
                "split",
                "run",
                "mylib",
                "--yes",
                "--allow-dirty",
                "--plan",
                fixture.saved_plan.to_str().unwrap(),
            ],
        )?;
        anyhow::ensure!(!blocked.status.success());
        let diagnostic = String::from_utf8_lossy(&blocked.stderr);
        anyhow::ensure!(
            diagnostic.contains("third remote ref state") || diagnostic.contains("have diverged"),
            "unexpected diagnostic: {diagnostic}"
        );
        anyhow::ensure!(bare_main(&fixture)? == third);
        anyhow::ensure!(snapshot_files(&fixture.bare_remote)? == remote_before);
        anyhow::ensure!(
            git(
                fixture.bare_remote.parent().unwrap(),
                &[
                    "--git-dir",
                    fixture.bare_remote.to_str().unwrap(),
                    "show",
                    &format!("{third}:REMOTE-THIRD.txt"),
                ],
            )?
            .stdout
                == b"preserve me\n"
        );
        anyhow::ensure!(
            String::from_utf8(git(fixture.target.path(), &["rev-parse", "refs/heads/main"])?.stdout)?.trim() == desired
        );
        anyhow::ensure!(std::fs::read(&fixture.sentinel)? == b"unrelated-publication-state\n");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_single_crate_basic() {
    let result: Result<()> = (|| {
        // Create monorepo with a single crate
        let ws = TestWorkspace::new()?;
        ws.add_crate("mylib", "0.1.0", &[])?;
        ws.commit("Add mylib")?;

        // Create split repo target
        let split_dir = initialized_split_target("main")?;
        let split_path = split_dir.path();

        // Create rail.toml config
        let config = format!(
            r#"[crates.mylib.split]
remote = "{}"
branch = "main"
mode = "single"
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_rejects_hidden_target_index_state_before_ref_mutation() {
    let result: Result<()> = (|| {
        for (case, flag) in [
            ("assume-unchanged", "--assume-unchanged"),
            ("skip-worktree", "--skip-worktree"),
        ] {
            let ws = TestWorkspace::new_named(&format!("split-hidden-index-{case}"))?;
            ws.add_crate("mylib", "0.1.0", &[])?;
            ws.commit("Add hidden-index split fixture")?;
            let target = initialized_split_target("main")?;
            std::fs::write(
                ws.path.join("rail.toml"),
                format!(
                    "[crates.mylib.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
                    target.path().display().to_string().replace('\\', "\\\\")
                ),
            )?;
            run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;

            ws.modify_file("mylib", "src/lib.rs", "pub fn hidden_index_change() {}\n")?;
            ws.commit("Prepare split across hidden target index state")?;
            let head = git(target.path(), &["rev-parse", "refs/heads/main"])?.stdout;
            git(target.path(), &["update-index", flag, "--", "src/lib.rs"])?;

            let rejected = run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;
            anyhow::ensure!(
                !rejected.status.success(),
                "split accepted the {case} target index state"
            );
            anyhow::ensure!(
                String::from_utf8_lossy(&rejected.stderr).contains("unsupported state flag"),
                "split did not classify the {case} target index state:\n{}",
                String::from_utf8_lossy(&rejected.stderr)
            );
            anyhow::ensure!(
                git(target.path(), &["rev-parse", "refs/heads/main"])?.stdout == head,
                "split changed the target ref after rejecting {case}"
            );
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_split_recovers_after_ref_cas_before_path_convergence() {
    use std::os::unix::fs::PermissionsExt as _;

    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-prepared-ref-recovery")?;
        ws.add_crate("mylib", "0.1.0", &[])?;
        ws.commit("Add prepared split fixture")?;
        let target = initialized_split_target("main")?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[crates.mylib.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
                target.path().display().to_string().replace('\\', "\\\\")
            ),
        )?;
        let checked = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "split",
                "run",
                "mylib",
                "--check",
                "--allow-dirty",
                "-f",
                "json",
            ],
        )?;
        assert_eq!(
            checked.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&checked.stdout),
            String::from_utf8_lossy(&checked.stderr),
        );
        let saved_plan = ws.path.join(".git/prepared-split-plan.json");
        std::fs::write(&saved_plan, &checked.stdout)?;

        let real_git = std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()?;
        assert!(real_git.status.success());
        let real_git = String::from_utf8(real_git.stdout)?.trim().to_string();
        let wrapper_dir = ws.path.join(".git/prepared-split-bin");
        std::fs::create_dir_all(&wrapper_dir)?;
        let wrapper = wrapper_dir.join("git");
        std::fs::write(
            &wrapper,
            r#"#!/bin/sh
inject=0
for argument in "$@"; do
  if [ "$argument" = "update-ref" ]; then
    inject=1
  fi
done
if [ "$inject" = 1 ] && [ ! -f "$CARGO_RAIL_TEST_SPLIT_CAS_MARKER" ]; then
  "$CARGO_RAIL_TEST_REAL_GIT" "$@"
  result=$?
  if [ "$result" -ne 0 ]; then
    exit "$result"
  fi
  : > "$CARGO_RAIL_TEST_SPLIT_CAS_MARKER"
  exit 86
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
        let marker = ws.path.join(".git/prepared-split-cas-injected");
        let interrupted = cargo_rail_command(&ws.path)?
            .env("PATH", path)
            .env("CARGO_RAIL_TEST_REAL_GIT", real_git)
            .env("CARGO_RAIL_TEST_SPLIT_CAS_MARKER", &marker)
            .args([
                "rail",
                "split",
                "run",
                "mylib",
                "--yes",
                "--allow-dirty",
                "--plan",
                saved_plan.to_str().unwrap(),
            ])
            .output()?;
        assert!(!interrupted.status.success());
        assert!(marker.exists(), "the post-CAS interruption fixture did not run");
        let prepared_head = String::from_utf8(git(target.path(), &["rev-parse", "refs/heads/main"])?.stdout)?
            .trim()
            .to_string();
        assert!(
            !target.path().join("Cargo.toml").exists(),
            "the interruption must happen before path convergence"
        );
        let fresh = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "split",
                "run",
                "mylib",
                "--check",
                "--allow-dirty",
                "-f",
                "json",
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
        let projected_effect = fresh["crates"][0]["prepared_effects"][0]["effect_id"]
            .as_str()
            .expect("fresh retry plan must project the prepared effect")
            .to_string();

        let recovered = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "split",
                "run",
                "mylib",
                "--yes",
                "--allow-dirty",
                "--plan",
                saved_plan.to_str().unwrap(),
                "-f",
                "json",
            ],
        )?;
        assert!(
            recovered.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&recovered.stdout),
            String::from_utf8_lossy(&recovered.stderr)
        );
        let recovered_json: serde_json::Value = serde_json::from_slice(&recovered.stdout)?;
        let receipt_path = recovered_json["receipt"].as_str().expect("apply receipt path");
        let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(receipt_path)?)?;
        assert_eq!(
            receipt["plan"]["actions"][0]["payload"]["prepared_effects"][0]["effect_id"], projected_effect,
            "fresh planning and saved-plan recovery must bind the same effect"
        );
        assert!(target.path().join("Cargo.toml").exists());
        assert!(target.path().join("src/lib.rs").exists());
        assert_eq!(
            String::from_utf8(git(target.path(), &["rev-parse", "HEAD"])?.stdout)?.trim(),
            prepared_head
        );
        let count_after_recovery = String::from_utf8(git(target.path(), &["rev-list", "--count", "HEAD"])?.stdout)?;

        let retry = run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;
        assert!(retry.status.success(), "{}", String::from_utf8_lossy(&retry.stderr));
        assert_eq!(
            String::from_utf8(git(target.path(), &["rev-list", "--count", "HEAD"])?.stdout)?,
            count_after_recovery,
            "recovery and retry must not duplicate the prepared chain"
        );
        assert!(String::from_utf8(git(target.path(), &["status", "--porcelain"])?.stdout)?.is_empty());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_split_saved_plan_recovers_after_exact_publication() {
    use std::os::unix::fs::PermissionsExt as _;

    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-prepared-publication-recovery")?;
        ws.add_crate("mylib", "0.1.0", &[])?;
        ws.commit("Add publication recovery fixture")?;
        let workspace_parent = ws.path.parent().expect("test workspace must have a parent");
        let target = tempfile::Builder::new()
            .prefix("cargo-rail-publication-target-")
            .tempdir_in(workspace_parent)?;
        let target_name = target.path().file_name().unwrap().to_string_lossy();
        let remote_parent = TempDir::new()?;
        let remote = remote_parent.path().join(format!("{target_name}.git"));
        git(
            remote_parent.path(),
            &["init", "--bare", "--initial-branch=main", remote.to_str().unwrap()],
        )?;
        let remote_url = file_url(&remote);
        git(target.path(), &["init", "--initial-branch=main"])?;
        git(target.path(), &["remote", "add", "origin", &remote_url])?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!("[crates.mylib.split]\nremote = \"{remote_url}\"\nbranch = \"main\"\nmode = \"single\"\n"),
        )?;
        ws.commit("Configure publication recovery")?;

        let checked = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "split",
                "run",
                "mylib",
                "--check",
                "--allow-dirty",
                "-f",
                "json",
            ],
        )?;
        assert_eq!(checked.status.code(), Some(1));
        let saved_plan = ws.path.join(".git/prepared-publication-plan.json");
        std::fs::write(&saved_plan, &checked.stdout)?;

        let real_git = std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()?;
        assert!(real_git.status.success());
        let real_git = String::from_utf8(real_git.stdout)?.trim().to_string();
        let wrapper_dir = ws.path.join(".git/prepared-publication-bin");
        std::fs::create_dir_all(&wrapper_dir)?;
        let wrapper = wrapper_dir.join("git");
        std::fs::write(
            &wrapper,
            r#"#!/bin/sh
inject=0
for argument in "$@"; do
  if [ "$argument" = "push" ]; then
    inject=1
  fi
done
if [ "$inject" = 1 ] && [ ! -f "$CARGO_RAIL_TEST_SPLIT_PUSH_MARKER" ]; then
  "$CARGO_RAIL_TEST_REAL_GIT" "$@"
  result=$?
  if [ "$result" -ne 0 ]; then
    exit "$result"
  fi
  : > "$CARGO_RAIL_TEST_SPLIT_PUSH_MARKER"
  exit 86
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
        let marker = ws.path.join(".git/prepared-split-push-injected");
        let interrupted = cargo_rail_command(&ws.path)?
            .env("PATH", path)
            .env("CARGO_RAIL_TEST_REAL_GIT", real_git)
            .env("CARGO_RAIL_TEST_SPLIT_PUSH_MARKER", &marker)
            .args([
                "rail",
                "split",
                "run",
                "mylib",
                "--yes",
                "--allow-dirty",
                "--plan",
                saved_plan.to_str().unwrap(),
            ])
            .output()?;
        assert!(!interrupted.status.success());
        assert!(marker.exists(), "the post-publication interruption fixture did not run");

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
            "the exact publication must have completed before interruption"
        );

        let fresh = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "split",
                "run",
                "mylib",
                "--check",
                "--allow-dirty",
                "-f",
                "json",
            ],
        )?;
        assert_eq!(fresh.status.code(), Some(1));
        let fresh: serde_json::Value = serde_json::from_slice(&fresh.stdout)?;
        let projected_effect = fresh["crates"][0]["prepared_effects"][0]["effect_id"]
            .as_str()
            .expect("fresh publication retry must project the prepared effect")
            .to_string();

        let recovered = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "split",
                "run",
                "mylib",
                "--yes",
                "--allow-dirty",
                "--plan",
                saved_plan.to_str().unwrap(),
                "-f",
                "json",
            ],
        )?;
        assert!(
            recovered.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&recovered.stdout),
            String::from_utf8_lossy(&recovered.stderr)
        );
        let recovered: serde_json::Value = serde_json::from_slice(&recovered.stdout)?;
        let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(
            recovered["receipt"].as_str().expect("apply receipt path"),
        )?)?;
        assert_eq!(
            receipt["plan"]["actions"][0]["payload"]["prepared_effects"][0]["effect_id"],
            projected_effect
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_reconstructs_large_commit_messages_without_argument_payloads() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-large-message")?;
        ws.add_crate("mylib", "0.1.0", &[])?;
        ws.commit("Add large-message split fixture")?;
        ws.modify_file("mylib", "src/lib.rs", "pub fn large_message_revision() {}\n")?;
        git(&ws.path, &["add", "crates/mylib/src/lib.rs"])?;
        let message_dir = TempDir::new()?;
        let message_path = message_dir.path().join("message.txt");
        let marker = "ordinary-split-large-message-";
        std::fs::write(&message_path, format!("{marker}{}\n", "x".repeat(192 * 1024)))?;
        git(&ws.path, &["commit", "-F", message_path.to_str().unwrap()])?;

        let target = initialized_split_target("main")?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[crates.mylib.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
                target.path().display().to_string().replace('\\', "\\\\")
            ),
        )?;
        let output = run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let messages = String::from_utf8(git(target.path(), &["log", "--format=%B"])?.stdout)?;
        assert!(messages.contains(marker));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_requires_an_explicitly_initialized_target_without_absorbing_files() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-explicit-target-init")?;
        ws.add_crate("mylib", "0.1.0", &[])?;
        ws.commit("Add explicit target initialization fixture")?;
        let target = TempDir::new()?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[crates.mylib.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
                target.path().display().to_string().replace('\\', "\\\\")
            ),
        )?;

        let check = run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--check"])?;
        assert!(!check.status.success());
        assert!(String::from_utf8_lossy(&check.stderr).contains("not an initialized Git repository"));
        assert!(std::fs::read_dir(target.path())?.next().is_none());

        let marker = target.path().join("user-owned.txt");
        std::fs::write(&marker, b"preserve me exactly\n")?;
        let apply = run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;
        assert!(!apply.status.success());
        assert!(String::from_utf8_lossy(&apply.stderr).contains("not an initialized Git repository"));
        assert_eq!(std::fs::read(&marker)?, b"preserve me exactly\n");
        assert!(!target.path().join(".git").exists());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_initialized_unborn_target_publishes_by_exact_url_and_rechecks_without_tracking_ref() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-unborn-nonlocal-target")?;
        ws.add_crate("mylib", "0.1.0", &[])?;
        ws.commit("Add unborn non-local target fixture")?;
        let workspace_parent = ws.path.parent().expect("test workspace must have a parent");
        let target = tempfile::Builder::new()
            .prefix("cargo-rail-unborn-target-")
            .tempdir_in(workspace_parent)?;
        let target_name = target.path().file_name().unwrap().to_string_lossy();
        let remote_parent = TempDir::new()?;
        let remote = remote_parent.path().join(format!("{target_name}.git"));
        git(
            remote_parent.path(),
            &["init", "--bare", "--initial-branch=main", remote.to_str().unwrap()],
        )?;
        let remote_url = file_url(&remote);
        git(target.path(), &["init", "--initial-branch=main"])?;
        git(target.path(), &["remote", "add", "origin", &remote_url])?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!("[crates.mylib.split]\nremote = \"{remote_url}\"\nbranch = \"main\"\nmode = \"single\"\n"),
        )?;
        ws.commit("Configure unborn non-local target")?;

        let apply = run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;
        assert!(apply.status.success(), "{}", String::from_utf8_lossy(&apply.stderr));
        let local_head = String::from_utf8(git(target.path(), &["rev-parse", "HEAD"])?.stdout)?;
        let remote_head = String::from_utf8(
            git(
                remote_parent.path(),
                &["--git-dir", remote.to_str().unwrap(), "rev-parse", "main"],
            )?
            .stdout,
        )?;
        assert_eq!(local_head.trim(), remote_head.trim());
        assert!(
            git(
                target.path(),
                &["for-each-ref", "--format=%(refname)", "refs/remotes/origin/main",],
            )?
            .stdout
            .is_empty(),
            "exact-URL publication must not make a tracking ref authoritative"
        );

        let check = run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--check", "--json"])?;
        assert!(check.status.success(), "{}", String::from_utf8_lossy(&check.stderr));
        let json: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        assert_eq!(json["crates"][0]["pending_commits"], 0);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_v025_notes_only_check_is_read_only_and_apply_migrates_once() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-v025-notes")?;
        ws.add_crate("legacy-lib", "0.1.0", &[])?;
        let source_commit = ws.commit("Add legacy-lib")?;

        let target = TempDir::new()?;
        git(target.path(), &["init", "-b", "main"])?;
        git(target.path(), &["config", "user.name", "Test User"])?;
        git(target.path(), &["config", "user.email", "test@example.com"])?;
        std::fs::create_dir_all(target.path().join("src"))?;
        std::fs::write(
            target.path().join("Cargo.toml"),
            r#"[package]
name = "legacy-lib"
version = "0.1.0"
edition = "2021"
license = "MIT"
authors = ["Test Author"]

[dependencies]
"#,
        )?;
        std::fs::copy(
            ws.path.join("crates/legacy-lib/README.md"),
            target.path().join("README.md"),
        )?;
        std::fs::copy(
            ws.path.join("crates/legacy-lib/src/lib.rs"),
            target.path().join("src/lib.rs"),
        )?;
        git(target.path(), &["add", "."])?;
        git(target.path(), &["commit", "-m", "Legacy split head"])?;
        let legacy_target = String::from_utf8(git(target.path(), &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();
        git(
            target.path(),
            &["fetch", "--quiet", ws.path.to_str().unwrap(), &source_commit],
        )?;
        git(
            target.path(),
            &[
                "notes",
                "--ref",
                "refs/notes/rail/legacy-lib",
                "add",
                "-m",
                &legacy_target,
                &source_commit,
            ],
        )?;
        git(
            target.path(),
            &["remote", "add", "origin", "https://example.invalid/legacy-lib.git"],
        )?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[crates.legacy-lib.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
                target.path().display().to_string().replace('\\', "\\\\")
            ),
        )?;

        let notes_before = git(target.path(), &["rev-parse", "refs/notes/rail/legacy-lib"])?.stdout;
        let check = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "split",
                "run",
                "legacy-lib",
                "--check",
                "--json",
                "--allow-dirty",
            ],
        )?;
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
        assert_eq!(authority["direction"], "mono_to_remote");
        assert_eq!(authority["owner"], "legacy-lib");
        assert_eq!(authority["transform_version"], 1);
        assert_eq!(authority["pending_candidate_count"], 1);
        assert_eq!(authority["mapping_count"], 1);
        assert_eq!(authority["migration_digest"], migration_digest);
        assert!(authority["source_repository"].as_str().unwrap().starts_with("sha256-"));
        assert!(authority["target_repository"].as_str().unwrap().starts_with("sha256-"));
        assert_eq!(
            String::from_utf8(git(target.path(), &["rev-parse", "HEAD"])?.stdout)?.trim(),
            legacy_target
        );
        assert_eq!(
            git(target.path(), &["rev-parse", "refs/notes/rail/legacy-lib"],)?.stdout,
            notes_before
        );

        let plan_path = ws.path.join("split-origin-migration-plan.json");
        std::fs::write(&plan_path, serde_json::to_vec_pretty(&check_json["mutation_plan"])?)?;
        let predecessor_source =
            String::from_utf8(git(&ws.path, &["rev-parse", &format!("{source_commit}^")])?.stdout)?
                .trim()
                .to_string();
        git(
            target.path(),
            &["notes", "--ref", "refs/notes/rail/legacy-lib", "remove", &source_commit],
        )?;
        git(
            target.path(),
            &[
                "notes",
                "--ref",
                "refs/notes/rail/legacy-lib",
                "add",
                "-m",
                &legacy_target,
                &predecessor_source,
            ],
        )?;
        let drifted = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "split",
                "run",
                "legacy-lib",
                "--plan",
                plan_path.to_str().expect("test plan path must be UTF-8"),
                "--allow-dirty",
            ],
        )?;
        assert!(!drifted.status.success());
        assert_eq!(
            String::from_utf8(git(target.path(), &["rev-parse", "HEAD"])?.stdout)?.trim(),
            legacy_target
        );
        git(
            target.path(),
            &[
                "notes",
                "--ref",
                "refs/notes/rail/legacy-lib",
                "remove",
                &predecessor_source,
            ],
        )?;
        git(
            target.path(),
            &[
                "notes",
                "--ref",
                "refs/notes/rail/legacy-lib",
                "add",
                "-m",
                &legacy_target,
                &source_commit,
            ],
        )?;
        git(
            target.path(),
            &[
                "remote",
                "set-url",
                "origin",
                "https://example.invalid/drifted-legacy-lib.git",
            ],
        )?;
        let identity_drift = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "split",
                "run",
                "legacy-lib",
                "--plan",
                plan_path.to_str().expect("test plan path must be UTF-8"),
                "--allow-dirty",
            ],
        )?;
        assert!(!identity_drift.status.success());
        assert_eq!(
            String::from_utf8(git(target.path(), &["rev-parse", "HEAD"])?.stdout)?.trim(),
            legacy_target,
            "remote identity drift must fail before moving the target HEAD"
        );
        git(
            target.path(),
            &["remote", "set-url", "origin", "https://example.invalid/legacy-lib.git"],
        )?;
        let notes_before_apply = git(target.path(), &["rev-parse", "refs/notes/rail/legacy-lib"])?.stdout;

        let apply = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "legacy-lib", "--yes", "--allow-dirty"],
        )?;
        assert!(apply.status.success(), "{}", String::from_utf8_lossy(&apply.stderr));
        let migrated_head = String::from_utf8(git(target.path(), &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();
        assert_ne!(migrated_head, legacy_target);
        let message = String::from_utf8(git(target.path(), &["log", "-1", "--format=%B"])?.stdout)?;
        assert!(message.contains("chore: migrate cargo-rail origin mappings"));
        assert!(message.contains(&format!("commit={source_commit}")));
        assert!(message.contains(&format!("target={legacy_target}")));
        assert_eq!(
            git(target.path(), &["rev-parse", "refs/notes/rail/legacy-lib"],)?.stdout,
            notes_before_apply,
            "migration must leave predecessor notes read-only"
        );

        git(target.path(), &["update-ref", "-d", "refs/notes/rail/legacy-lib"])?;
        let ordinary_only = run_cargo_rail(&ws.path, &["rail", "split", "run", "legacy-lib", "--check", "--json"])?;
        assert_eq!(ordinary_only.status.code(), Some(0));
        assert_eq!(
            String::from_utf8(git(target.path(), &["rev-parse", "HEAD"])?.stdout)?.trim(),
            migrated_head
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_split_revalidates_predecessor_authority_before_nontrivial_target_mutation() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let ws = TestWorkspace::new_named("split-v025-live-drift")?;
        let crate_root = ws.add_crate("legacy-live", "0.1.0", &[])?;
        let source_commit = ws.commit("Add legacy-live")?;

        let target = TempDir::new()?;
        git(target.path(), &["init", "-b", "main"])?;
        git(target.path(), &["config", "user.name", "Test User"])?;
        git(target.path(), &["config", "user.email", "test@example.com"])?;
        git(
            target.path(),
            &["commit", "--allow-empty", "-m", "Legacy mapped target"],
        )?;
        let original_target = String::from_utf8(git(target.path(), &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();
        git(
            target.path(),
            &["commit", "--allow-empty", "-m", "Unmapped target head"],
        )?;
        let target_head = String::from_utf8(git(target.path(), &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();
        git(
            target.path(),
            &["fetch", "--quiet", ws.path.to_str().unwrap(), &source_commit],
        )?;
        git(
            target.path(),
            &[
                "notes",
                "--ref",
                "refs/notes/rail/legacy-live",
                "add",
                "-m",
                &original_target,
                &source_commit,
            ],
        )?;

        std::fs::write(crate_root.join("src/lib.rs"), "pub fn pending_split() {}\n")?;
        ws.commit("Add nontrivial pending split work")?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[crates.legacy-live.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
                target.path().display().to_string().replace('\\', "\\\\")
            ),
        )?;

        let real_git = std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()?;
        assert!(real_git.status.success());
        let real_git = String::from_utf8(real_git.stdout)?.trim().to_string();
        let wrapper_dir = ws.path.join(".git/rail-test-bin");
        std::fs::create_dir_all(&wrapper_dir)?;
        let wrapper = wrapper_dir.join("git");
        std::fs::write(
            &wrapper,
            r#"#!/bin/sh
if [ ! -f "$CARGO_RAIL_TEST_SPLIT_DRIFT_MARKER" ]; then
  for receipt in "$CARGO_RAIL_TEST_SPLIT_RECEIPTS"/split-*-plan-*.json; do
    if [ -f "$receipt" ]; then
    : > "$CARGO_RAIL_TEST_SPLIT_DRIFT_MARKER"
    "$CARGO_RAIL_TEST_REAL_GIT" -C "$CARGO_RAIL_TEST_SPLIT_TARGET" notes \
      --ref refs/notes/rail/legacy-live add -f -m "$CARGO_RAIL_TEST_SPLIT_REPLACEMENT" \
      "$CARGO_RAIL_TEST_SPLIT_SOURCE"
    break
    fi
  done
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
        let marker = ws.path.join(".git/split-drift-injected");

        let output = cargo_rail_command(&ws.path)?
            .env("PATH", path)
            .env("CARGO_RAIL_TEST_REAL_GIT", real_git)
            .env("CARGO_RAIL_TEST_SPLIT_DRIFT_MARKER", &marker)
            .env(
                "CARGO_RAIL_TEST_SPLIT_RECEIPTS",
                ws.path.join("target/cargo-rail/receipts"),
            )
            .env("CARGO_RAIL_TEST_SPLIT_TARGET", target.path())
            .env("CARGO_RAIL_TEST_SPLIT_REPLACEMENT", &target_head)
            .env("CARGO_RAIL_TEST_SPLIT_SOURCE", &source_commit)
            .args(["rail", "split", "run", "legacy-live", "--yes", "--allow-dirty"])
            .output()?;
        assert!(!output.status.success());
        assert!(marker.exists(), "the live-drift fixture did not run");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("changed after it was checked"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(git(target.path(), &["rev-parse", "HEAD"])?.stdout)?.trim(),
            target_head,
            "authority drift must fail before any nontrivial split target commit"
        );
        assert!(String::from_utf8(git(target.path(), &["status", "--porcelain"])?.stdout)?.is_empty());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_split_rejects_existing_target_disappearance_after_revalidation() {
    use std::os::unix::fs::PermissionsExt as _;

    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-target-disappearance")?;
        let crate_root = ws.add_crate("disappearing-target", "0.1.0", &[])?;
        let target = TempDir::new()?;
        git(target.path(), &["init", "--initial-branch=main"])?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[crates.disappearing-target.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
                target.path().display().to_string().replace('\\', "\\\\")
            ),
        )?;
        ws.commit("Add disappearing-target fixture")?;
        run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "disappearing-target", "--yes", "--allow-dirty"],
        )?;
        let target_head = String::from_utf8(git(target.path(), &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();
        let target_manifest = std::fs::read(target.path().join("Cargo.toml"))?;

        std::fs::write(
            crate_root.join("src/lib.rs"),
            "pub fn pending_after_target_check() {}\n",
        )?;
        ws.commit("Add pending split after target check")?;

        let backup = TempDir::new()?;
        let backup_git = backup.path().join("repository.git");
        let real_git = std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()?;
        assert!(real_git.status.success());
        let wrapper_dir = ws.path.join(".git/disappearing-target-bin");
        std::fs::create_dir_all(&wrapper_dir)?;
        let wrapper = wrapper_dir.join("git");
        std::fs::write(
            &wrapper,
            r#"#!/bin/sh
inject=0
for receipt in "$CARGO_RAIL_TEST_SPLIT_RECEIPTS"/split-*-plan-*.json; do
  if [ -f "$receipt" ]; then inject=1; fi
done
if [ "$inject" = 1 ] && [ ! -f "$CARGO_RAIL_TEST_SPLIT_DISAPPEAR_MARKER" ]; then
  "$CARGO_RAIL_TEST_REAL_GIT" "$@"
  result=$?
  if [ "$result" -ne 0 ]; then
    exit "$result"
  fi
  mv "$CARGO_RAIL_TEST_SPLIT_TARGET/.git" "$CARGO_RAIL_TEST_SPLIT_BACKUP"
  : > "$CARGO_RAIL_TEST_SPLIT_DISAPPEAR_MARKER"
  exit 0
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
        let marker = ws.path.join(".git/disappearing-target-injected");
        let output = cargo_rail_command(&ws.path)?
            .env("PATH", path)
            .env("CARGO_RAIL_TEST_REAL_GIT", String::from_utf8(real_git.stdout)?.trim())
            .env("CARGO_RAIL_TEST_SPLIT_TARGET", target.path())
            .env("CARGO_RAIL_TEST_SPLIT_BACKUP", &backup_git)
            .env("CARGO_RAIL_TEST_SPLIT_DISAPPEAR_MARKER", &marker)
            .env(
                "CARGO_RAIL_TEST_SPLIT_RECEIPTS",
                ws.path.join("target/cargo-rail/receipts"),
            )
            .args(["rail", "split", "run", "disappearing-target", "--yes", "--allow-dirty"])
            .output()?;
        let diagnostic = String::from_utf8_lossy(&output.stderr);

        assert!(
            marker.exists(),
            "the target-disappearance fixture did not reach the engine boundary"
        );
        assert!(
            !output.status.success(),
            "stale existing-target authority must fail closed"
        );
        assert!(
            diagnostic.contains("is not an initialized Git repository"),
            "{diagnostic}"
        );
        assert!(
            !target.path().join(".git").exists(),
            "split must not recreate a target whose checked repository disappeared"
        );
        assert_eq!(
            std::fs::read(target.path().join("Cargo.toml"))?,
            target_manifest,
            "split must not change the target worktree after authority loss"
        );

        std::fs::rename(&backup_git, target.path().join(".git"))?;
        assert!(target.path().join(".git/HEAD").is_file());
        assert_eq!(
            String::from_utf8(git(target.path(), &["--git-dir=.git", "--work-tree=.", "rev-parse", "HEAD"],)?.stdout,)?
                .trim(),
            target_head
        );
        assert!(
            String::from_utf8(
                git(
                    target.path(),
                    &["--git-dir=.git", "--work-tree=.", "status", "--porcelain"],
                )?
                .stdout,
            )?
            .is_empty()
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_split_rejects_selected_target_aliases_and_shared_remote_override() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::symlink;

        let ws = TestWorkspace::new_named("split-duplicate-targets")?;
        ws.add_crate("alias-a", "0.1.0", &[])?;
        ws.add_crate("alias-b", "0.1.0", &[])?;
        ws.commit("Add duplicate-target fixtures")?;
        let targets = TempDir::new()?;
        let canonical = targets.path().join("canonical");
        let alias = targets.path().join("alias");
        std::fs::create_dir_all(&canonical)?;
        symlink(&canonical, &alias)?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[crates.alias-a.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n\n[crates.alias-b.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
                canonical.display().to_string().replace('\\', "\\\\"),
                alias.display().to_string().replace('\\', "\\\\"),
            ),
        )?;

        let aliased = run_cargo_rail(&ws.path, &["rail", "split", "run", "--all", "--check"])?;
        assert!(!aliased.status.success());
        let diagnostic = String::from_utf8_lossy(&aliased.stderr);
        assert!(diagnostic.contains("alias-a") && diagnostic.contains("alias-b"));
        assert!(diagnostic.contains("overlapping target repositories"));
        assert!(!canonical.join(".git").exists());

        let distinct = targets.path().join("distinct");
        let shared = targets.path().join("shared-override");
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[crates.alias-a.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n\n[crates.alias-b.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
                canonical.display().to_string().replace('\\', "\\\\"),
                distinct.display().to_string().replace('\\', "\\\\"),
            ),
        )?;
        let shared_override = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "split",
                "run",
                "--all",
                "--remote",
                shared.to_str().unwrap(),
                "--check",
            ],
        )?;
        assert!(!shared_override.status.success());
        let diagnostic = String::from_utf8_lossy(&shared_override.stderr);
        assert!(diagnostic.contains("alias-a") && diagnostic.contains("alias-b"));
        assert!(diagnostic.contains("--remote applies to every --all selection"));
        assert!(!shared.join(".git").exists());

        let missing_parent = targets.path().join("missing-parent");
        let missing_child = missing_parent.join("nested-target");
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[crates.alias-a.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n\n[crates.alias-b.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
                missing_parent.display().to_string().replace('\\', "\\\\"),
                missing_child.display().to_string().replace('\\', "\\\\"),
            ),
        )?;
        let missing_overlap = run_cargo_rail(&ws.path, &["rail", "split", "run", "--all", "--check"])?;
        assert!(!missing_overlap.status.success());
        assert!(String::from_utf8_lossy(&missing_overlap.stderr).contains("overlapping target repositories"));
        assert!(!missing_parent.exists());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_split_rejects_linked_worktrees_before_shared_git_state_changes() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-linked-worktree-targets")?;
        ws.add_crate("linked-a", "0.1.0", &[])?;
        ws.add_crate("linked-b", "0.1.0", &[])?;
        ws.commit("Add linked-worktree target fixtures")?;

        let repository = TempDir::new()?;
        git(repository.path(), &["init", "--initial-branch=main"])?;
        git(repository.path(), &["config", "user.name", "Test User"])?;
        git(repository.path(), &["config", "user.email", "test@example.com"])?;
        git(
            repository.path(),
            &["commit", "--allow-empty", "-m", "Shared target root"],
        )?;
        git(repository.path(), &["branch", "linked-a"])?;
        git(repository.path(), &["branch", "linked-b"])?;
        let worktrees = TempDir::new()?;
        let target_a = worktrees.path().join("target-a");
        let target_b = worktrees.path().join("target-b");
        git(
            repository.path(),
            &["worktree", "add", target_a.to_str().unwrap(), "linked-a"],
        )?;
        git(
            repository.path(),
            &["worktree", "add", target_b.to_str().unwrap(), "linked-b"],
        )?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[crates.linked-a.split]\nremote = \"{}\"\nbranch = \"linked-a\"\nmode = \"single\"\n\n[crates.linked-b.split]\nremote = \"{}\"\nbranch = \"linked-b\"\nmode = \"single\"\n",
                target_a.display().to_string().replace('\\', "\\\\"),
                target_b.display().to_string().replace('\\', "\\\\"),
            ),
        )?;
        let refs_before = git(
            repository.path(),
            &["for-each-ref", "--format=%(refname)=%(objectname)"],
        )?
        .stdout;
        let objects_before = git(repository.path(), &["count-objects", "-v"])?.stdout;

        let output = run_cargo_rail(&ws.path, &["rail", "split", "run", "--all", "--check"])?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("share Git common directory"));
        assert_eq!(
            git(
                repository.path(),
                &["for-each-ref", "--format=%(refname)=%(objectname)"]
            )?
            .stdout,
            refs_before
        );
        assert_eq!(git(repository.path(), &["count-objects", "-v"])?.stdout, objects_before);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_uses_the_preinitialized_configured_branch() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-configured-branch")?;
        ws.add_crate("mylib", "0.1.0", &[])?;
        ws.commit("Add mylib")?;
        let split_dir = initialized_split_target("stable")?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                r#"[crates.mylib.split]
remote = "{}"
branch = "stable"
mode = "single"
"#,
                split_dir.path().display().to_string().replace('\\', "\\\\")
            ),
        )?;

        let output = run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;
        assert!(output.status.success());
        let branch = git(split_dir.path(), &["branch", "--show-current"])?;
        assert_eq!(String::from_utf8_lossy(&branch.stdout).trim(), "stable");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_preserves_git_history() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("mylib", "0.1.0", &[])?;
        ws.commit("Initial mylib")?;

        // Make several commits
        ws.modify_file("mylib", "src/lib.rs", "// Version 1")?;
        ws.commit("Update mylib v1")?;

        ws.modify_file("mylib", "src/lib.rs", "// Version 2")?;
        ws.commit("Update mylib v2")?;

        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.mylib.split]
remote = "{}"
branch = "main"
mode = "single"
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_filters_unrelated_commits() {
    let result: Result<()> = (|| {
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

        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.lib-a.split]
remote = "{}"
branch = "main"
mode = "single"
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_transforms_path_dependencies() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("lib-util", "0.1.0", &[])?;
        ws.add_crate(
            "lib-core",
            "0.2.0",
            &[("lib-util", r#"{ version = "0.1", path = "../lib-util" }"#)],
        )?;
        ws.commit("Add libs with dependency")?;

        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.lib-core.split]
remote = "{}"
branch = "main"
mode = "single"
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_combined_mode_multiple_crates() {
    let result: Result<()> = (|| {
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

        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.combined.split]
remote = "{}"
branch = "main"
mode = "combined"
members = ["lib-core", "service-api"]
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_release_flow_creates_tag_and_changelog() {
    let result: Result<()> = (|| {
        // Split a crate, then run release in the split repo to ensure tagging/changelog works.
        let ws = TestWorkspace::new()?;
        ws.add_crate("lib-release", "0.1.0", &[])?;
        ws.commit("Add lib-release")?;

        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.lib-release.split]
remote = "{}"
branch = "main"
mode = "single"
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
            r#"[release]
tag_prefix = "v"
tag_format = "v{version}"

[release.changelog]
path = "CHANGELOG.md"
"#,
        )?;

        // Tag current version
        git(split_root, &["tag", "-a", "v0.1.0", "-m", "Initial split tag"])?;

        // Make a change to release
        std::fs::write(split_root.join("src/lib.rs"), "// bumped")?;
        std::fs::create_dir_all(split_root.join(".changes"))?;
        std::fs::write(
            split_root.join(".changes/release.md"),
            "---\n\"lib-release\" = \"patch\"\n---\n\nPrepare the split crate release.\n",
        )?;
        git(split_root, &["add", "."])?;
        git(split_root, &["commit", "-m", "feat: prepare release"])?;

        // Run release publish in split repo (skip crates.io)
        let output = run_cargo_rail(
            split_root,
            &["rail", "release", "run", "--all", "--bump", "patch", "--yes"],
        )?;
        assert!(
            output.status.success(),
            "Split release should succeed.\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

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
    })();
    super::helpers::finish_test(result);
}

/// Test split --remote override flag
#[test]
fn test_split_remote_override() {
    let result: Result<()> = (|| {
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
            r#"[crates.override-lib.split]
remote = "/tmp/default-remote"
branch = "main"
mode = "single"
"#,
        )?;

        ws.commit("Add override-lib with config")?;

        let head_before = git(custom_target.path(), &["rev-parse", "HEAD"])?.stdout;
        let index_before = std::fs::read(custom_target.path().join(".git/index"))?;
        let worktree_before = std::fs::read(custom_target.path().join("README.md"))?;
        let fetch_head_before = std::fs::read(custom_target.path().join(".git/FETCH_HEAD")).ok();
        let mut objects_before = String::from_utf8(
            git(
                custom_target.path(),
                &["cat-file", "--batch-all-objects", "--batch-check=%(objectname)"],
            )?
            .stdout,
        )?
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
        objects_before.sort();

        // Refuse an unrelated existing repository before importing source objects.
        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "split",
                "run",
                "override-lib",
                "--yes",
                "--allow-dirty",
                "--remote",
                custom_target.path().to_str().unwrap(),
            ],
        )?;
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("existing split target has no cargo-rail origin evidence"),
            "unexpected diagnostic: {stderr}"
        );
        let mut objects_after = String::from_utf8(
            git(
                custom_target.path(),
                &["cat-file", "--batch-all-objects", "--batch-check=%(objectname)"],
            )?
            .stdout,
        )?
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
        objects_after.sort();
        assert_eq!(objects_after, objects_before, "split must not import source objects");
        assert_eq!(git(custom_target.path(), &["rev-parse", "HEAD"])?.stdout, head_before);
        assert_eq!(std::fs::read(custom_target.path().join(".git/index"))?, index_before);
        assert_eq!(std::fs::read(custom_target.path().join("README.md"))?, worktree_before);
        assert_eq!(
            std::fs::read(custom_target.path().join(".git/FETCH_HEAD")).ok(),
            fetch_head_before
        );
        assert!(String::from_utf8(git(custom_target.path(), &["status", "--porcelain"])?.stdout)?.is_empty());

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test split --json output
#[test]
fn test_split_json_output() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-json")?;
        ws.add_crate("json-lib", "0.1.0", &[])?;
        ws.commit("Add json-lib")?;

        // Configure split using new [crates.<name>.split] format
        let target_dir = tempfile::TempDir::new()?;
        git(target_dir.path(), &["init", "--initial-branch=main"])?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            format!(
                r#"[crates.json-lib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
                target_dir.path().display().to_string().replace('\\', "\\\\")
            ),
        )?;

        // Run split with --check and --json
        let output = run_cargo_rail(&ws.path, &["rail", "split", "run", "json-lib", "--check", "--json"])?;
        assert_eq!(
            output.status.code(),
            Some(1),
            "split run --check --format json should exit 1 when changes are pending"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|_| panic!("split --json should output valid JSON. stdout: {}", stdout));
        assert_eq!(json["schema_version"], serde_json::json!(1));
        assert_eq!(json["command"], serde_json::json!("split"));
        assert_eq!(json["mode"], serde_json::json!("check"));
        assert_eq!(json["result"], serde_json::json!("pending_changes"));
        assert_eq!(json["exit_code"], serde_json::json!(1));

        let applied = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "json-lib", "--yes", "--allow-dirty"],
        )?;
        assert!(applied.status.success());
        let clean = run_cargo_rail(&ws.path, &["rail", "split", "run", "json-lib", "--check", "--json"])?;
        assert_eq!(clean.status.code(), Some(0));
        let clean_json: serde_json::Value = serde_json::from_slice(&clean.stdout)?;
        assert_eq!(clean_json["result"], "clean");
        assert_eq!(clean_json["exit_code"], 0);
        assert_eq!(clean_json["crates"][0]["pending"], false);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_ownership_uses_its_captured_snapshot_and_cargo_graph() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-shared-snapshot")?;
        ws.add_crate("owned-dependency", "0.1.0", &[])?;
        ws.add_crate(
            "owned-root",
            "0.1.0",
            &[("owned-dependency", "{ path = \"../owned-dependency\" }")],
        )?;
        ws.commit("Add owned Cargo graph")?;
        let target = TempDir::new()?;
        git(target.path(), &["init", "--initial-branch=main"])?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                r#"[release.version_groups]
owned = ["owned-root", "owned-dependency"]

[crates.owned-root.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
                target.path().display().to_string().replace('\\', "\\\\")
            ),
        )?;

        let split = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "split",
                "run",
                "owned-root",
                "--check",
                "--json",
                "--allow-dirty",
            ],
        )?;
        assert_eq!(split.status.code(), Some(1));
        let split: serde_json::Value = serde_json::from_slice(&split.stdout)?;
        let ownership = &split["planning"]["targets"][0]["ownership"];
        assert!(
            ownership["snapshot_id"]
                .as_str()
                .is_some_and(|identity| identity.starts_with("sha256-"))
        );
        assert!(
            split["mutation_plan"]["pre_apply"]["metadata_fingerprint"]
                .as_str()
                .is_some_and(|fingerprint| fingerprint.starts_with("snapshot:v1-sha256-"))
        );
        assert_eq!(ownership["members"], serde_json::json!(["owned-root"]));
        assert_eq!(ownership["dependency_closure"], serde_json::json!(["owned-dependency"]));
        assert_eq!(ownership["release_boundaries"][0]["name"], "owned");
        assert_eq!(
            ownership["release_boundaries"][0]["members"],
            serde_json::json!(["owned-dependency", "owned-root"])
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test split init command
#[test]
fn test_split_init_command() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-init-cmd")?;
        ws.add_crate("init-lib", "0.1.0", &[])?;
        ws.commit("Add init-lib")?;

        // Remove existing config to test init
        ws.remove_config()?;

        // Create minimal config without splits
        std::fs::create_dir_all(ws.path.join(".config"))?;
        std::fs::write(ws.path.join(".config/rail.toml"), "")?;

        // Run split init with --dry-run.
        let output = run_cargo_rail(&ws.path, &["rail", "split", "init", "--dry-run"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            output.status.success(),
            "split init --dry-run should succeed. stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("init-lib") || stdout.contains("[crates."),
            "split init should show detected crates. Output:\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that split uses parallel prefetch for performance on repos with many commits.
/// This test creates more than 5 commits to trigger the parallel prefetch path.
#[test]
fn test_split_parallel_prefetch_many_commits() {
    let result: Result<()> = (|| {
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
        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.prefetch-lib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        // Run split - should use parallel prefetch
        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "--verbose",
                "split",
                "run",
                "prefetch-lib",
                "--yes",
                "--allow-dirty",
            ],
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
            stderr.contains("Prefetching exact trees and transform inputs in parallel"),
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
    })();
    super::helpers::finish_test(result);
}

/// Test that split handles "dirty history" gracefully - commits where the crate
/// was temporarily deleted or didn't exist at certain points in history.
///
/// This is a common scenario when:
/// - A crate is temporarily removed and later restored
/// - Files are moved/renamed in a way that deleted the old path temporarily
/// - The crate didn't exist at the start of the filtered history
#[test]
fn test_split_handles_dirty_history() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-dirty-history")?;

        // Create crate and commit
        ws.add_crate("dirty-lib", "0.1.0", &[])?;
        ws.commit("Add dirty-lib")?;

        // Make a change
        ws.modify_file("dirty-lib", "src/lib.rs", "// Version 1")?;
        ws.commit("Update dirty-lib v1")?;

        // DELETE the crate entirely (simulating dirty history)
        std::fs::remove_dir_all(ws.path.join("crates/dirty-lib"))?;
        ws.commit("Remove dirty-lib temporarily")?;

        // Recreate the crate (restoration)
        ws.add_crate("dirty-lib", "0.2.0", &[])?;
        ws.modify_file("dirty-lib", "src/lib.rs", "// Version 2 - restored")?;
        ws.commit("Restore dirty-lib")?;

        // Make another change after restoration
        ws.modify_file("dirty-lib", "src/lib.rs", "// Version 3 - final")?;
        ws.commit("Update dirty-lib v3")?;

        // Configure split
        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.dirty-lib.split]
remote = "{}"
branch = "main"
mode = "single"
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
        assert!(
            log.contains("Remove dirty-lib"),
            "deletion must be preserved. Log:\n{}",
            log
        );
        assert!(log.contains("Restore dirty-lib"), "Should contain restore commit");
        assert!(log.contains("Update dirty-lib v3"), "Should contain v3 update");

        let deletion = git(
            split_dir.path(),
            &[
                "log",
                "--format=%H",
                "--fixed-strings",
                "--grep=Remove dirty-lib temporarily",
            ],
        )?;
        let deletion_sha = String::from_utf8_lossy(&deletion.stdout).trim().to_string();
        let deletion_tree = git(split_dir.path(), &["ls-tree", "-r", "--name-only", &deletion_sha])?;
        assert!(
            deletion_tree.stdout.is_empty(),
            "the historical deletion snapshot must contain no stale crate files"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_split_history_preserves_rename_delete_mode_symlink_and_exact_final_tree() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let ws = TestWorkspace::new_named("split-exact-tree-history")?;
        ws.add_crate("exact-tree", "0.1.0", &[])?;
        let crate_root = ws.path.join("crates/exact-tree");
        std::fs::write(crate_root.join("src/old.rs"), "pub const VALUE: u8 = 1;\n")?;
        std::fs::write(crate_root.join("tool.sh"), "#!/bin/sh\nexit 0\n")?;
        ws.commit("Add historical files")?;

        let tool = crate_root.join("tool.sh");
        let mut permissions = std::fs::metadata(&tool)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&tool, permissions)?;
        std::fs::write(crate_root.join("src/old.rs"), "pub const VALUE: u8 = 2;\n")?;
        ws.commit("Modify file and make tool executable")?;

        std::fs::rename(crate_root.join("src/old.rs"), crate_root.join("src/new.rs"))?;
        symlink("new.rs", crate_root.join("src/link.rs"))?;
        ws.commit("Rename file and add symlink")?;

        std::fs::remove_file(&tool)?;
        ws.commit("Delete executable tool")?;

        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.exact-tree.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "exact-tree", "--yes", "--allow-dirty"],
        )?;
        assert!(
            output.status.success(),
            "split stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let rename_commit = git(
            split_dir.path(),
            &[
                "log",
                "--format=%H",
                "--fixed-strings",
                "--grep=Rename file and add symlink",
            ],
        )?;
        let rename_commit = String::from_utf8_lossy(&rename_commit.stdout).trim().to_string();
        let rename_tree = git(split_dir.path(), &["ls-tree", "-r", &rename_commit])?;
        let rename_tree = String::from_utf8_lossy(&rename_tree.stdout);
        assert!(rename_tree.contains("120000 blob") && rename_tree.contains("src/link.rs"));
        assert!(rename_tree.contains("100755 blob") && rename_tree.contains("tool.sh"));
        assert!(rename_tree.contains("src/new.rs"));
        assert!(!rename_tree.contains("src/old.rs"));

        assert!(!split_dir.path().join("tool.sh").exists());
        assert!(!split_dir.path().join("src/old.rs").exists());
        assert_eq!(
            std::fs::read_link(split_dir.path().join("src/link.rs"))?,
            std::path::PathBuf::from("new.rs")
        );

        let source_tree = git(
            &ws.path,
            &["ls-tree", "-r", "--name-only", "HEAD", "--", "crates/exact-tree"],
        )?;
        let source_owned = String::from_utf8_lossy(&source_tree.stdout)
            .lines()
            .map(|path| path.trim_start_matches("crates/exact-tree/").to_string())
            .collect::<std::collections::BTreeSet<_>>();
        let target_tree = git(split_dir.path(), &["ls-tree", "-r", "--name-only", "HEAD"])?;
        let target_owned = String::from_utf8_lossy(&target_tree.stdout)
            .lines()
            .filter(|path| matches!(*path, "Cargo.toml" | "README.md") || path.starts_with("src/"))
            .map(str::to_string)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            target_owned, source_owned,
            "final owned tree must equal the source subtree"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_preserves_merge_parents_and_commit_identities() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-merge-history")?;
        ws.add_crate("merge-tree", "0.1.0", &[])?;
        ws.commit("Add merge-tree")?;
        git(&ws.path, &["checkout", "-b", "feature"])?;
        ws.modify_file("merge-tree", "src/feature.rs", "pub const FEATURE: bool = true;\n")?;
        ws.commit("Add feature side")?;
        git(&ws.path, &["checkout", "main"])?;
        ws.modify_file("merge-tree", "src/main_side.rs", "pub const MAIN: bool = true;\n")?;
        ws.commit("Add main side")?;
        git(&ws.path, &["merge", "--no-ff", "feature", "-m", "Merge split feature"])?;

        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.merge-tree.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;
        run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "merge-tree", "--yes", "--allow-dirty"],
        )?;

        let target_merge = git(
            split_dir.path(),
            &["log", "--format=%H", "--fixed-strings", "--grep=Merge split feature"],
        )?;
        let target_merge = String::from_utf8_lossy(&target_merge.stdout).trim().to_string();
        let parents = git(split_dir.path(), &["rev-list", "--parents", "-n", "1", &target_merge])?;
        assert_eq!(String::from_utf8_lossy(&parents.stdout).split_whitespace().count(), 3);

        let metadata_format = "%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI";
        let source_metadata = git(
            &ws.path,
            &["show", "-s", &format!("--format={metadata_format}"), "HEAD"],
        )?;
        let target_metadata = git(
            split_dir.path(),
            &["show", "-s", &format!("--format={metadata_format}"), &target_merge],
        )?;
        assert_eq!(source_metadata.stdout, target_metadata.stdout);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

// Safety Rails Tests

/// Test that split apply requires explicit confirmation in non-interactive mode
#[test]
fn test_split_requires_explicit_confirmation_non_interactive() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("confirm-lib", "0.1.0", &[])?;
        ws.commit("Add confirm-lib")?;

        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.confirm-lib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        let output = run_cargo_rail(&ws.path, &["rail", "split", "run", "confirm-lib", "--allow-dirty"])?;
        assert!(
            !output.status.success(),
            "split should fail without --yes/--plan in non-interactive mode"
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

/// Test that split fails on dirty worktree without --allow-dirty
#[test]
fn test_split_dirty_worktree_error() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("safety-lib", "0.1.0", &[])?;
        ws.commit("Add safety-lib")?;

        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.safety-lib.split]
remote = "{}"
branch = "main"
mode = "single"
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_dirty_check_excludes_exact_generated_roots_without_hiding_named_source_paths() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-generated-state-boundary")?;
        ws.add_crate("boundary-lib", "0.1.0", &[])?;

        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.boundary-lib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;
        std::fs::write(ws.path.join(".gitignore"), "ignored-state/\n")?;
        let status = std::process::Command::new(env!("CARGO"))
            .args(["generate-lockfile", "--offline"])
            .current_dir(&ws.path)
            .status()?;
        assert!(status.success(), "fixture lockfile generation failed");
        ws.commit("Configure exact generated-state boundary")?;

        let generated = ws.path.join("target/debug/generated.rlib");
        std::fs::create_dir_all(generated.parent().expect("generated fixture must have a parent"))?;
        std::fs::write(&generated, "Cargo output\n")?;
        let output = run_cargo_rail(&ws.path, &["rail", "split", "run", "boundary-lib", "--yes"])?;
        assert!(
            output.status.success(),
            "resolved Cargo output must not dirty split: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let source = ws.path.join("docs/target/intentional.txt");
        std::fs::create_dir_all(source.parent().expect("source fixture must have a parent"))?;
        std::fs::write(&source, "intentional source\n")?;
        let output = run_cargo_rail(&ws.path, &["rail", "split", "run", "boundary-lib", "--yes"])?;
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("docs/target/intentional.txt"),
            "named source path must remain inside the dirty boundary: {stderr}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that --allow-dirty bypasses the dirty worktree check
#[test]
fn test_split_allow_dirty_bypasses_check() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("allow-dirty-lib", "0.1.0", &[])?;
        ws.commit("Add allow-dirty-lib")?;

        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.allow-dirty-lib.split]
remote = "{}"
branch = "main"
mode = "single"
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_rejects_target_inside_source_repository() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-target-boundary")?;
        ws.add_crate("boundary-lib", "0.1.0", &[])?;
        let source_head = ws.commit("Add boundary-lib")?;
        let target = ws.path.join("split-target");
        let config = format!(
            r#"[crates.boundary-lib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            target.display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "boundary-lib", "--yes", "--allow-dirty"],
        )?;
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("split target") && stderr.contains("overlaps") && stderr.contains("source worktree"),
            "boundary error should identify both roots\nstderr:\n{}",
            stderr
        );
        assert!(!target.exists(), "rejected target must not be created");
        let head = git(&ws.path, &["rev-parse", "HEAD"])?;
        assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), source_head);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_rejects_unknown_member_before_target_mutation() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-crate-boundary")?;
        ws.add_crate("boundary-lib", "0.1.0", &[])?;
        ws.commit("Add boundary-lib")?;
        let target_dir = TempDir::new()?;
        let config = format!(
            r#"[crates.boundary-lib.split]
remote = "{}"
branch = "main"
mode = "single"
members = ["outside"]
"#,
            target_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "boundary-lib", "--yes", "--allow-dirty"],
        )?;
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("crate 'outside' not found in configuration") && !target_dir.path().join(".git").exists(),
            "member resolution should fail before target mutation\nstderr:\n{}",
            stderr
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that split is idempotent - running twice produces the same result (no-op on second run)
#[test]
fn test_split_idempotent_run_twice() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("idempotent-lib", "0.1.0", &[])?;
        ws.commit("Initial commit")?;

        // Make a few commits to have history
        ws.modify_file("idempotent-lib", "src/lib.rs", "// Version 1")?;
        ws.commit("Update v1")?;

        ws.modify_file("idempotent-lib", "src/lib.rs", "// Version 2")?;
        ws.commit("Update v2")?;

        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.idempotent-lib.split]
remote = "{}"
branch = "main"
mode = "single"
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

        // Normal output reports the exact pre-apply pending count.
        let stdout2 = String::from_utf8_lossy(&output2.stdout);
        assert!(
            stdout2.contains("0 pending commit(s)"),
            "Second run should indicate already up-to-date. stdout: {}",
            stdout2
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
    })();
    super::helpers::finish_test(result);
}

/// Test that split is incremental - new commits are added without duplicating existing ones
#[test]
fn test_split_incremental_new_commits() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("incremental-lib", "0.1.0", &[])?;
        ws.commit("Initial commit")?;

        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.incremental-lib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        // First split
        let output1 = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "incremental-lib", "--yes", "--allow-dirty"],
        )?;
        assert!(
            output1.status.success(),
            "Initial split should succeed.\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output1.stdout),
            String::from_utf8_lossy(&output1.stderr)
        );

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
    })();
    super::helpers::finish_test(result);
}

/// Test that split combined mode is idempotent
#[test]
fn test_split_combined_mode_idempotent() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("lib-core", "0.1.0", &[])?;
        ws.add_crate("service-api", "0.1.0", &[("lib-core", r#"{ path = "../lib-core" }"#)])?;
        ws.commit("Initial combined crates")?;

        // Make commits to both crates
        ws.modify_file("lib-core", "src/lib.rs", "// Core v1")?;
        ws.commit("Update lib-core")?;

        ws.modify_file("service-api", "src/lib.rs", "// API v1")?;
        ws.commit("Update service-api")?;

        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.combined.split]
remote = "{}"
branch = "main"
mode = "combined"
members = ["lib-core", "service-api"]
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

        // Normal output reports the exact pre-apply pending count.
        let stdout2 = String::from_utf8_lossy(&output2.stdout);
        assert!(
            stdout2.contains("0 pending commit(s)"),
            "Second run should indicate already up-to-date. stdout: {}",
            stdout2
        );

        // Verify no changes
        let log2 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
        let commit_count2: usize = String::from_utf8_lossy(&log2.stdout).trim().parse()?;
        let head2 = git(split_dir.path(), &["rev-parse", "HEAD"])?;
        let head_sha2 = String::from_utf8_lossy(&head2.stdout).trim().to_string();

        assert_eq!(commit_count1, commit_count2, "Commit count should not change");
        assert_eq!(head_sha1, head_sha2, "HEAD should not change");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that split recovers gracefully from partial/interrupted state
#[test]
fn test_split_partial_state_recovery() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("partial-lib", "0.1.0", &[])?;
        ws.commit("Initial commit")?;

        ws.modify_file("partial-lib", "src/lib.rs", "// V1")?;
        ws.commit("Version 1")?;

        ws.modify_file("partial-lib", "src/lib.rs", "// V2")?;
        ws.commit("Version 2")?;

        ws.modify_file("partial-lib", "src/lib.rs", "// V3")?;
        ws.commit("Version 3")?;

        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.partial-lib.split]
remote = "{}"
branch = "main"
mode = "single"
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
        let ws_notes_ref = git(&ws.path, &["for-each-ref", "--format=%(refname)", &notes_ref])?;
        if !String::from_utf8_lossy(&ws_notes_ref.stdout).trim().is_empty() {
            git(&ws.path, &["update-ref", "-d", &notes_ref])?;
        }
        let split_notes_ref = git(split_dir.path(), &["for-each-ref", "--format=%(refname)", &notes_ref])?;
        if !String::from_utf8_lossy(&split_notes_ref.stdout).trim().is_empty() {
            git(split_dir.path(), &["update-ref", "-d", &notes_ref])?;
        }

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
    })();
    super::helpers::finish_test(result);
}

/// Test that split with auxiliary files is idempotent (final commit doesn't duplicate)
#[test]
fn test_split_auxiliary_files_idempotent() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("aux-lib", "0.1.0", &[])?;
        ws.commit("Initial commit")?;

        // Add some auxiliary files at workspace root
        std::fs::write(ws.path.join("rustfmt.toml"), "max_width = 100")?;
        std::fs::write(ws.path.join(".editorconfig"), "root = true")?;
        git(&ws.path, &["add", "."])?;
        git(&ws.path, &["commit", "-m", "Add config files"])?;

        let split_dir = initialized_split_target("main")?;
        let config = format!(
            r#"[crates.aux-lib.split]
remote = "{}"
branch = "main"
mode = "single"
include = ["rustfmt.toml", ".editorconfig"]
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

        let stdout2 = String::from_utf8_lossy(&output2.stdout);
        assert!(
            stdout2.contains("0 pending commit(s)"),
            "Should be up-to-date. stdout: {}",
            stdout2
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
    })();
    super::helpers::finish_test(result);
}
