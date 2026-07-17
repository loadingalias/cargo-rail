use std::path::Path;
use std::process::{Command, Output};

use anyhow::Result;
use cargo_rail::git::SystemGit;
#[cfg(unix)]
use cargo_rail::source::SourceEntryKind;
use cargo_rail::source::{ChangeLayer, ChangeRelation, SourceChange, SourceChangeKind, SourceSnapshot};
use serde_json::Value;

use crate::helpers::{TestWorkspace, git, run_cargo_rail};

fn assert_actionable_capture_error(output: &Output, message: &str, help: &str) -> Result<()> {
  assert_eq!(output.status.code(), Some(2));
  assert!(output.stderr.is_empty(), "JSON errors must keep stderr empty");
  let error: Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(error["error"], true);
  assert_eq!(error["code"], 2);
  assert!(
    error["message"].as_str().is_some_and(|actual| actual.contains(message)),
    "unexpected error message: {}",
    error["message"]
  );
  assert_eq!(error["help"], help);
  Ok(())
}

fn change<'a>(changes: &'a [SourceChange], path: &str) -> &'a SourceChange {
  changes
    .iter()
    .find(|change| change.path.as_str() == path)
    .unwrap_or_else(|| panic!("missing change {path:?}: {changes:#?}"))
}

#[test]
fn git_worktree_capture_builds_one_final_tree_and_provenance_rich_changes() -> Result<()> {
  let ws = TestWorkspace::new_named("source-worktree-capture")?;
  let crate_root = ws.add_crate("demo", "0.1.0", &[])?;
  for (name, content) in [
    ("committed.txt", "committed-base\n"),
    ("staged.txt", "staged-base\n"),
    ("unstaged.txt", "unstaged-base\n"),
    ("deleted.txt", "delete-me\n"),
    ("old.txt", "rename-me-uniquely\n"),
    ("copy-source.txt", "copy-me-uniquely\n"),
    ("type.txt", "regular-file\n"),
    ("tool.sh", "#!/bin/sh\nexit 0\n"),
  ] {
    std::fs::write(crate_root.join(name), content)?;
  }
  std::fs::write(ws.path.join(".gitignore"), "ignored.txt\n")?;
  let base = ws.commit("Add source capture fixture")?;

  std::fs::write(crate_root.join("committed.txt"), "committed-final\n")?;
  ws.commit("Commit one source layer")?;

  std::fs::write(crate_root.join("staged.txt"), "staged-final\n")?;
  git(&ws.path, &["add", "crates/demo/staged.txt"])?;
  std::fs::write(crate_root.join("unstaged.txt"), "unstaged-final\n")?;
  std::fs::remove_file(crate_root.join("deleted.txt"))?;
  git(&ws.path, &["mv", "crates/demo/old.txt", "crates/demo/new.txt"])?;
  std::fs::copy(crate_root.join("copy-source.txt"), crate_root.join("copied.txt"))?;
  git(&ws.path, &["add", "crates/demo/copied.txt"])?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    std::fs::remove_file(crate_root.join("type.txt"))?;
    symlink("copy-source.txt", crate_root.join("type.txt"))?;
    git(&ws.path, &["add", "crates/demo/type.txt"])?;

    let mut permissions = std::fs::metadata(crate_root.join("tool.sh"))?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(crate_root.join("tool.sh"), permissions)?;
  }
  std::fs::write(crate_root.join("untracked.txt"), "untracked\n")?;
  std::fs::create_dir_all(ws.path.join("docs/target"))?;
  std::fs::write(ws.path.join("docs/target/example.txt"), "intentional source\n")?;
  std::fs::write(ws.path.join("ignored.txt"), "ignored\n")?;

  let git = SystemGit::open(&ws.path)?;
  let (snapshot, changes) = SourceSnapshot::capture_git_worktree(&git, &base)?;
  let paths: Vec<_> = snapshot
    .tree()
    .entries()
    .iter()
    .map(|entry| entry.path.as_str())
    .collect();
  assert!(paths.windows(2).all(|pair| pair[0] < pair[1]), "tree is not sorted");
  assert!(paths.contains(&"crates/demo/new.txt"));
  assert!(paths.contains(&"crates/demo/copied.txt"));
  assert!(paths.contains(&"crates/demo/untracked.txt"));
  assert!(paths.contains(&"docs/target/example.txt"));
  assert!(!paths.contains(&"crates/demo/old.txt"));
  assert!(!paths.contains(&"crates/demo/deleted.txt"));
  assert!(!paths.contains(&"ignored.txt"));

  #[cfg(unix)]
  {
    let executable = snapshot
      .tree()
      .entries()
      .iter()
      .find(|entry| entry.path.as_str() == "crates/demo/tool.sh")
      .expect("missing executable");
    assert!(matches!(
      executable.kind,
      SourceEntryKind::RegularFile { executable: true, .. }
    ));
    let symlink = snapshot
      .tree()
      .entries()
      .iter()
      .find(|entry| entry.path.as_str() == "crates/demo/type.txt")
      .expect("missing symlink");
    assert_eq!(
      symlink.kind,
      SourceEntryKind::Symlink {
        target: "copy-source.txt".to_string()
      }
    );
  }

  let changes = changes.entries();
  assert_eq!(
    change(changes, "crates/demo/committed.txt").kind,
    SourceChangeKind::Modified
  );
  assert!(
    change(changes, "crates/demo/committed.txt")
      .provenance
      .contains(ChangeLayer::Committed)
  );
  assert!(
    change(changes, "crates/demo/staged.txt")
      .provenance
      .contains(ChangeLayer::Staged)
  );
  assert!(
    change(changes, "crates/demo/unstaged.txt")
      .provenance
      .contains(ChangeLayer::Unstaged)
  );
  assert_eq!(
    change(changes, "crates/demo/deleted.txt").kind,
    SourceChangeKind::Deleted
  );
  assert_eq!(
    change(changes, "crates/demo/old.txt").relation,
    Some(ChangeRelation::RenamedTo(cargo_rail::source::RepositoryPath::new(
      Path::new("crates/demo/new.txt")
    )?))
  );
  assert_eq!(
    change(changes, "crates/demo/new.txt").relation,
    Some(ChangeRelation::RenamedFrom(cargo_rail::source::RepositoryPath::new(
      Path::new("crates/demo/old.txt")
    )?))
  );
  assert_eq!(
    change(changes, "crates/demo/copied.txt").relation,
    Some(ChangeRelation::CopiedFrom(cargo_rail::source::RepositoryPath::new(
      Path::new("crates/demo/copy-source.txt")
    )?))
  );
  #[cfg(unix)]
  assert_eq!(
    change(changes, "crates/demo/type.txt").kind,
    SourceChangeKind::TypeChanged
  );
  assert!(
    change(changes, "crates/demo/untracked.txt")
      .provenance
      .contains(ChangeLayer::Untracked)
  );
  assert!(
    change(changes, "docs/target/example.txt")
      .provenance
      .contains(ChangeLayer::Untracked)
  );
  #[cfg(unix)]
  {
    assert_eq!(change(changes, "crates/demo/tool.sh").kind, SourceChangeKind::Modified);
    assert!(
      change(changes, "crates/demo/tool.sh")
        .provenance
        .contains(ChangeLayer::Unstaged)
    );
  }
  assert!(
    changes
      .windows(2)
      .all(|pair| pair[0].path.as_str() < pair[1].path.as_str()),
    "change set is not sorted"
  );
  assert_eq!(
    changes
      .iter()
      .filter(|change| change.path.as_str() == "crates/demo/new.txt")
      .count(),
    1
  );
  Ok(())
}

#[cfg(unix)]
#[test]
fn git_worktree_capture_rejects_symlinked_parent_escape() -> Result<()> {
  use std::os::unix::fs::symlink;

  let ws = TestWorkspace::new_named("source-worktree-parent-symlink")?;
  let crate_root = ws.add_crate("demo", "0.1.0", &[])?;
  let guarded = crate_root.join("guarded");
  std::fs::create_dir_all(&guarded)?;
  std::fs::write(guarded.join("secret.txt"), "inside\n")?;
  ws.commit("Add guarded source")?;

  std::fs::remove_dir_all(&guarded)?;
  let outside = tempfile::tempdir()?;
  std::fs::write(outside.path().join("secret.txt"), "outside\n")?;
  symlink(outside.path(), &guarded)?;

  let git = SystemGit::open(&ws.path)?;
  let error = SourceSnapshot::capture_git_worktree(&git, "HEAD").expect_err("symlinked parent must be rejected");
  assert!(error.to_string().contains("through symbolic-link ancestor"));
  Ok(())
}

#[test]
fn change_set_does_not_double_count_a_recreated_untracked_path() -> Result<()> {
  let ws = TestWorkspace::new_named("source-worktree-recreated-path")?;
  let crate_root = ws.add_crate("demo", "0.1.0", &[])?;
  let source = crate_root.join("recreated.txt");
  std::fs::write(&source, "base\n")?;
  ws.commit("Add recreated-path fixture")?;

  git(&ws.path, &["rm", "--cached", "crates/demo/recreated.txt"])?;
  let git_backend = SystemGit::open(&ws.path)?;
  let (_, unchanged) = SourceSnapshot::capture_git_worktree(&git_backend, "HEAD")?;
  assert!(
    unchanged.entries().is_empty(),
    "an identical final path is not a change"
  );

  std::fs::write(&source, "modified while untracked\n")?;
  let (snapshot, modified) = SourceSnapshot::capture_git_worktree(&git_backend, "HEAD")?;
  assert_eq!(
    snapshot
      .tree()
      .entries()
      .iter()
      .filter(|entry| entry.path.as_str() == "crates/demo/recreated.txt")
      .count(),
    1
  );
  let modified = change(modified.entries(), "crates/demo/recreated.txt");
  assert_eq!(modified.kind, SourceChangeKind::Modified);
  assert!(modified.provenance.contains(ChangeLayer::Staged));
  assert!(modified.provenance.contains(ChangeLayer::Untracked));
  Ok(())
}

#[test]
fn worktree_capture_rejects_assume_unchanged_index_entries() -> Result<()> {
  let ws = TestWorkspace::new_named("source-worktree-assume-unchanged")?;
  let crate_root = ws.add_crate("demo", "0.1.0", &[])?;
  let source = crate_root.join("hidden.txt");
  std::fs::write(&source, "base\n")?;
  ws.commit("Add assume-unchanged fixture")?;

  git(
    &ws.path,
    &["update-index", "--assume-unchanged", "crates/demo/hidden.txt"],
  )?;
  std::fs::write(&source, "hidden worktree change\n")?;

  let git_backend = SystemGit::open(&ws.path)?;
  let error = SourceSnapshot::capture_git_worktree(&git_backend, "HEAD")
    .expect_err("assume-unchanged paths make capture ambiguous");
  assert!(error.to_string().contains("assume-unchanged index entry"));
  Ok(())
}

#[test]
fn plan_rejects_an_unmerged_index_with_actionable_diagnostics() -> Result<()> {
  let ws = TestWorkspace::new_named("source-worktree-unmerged-index")?;
  let crate_root = ws.add_crate("demo", "0.1.0", &[])?;
  let conflict = crate_root.join("conflict.txt");
  std::fs::write(&conflict, "base\n")?;
  ws.commit("Add conflict fixture")?;

  git(&ws.path, &["checkout", "-b", "conflict-side"])?;
  std::fs::write(&conflict, "side\n")?;
  ws.commit("Change conflict fixture on side")?;

  git(&ws.path, &["checkout", "main"])?;
  std::fs::write(&conflict, "main\n")?;
  ws.commit("Change conflict fixture on main")?;

  let merge = Command::new("git")
    .current_dir(&ws.path)
    .args(["merge", "--no-edit", "conflict-side"])
    .output()?;
  assert_eq!(
    merge.status.code(),
    Some(1),
    "fixture merge must conflict: stdout={} stderr={}",
    String::from_utf8_lossy(&merge.stdout),
    String::from_utf8_lossy(&merge.stderr)
  );
  let unmerged = git(&ws.path, &["ls-files", "--unmerged", "--", "crates/demo/conflict.txt"])?;
  assert!(!unmerged.stdout.is_empty(), "fixture did not create an unmerged index");

  let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
  assert_actionable_capture_error(
    &output,
    "cannot capture unmerged index entry 'crates/demo/conflict.txt'",
    "resolve the index conflict and stage the resolution before retrying",
  )
}

#[test]
fn plan_rejects_an_unsupported_filesystem_object_with_actionable_diagnostics() -> Result<()> {
  let ws = TestWorkspace::new_named("source-worktree-unsupported-object")?;
  let crate_root = ws.add_crate("demo", "0.1.0", &[])?;
  let unsupported = crate_root.join("tracked-object");
  std::fs::write(&unsupported, "regular file\n")?;
  ws.commit("Add filesystem-object fixture")?;

  std::fs::remove_file(&unsupported)?;
  std::fs::create_dir(&unsupported)?;

  let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
  assert_actionable_capture_error(
    &output,
    "cannot capture unsupported filesystem object 'crates/demo/tracked-object'",
    "replace it with a regular file or symbolic link before retrying",
  )
}

#[cfg(unix)]
#[test]
fn plan_rejects_a_unix_socket_without_reading_it() -> Result<()> {
  use std::os::unix::net::UnixListener;

  let ws = TestWorkspace::new_named("source-worktree-unsupported-socket")?;
  let crate_root = ws.add_crate("demo", "0.1.0", &[])?;
  let unsupported = crate_root.join("tracked-socket");
  std::fs::write(&unsupported, "regular file\n")?;
  ws.commit("Add socket fixture")?;

  std::fs::remove_file(&unsupported)?;
  let _listener = UnixListener::bind(&unsupported)?;

  let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
  assert_actionable_capture_error(
    &output,
    "cannot capture unsupported filesystem object 'crates/demo/tracked-socket'",
    "replace it with a regular file or symbolic link before retrying",
  )
}
