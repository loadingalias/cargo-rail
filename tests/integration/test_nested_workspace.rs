//! Integration tests for nested workspace layouts (git root != Cargo workspace root).

use crate::helpers::{NestedWorkspace, git, run_cargo_rail};
use anyhow::{Result, anyhow};
use cargo_rail::git::SystemGit;
use cargo_rail::source::GitWorktreeCapture;
use cargo_rail::workspace::WorkspaceContext;
use serde_json::Value;
use tempfile::TempDir;

#[test]
fn test_plan_from_nested_workspace_strips_workspace_prefix() -> Result<()> {
  let ws = NestedWorkspace::new("rust")?;
  ws.add_crate("lib-a", "0.1.0")?;
  ws.commit("add lib-a")?;

  // Create a stable base ref from git root.
  git(&ws.git_root, &["branch", "origin/main"])?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() -> bool { true }")?;
  ws.commit("change lib-a src")?;

  // Run from cargo workspace root (nested under git root).
  let output = run_cargo_rail(
    &ws.workspace_root,
    &["rail", "plan", "--since", "origin/main", "--format", "json"],
  )?;
  assert!(output.status.success(), "plan should succeed in nested workspace");
  assert!(
    output.stderr.is_empty(),
    "a supported nested workspace must not emit warnings: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  let json: Value = serde_json::from_slice(&output.stdout)?;
  let files = json["files"]
    .as_array()
    .ok_or_else(|| anyhow!("files should be an array"))?;

  assert_eq!(
    files.len(),
    1,
    "expected one changed file: {files:#?}\n{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(
    files[0]["path"],
    Value::String("crates/lib-a/src/lib.rs".to_string()),
    "planner should emit workspace-relative path without nested prefix"
  );
  assert_eq!(json["surfaces"]["build"]["enabled"], Value::Bool(true));
  assert_eq!(json["surfaces"]["test"]["enabled"], Value::Bool(true));

  Ok(())
}

#[test]
fn test_nested_workspace_source_capture_stays_repository_relative() -> Result<()> {
  let ws = NestedWorkspace::new("rust")?;
  ws.add_crate("lib-a", "0.1.0")?;
  ws.commit("add lib-a")?;
  std::fs::write(ws.git_root.join("root-untracked.txt"), "repository input\n")?;
  std::fs::write(
    ws.workspace_root.join("crates/lib-a/workspace-untracked.txt"),
    "workspace input\n",
  )?;

  let git = SystemGit::open(&ws.workspace_root)?;
  let capture = GitWorktreeCapture::capture(&git)?;
  let paths = capture
    .snapshot()
    .tree()
    .entries()
    .iter()
    .map(|entry| entry.path.as_str())
    .collect::<Vec<_>>();

  assert!(paths.contains(&"README.md"), "repository-root source was omitted");
  assert!(
    paths.contains(&"root-untracked.txt"),
    "repository-root untracked source was omitted"
  );
  assert!(
    paths.contains(&"rust/Cargo.toml"),
    "workspace path lost its repository prefix"
  );
  assert!(
    paths.contains(&"rust/crates/lib-a/workspace-untracked.txt"),
    "workspace untracked source lost its repository prefix"
  );
  assert!(
    !paths.contains(&"Cargo.toml"),
    "nested workspace path was incorrectly rebased to the Cargo root"
  );
  Ok(())
}

#[test]
fn test_nested_workspace_excludes_only_its_resolved_generated_roots() -> Result<()> {
  let ws = NestedWorkspace::new("rust")?;
  ws.add_crate("lib-a", "0.1.0")?;
  std::fs::write(ws.git_root.join(".gitignore"), "ignored-state/\n")?;
  ws.commit("add nested generated-state fixture")?;

  for (path, content) in [
    ("rust/target/debug/generated.rlib", "Cargo output\n"),
    ("rust/target/cargo-rail/receipts/generated.json", "cargo-rail output\n"),
    ("target/intentional.txt", "repository source\n"),
  ] {
    let path = ws.git_root.join(path);
    std::fs::create_dir_all(path.parent().expect("fixture path must have a parent"))?;
    std::fs::write(path, content)?;
  }

  let ctx = WorkspaceContext::build_with_source_capture(&ws.workspace_root, true)?;
  let paths = ctx
    .source_capture()
    .expect("source capture should be present")
    .snapshot()
    .tree()
    .entries()
    .iter()
    .map(|entry| entry.path.as_str())
    .collect::<Vec<_>>();
  assert!(paths.contains(&"target/intentional.txt"));
  assert!(!paths.iter().any(|path| path.starts_with("rust/target/")));
  Ok(())
}

#[test]
fn test_nested_workspace_rejects_a_cargo_target_root_that_contains_source() -> Result<()> {
  let ws = NestedWorkspace::new("rust")?;
  ws.add_crate("lib-a", "0.1.0")?;
  std::fs::create_dir_all(ws.workspace_root.join(".cargo"))?;
  std::fs::write(
    ws.workspace_root.join(".cargo/config.toml"),
    "[build]\ntarget-dir = \".\"\n",
  )?;
  ws.commit("configure an unsafe Cargo target root")?;

  let output = run_cargo_rail(
    &ws.workspace_root,
    &["rail", "plan", "--since", "HEAD", "--format", "json"],
  )?;
  assert_eq!(output.status.code(), Some(2));
  assert!(output.stderr.is_empty(), "JSON errors must keep stderr empty");
  let error: Value = serde_json::from_slice(&output.stdout)?;
  assert!(
    error["message"]
      .as_str()
      .is_some_and(|message| message.contains("contains Cargo workspace root")),
    "unexpected error: {error:#?}"
  );
  assert_eq!(
    error["help"],
    "configure Cargo build output in a dedicated directory below or outside the repository"
  );
  Ok(())
}

#[test]
fn test_workspace_outside_git_worktree_is_rejected() -> Result<()> {
  let fixture = TempDir::new_in(std::env::temp_dir())?;
  let workspace_root = fixture.path();
  let repository_root = workspace_root.join("repository");
  let crate_root = repository_root.join("member");
  std::fs::create_dir_all(crate_root.join("src"))?;
  std::fs::write(
    workspace_root.join("Cargo.toml"),
    "[workspace]\nmembers = [\"repository/member\"]\nresolver = \"2\"\n",
  )?;
  std::fs::write(
    crate_root.join("Cargo.toml"),
    "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
  )?;
  std::fs::write(crate_root.join("src/lib.rs"), "pub fn member() {}\n")?;

  git(&repository_root, &["init", "--initial-branch=main"])?;
  git(&repository_root, &["config", "user.name", "Test User"])?;
  git(&repository_root, &["config", "user.email", "test@example.com"])?;
  git(&repository_root, &["add", "."])?;
  git(&repository_root, &["commit", "-m", "add inner repository"])?;

  let output = run_cargo_rail(&crate_root, &["rail", "plan", "--since", "HEAD", "--format", "json"])?;
  assert_eq!(output.status.code(), Some(2));
  assert!(output.stderr.is_empty(), "JSON errors must keep stderr empty");
  let error: Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(error["error"], true);
  assert!(
    error["message"]
      .as_str()
      .is_some_and(|message| message.contains("is outside Git worktree")),
    "unexpected error: {error:#?}"
  );
  assert_eq!(
    error["help"],
    "select a Cargo workspace contained by this repository, or run outside the nested Git worktree"
  );
  Ok(())
}
