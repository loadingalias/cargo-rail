//! Integration tests for release + changelog generation
//!
//! Covers:
//! - Tag pattern detection ({crate}-v*)
//! - Compare URLs with GitHub remote
//! - Commit/PR links and breaking markers
//! - per-crate changelog skip and require_changelog_entries flags

use crate::helpers::{TestWorkspace, git, run_cargo_rail};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;

fn write_release_config(ws: &TestWorkspace, extras: &str) -> Result<()> {
  ws.write_release_config(&format!(
    r#"tag_prefix = "v"
tag_format = "{{crate}}-v{{version}}"
source = "both"
require_changelog_entries = false
require_clean = false
semver_check = "off"
{}
"#,
    extras
  ))?;
  Ok(())
}

fn shallow_clone(ws: &TestWorkspace, name: &str) -> Result<(tempfile::TempDir, PathBuf)> {
  let root = tempfile::TempDir::new()?;
  let clone_path = root.path().join(name);
  let output = Command::new("git")
    .args([
      "clone",
      "--depth",
      "1",
      &file_url(&ws.path),
      clone_path.to_str().unwrap(),
    ])
    .output()?;
  assert!(
    output.status.success(),
    "shallow clone failed\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  Ok((root, clone_path))
}

fn file_url(path: &Path) -> String {
  #[cfg(windows)]
  {
    format!("file:///{}", path.display().to_string().replace('\\', "/"))
  }
  #[cfg(not(windows))]
  {
    format!("file://{}", path.display())
  }
}

fn run_release_with_fault(cwd: &Path, args: &[&str], fault: &str) -> Result<std::process::Output> {
  run_release_with_fault_env(cwd, args, "CARGO_RAIL_RELEASE_FAIL_AFTER", fault)
}

fn run_release_with_before_fault(cwd: &Path, args: &[&str], fault: &str) -> Result<std::process::Output> {
  run_release_with_fault_env(cwd, args, "CARGO_RAIL_RELEASE_FAIL_BEFORE", fault)
}

fn run_release_with_fault_env(cwd: &Path, args: &[&str], variable: &str, fault: &str) -> Result<std::process::Output> {
  Ok(
    Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
      .current_dir(cwd)
      .env("GIT_CONFIG_COUNT", "2")
      .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
      .env("GIT_CONFIG_VALUE_0", "false")
      .env("GIT_CONFIG_KEY_1", "tag.gpgsign")
      .env("GIT_CONFIG_VALUE_1", "false")
      .env(variable, fault)
      .args(args)
      .output()?,
  )
}

fn only_release_state(workspace: &Path) -> Result<PathBuf> {
  std::fs::read_dir(workspace.join("target/cargo-rail/releases"))?
    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
    .find(|path| path.extension().is_some_and(|extension| extension == "json"))
    .ok_or_else(|| anyhow::anyhow!("missing release state"))
}

fn push_release_workspace(crate_name: &str) -> Result<(TestWorkspace, tempfile::TempDir)> {
  let ws = TestWorkspace::new_single_crate(crate_name, "0.1.0")?;
  let remote = tempfile::TempDir::new()?;
  git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
  ws.set_remote(remote.path().to_str().unwrap())?;
  git(&ws.path, &["push", "-u", "origin", "main"])?;
  ws.write_release_config(
    r#"tag_format = "v{version}"
source = "both"
require_clean = false
require_release_notes = false
remote_effects = "push"
"#,
  )?;
  Ok((ws, remote))
}

fn install_pre_push_hook(ws: &TestWorkspace, script: &str) -> Result<()> {
  let hook_path = ws.path.join(".git/hooks/pre-push");
  std::fs::write(&hook_path, script)?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = std::fs::metadata(&hook_path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&hook_path, perms)?;
  }
  Ok(())
}

#[test]
fn release_plan_works_on_single_crate_repo() -> Result<()> {
  // Test that release plan works on a split repo (single-crate, non-workspace)
  let ws = TestWorkspace::new_single_crate("private-tool", "0.1.0")?;

  // Add release config (what a split repo would have)
  ws.write_release_config(
    r#"tag_prefix = "v"
tag_format = "v{version}"
require_clean = false
"#,
  )?;

  // Run release plan
  let output = run_cargo_rail(&ws.path, &["rail", "release", "run", "--check", "--bump", "patch"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show the crate in the plan
  assert!(
    stdout.contains("private-tool"),
    "Plan should include private-tool. Output:\n{}",
    stdout
  );
  assert!(
    stdout.contains("0.1.0 → 0.1.1") || stdout.contains("0.1.0") && stdout.contains("0.1.1"),
    "Plan should show version bump. Output:\n{}",
    stdout
  );
  assert!(
    !stdout.contains("0 crate(s)"),
    "Plan should not show 0 crates. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn release_source_defaults_to_reviewed_changes_only() -> Result<()> {
  let ws = TestWorkspace::new_named("release-source-changes-default")?;
  ws.write_release_config(
    r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
semver_check = "off"
"#,
  )?;
  ws.add_crate("lib-a", "1.2.3", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "1.2.3")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn redesigned() {}\n")?;
  ws.commit("feat!: conventional history must not control this release")?;
  std::fs::create_dir_all(ws.path.join(".changes"))?;
  std::fs::write(
    ws.path.join(".changes/reviewed.md"),
    "---\n\"lib-a\" = \"patch\"\n---\n\nReviewed patch intent.\n",
  )?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "lib-a", "--check", "--format", "json"],
  )?;
  assert_eq!(output.status.code(), Some(1));
  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  let plan = &json["release_plan"];
  assert_eq!(plan["source"], serde_json::json!("changes"));
  assert_eq!(plan["crates"][0]["new_version"], serde_json::json!("1.2.4"));
  assert_eq!(plan["crates"][0]["commits"], serde_json::json!([]));
  assert_eq!(plan["crates"][0]["commit_diagnostics"], serde_json::json!([]));
  assert!(
    plan["crates"][0]["changelog_body"]
      .as_str()
      .unwrap()
      .contains("Reviewed patch intent.")
  );
  assert!(
    !String::from_utf8_lossy(&output.stdout).contains("conventional history must not control"),
    "changes mode leaked commit prose: {}",
    String::from_utf8_lossy(&output.stdout)
  );

  Ok(())
}

#[test]
fn release_commit_source_is_explicit_compatibility_mode() -> Result<()> {
  let ws = TestWorkspace::new_named("release-source-commits")?;
  ws.write_release_config(
    r#"source = "commits"
tag_prefix = "v"
tag_format = "{crate}-v{version}"
require_clean = false
semver_check = "off"
"#,
  )?;
  ws.add_crate("lib-a", "1.2.3", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "1.2.3")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn fixed() {}\n")?;
  ws.commit("fix: compatibility bump")?;
  std::fs::create_dir_all(ws.path.join(".changes"))?;
  std::fs::write(
    ws.path.join(".changes/ignored.md"),
    "---\n\"lib-a\" = \"major\"\n---\n\nIgnored by commits mode.\n",
  )?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail", "release", "run", "lib-a", "--bump", "auto", "--check", "--format", "json",
    ],
  )?;
  assert_eq!(output.status.code(), Some(1));
  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  let plan = &json["release_plan"];
  assert_eq!(plan["source"], serde_json::json!("commits"));
  assert_eq!(plan["crates"][0]["new_version"], serde_json::json!("1.2.4"));
  assert_eq!(plan["change_files_to_delete"], serde_json::json!([]));
  assert_eq!(plan["crates"][0]["change_entries"], serde_json::json!([]));

  Ok(())
}

#[test]
fn no_release_change_intent_satisfies_default_coverage_without_a_bump() -> Result<()> {
  let ws = TestWorkspace::new_named("release-no-release-intent")?;
  ws.write_release_config(
    r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
require_clean = false
semver_check = "off"
"#,
  )?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub(crate) fn reorganized() {}\n")?;
  ws.commit("internal reorganization")?;

  let add = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "change",
      "add",
      "lib-a",
      "--bump",
      "none",
      "--message",
      "Internal-only refactor; no released behavior changed.",
    ],
  )?;
  assert!(add.status.success(), "{}", String::from_utf8_lossy(&add.stderr));

  let check = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a"])?;
  assert!(
    check.status.success(),
    "reviewed no-release intent should satisfy coverage\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&check.stdout),
    String::from_utf8_lossy(&check.stderr)
  );

  let plan = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "lib-a", "--bump", "auto", "--check"],
  )?;
  assert!(plan.status.success());
  let stdout = String::from_utf8_lossy(&plan.stdout);
  assert!(stdout.contains("No release-worthy changes detected."), "{}", stdout);
  assert!(
    stdout.contains("no reviewed release intent or dependency updates"),
    "{}",
    stdout
  );
  assert!(!stdout.contains("Internal-only refactor"), "{}", stdout);

  Ok(())
}

#[test]
fn release_retains_unconsumed_no_release_intent_from_a_shared_file() -> Result<()> {
  let ws = TestWorkspace::new_named("release-retain-no-release-intent")?;
  ws.write_release_config(
    r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
require_release_notes = false
semver_check = "off"
"#,
  )?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add crates")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  tag_release(&ws, "lib-b", "0.1.0")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn released_change() {}\n")?;
  ws.modify_file("lib-b", "src/lib.rs", "pub fn internal_change() {}\n")?;
  std::fs::create_dir_all(ws.path.join(".changes"))?;
  let change_path = ws.path.join(".changes/shared.md");
  std::fs::write(
    &change_path,
    "---\n\"lib-a\" = \"patch\"\n\"lib-b\" = \"none\"\n---\n\nShared internal work with one released fix.\n",
  )?;
  ws.commit("Add reviewed shared change")?;

  let preview = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "lib-a", "--check", "--format", "json"],
  )?;
  assert_eq!(preview.status.code(), Some(1));
  let json: serde_json::Value = serde_json::from_slice(&preview.stdout)?;
  assert_eq!(json["release_plan"]["change_files_to_delete"], serde_json::json!([]));
  let retained = &json["release_plan"]["change_files_to_update"][0];
  let retained_path = retained["path"]
    .as_str()
    .expect("retained change-file path should be a JSON string");
  assert_eq!(
    std::fs::canonicalize(retained_path)?,
    std::fs::canonicalize(&change_path)?,
    "release plan should retain the same change file"
  );
  assert_eq!(
    retained["content"],
    serde_json::json!("---\n\"lib-b\" = \"none\"\n---\n\nShared internal work with one released fix.\n")
  );

  let apply = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "lib-a", "--skip-publish", "--yes"],
  )?;
  assert!(
    apply.status.success(),
    "release should retain lib-b coverage\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&apply.stdout),
    String::from_utf8_lossy(&apply.stderr)
  );
  assert_eq!(
    std::fs::read_to_string(&change_path)?,
    "---\n\"lib-b\" = \"none\"\n---\n\nShared internal work with one released fix.\n"
  );

  let coverage = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-b"])?;
  assert!(
    coverage.status.success(),
    "retained no-release intent should continue to cover lib-b\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&coverage.stdout),
    String::from_utf8_lossy(&coverage.stderr)
  );

  Ok(())
}

#[test]
fn reviewed_changes_require_repository_wide_coverage_by_default() -> Result<()> {
  let ws = TestWorkspace::new_named("release-default-change-coverage")?;
  ws.write_release_config(
    r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
require_clean = false
semver_check = "off"
"#,
  )?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
  ws.commit("unstructured commit subject")?;

  let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a"])?;
  let combined = format!(
    "{}\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(!output.status.success(), "{}", combined);
  assert!(combined.contains("missing change files"), "{}", combined);
  assert!(!combined.contains("not a conventional commit"), "{}", combined);

  Ok(())
}

#[test]
fn release_apply_accepts_the_untracked_change_entry_bound_by_its_plan() -> Result<()> {
  let ws = TestWorkspace::new_named("release-apply-bound-change-entry")?;
  ws.write_release_config(
    r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
require_release_notes = false
semver_check = "off"
"#,
  )?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  std::fs::create_dir_all(ws.path.join(".changes"))?;
  let change_path = ws.path.join(".changes/untracked-reviewed.md");
  std::fs::write(
    &change_path,
    "---\n\"lib-a\" = \"patch\"\n---\n\nReviewed patch from an untracked plan input.\n",
  )?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--bump",
      "auto",
      "--skip-publish",
      "--yes",
    ],
  )?;
  assert!(
    output.status.success(),
    "bound dirty input should be accepted\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(!change_path.exists());
  let manifest = std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?;
  assert!(manifest.contains("version = \"0.1.1\""), "{}", manifest);

  Ok(())
}

#[test]
fn release_abort_restores_untracked_reviewed_input_after_a_local_fault() -> Result<()> {
  let ws = TestWorkspace::new_named("release-restore-untracked-intent")?;
  ws.write_release_config(
    r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
require_release_notes = false
semver_check = "off"
"#,
  )?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn reviewed_change() {}\n")?;
  let initial_head = ws.commit("Implement reviewed change")?;

  let content = "---\n\"lib-a\" = \"patch\"\n---\n\nPreserve this reviewed intent across recovery.\n";
  std::fs::create_dir_all(ws.path.join(".changes"))?;
  let change_path = ws.path.join(".changes/recover.md");
  std::fs::write(&change_path, content)?;

  let interrupted = run_release_with_before_fault(
    &ws.path,
    &["rail", "release", "run", "lib-a", "--skip-publish", "--yes"],
    "commit:lib-a",
  )?;
  assert!(!interrupted.status.success());
  assert_eq!(
    std::fs::read_to_string(&change_path)?,
    content,
    "a pre-commit failure must immediately restore untracked reviewed input"
  );

  let state_path = only_release_state(&ws.path)?;
  let aborted = run_cargo_rail(
    &ws.path,
    &["rail", "release", "abort", state_path.to_str().unwrap(), "--yes"],
  )?;
  assert!(
    aborted.status.success(),
    "abort should restore journaled local inputs\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&aborted.stdout),
    String::from_utf8_lossy(&aborted.stderr)
  );
  assert_eq!(std::fs::read_to_string(&change_path)?, content);
  assert_eq!(
    String::from_utf8_lossy(&git(&ws.path, &["rev-parse", "HEAD"])?.stdout).trim(),
    initial_head
  );
  assert!(std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?.contains("version = \"0.1.0\""));

  Ok(())
}

#[test]
fn release_apply_rejects_unrelated_dirt_before_the_first_write() -> Result<()> {
  let ws = TestWorkspace::new_named("release-apply-unrelated-dirt")?;
  ws.write_release_config(
    r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
require_release_notes = false
semver_check = "off"
"#,
  )?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  std::fs::create_dir_all(ws.path.join(".changes"))?;
  let change_path = ws.path.join(".changes/reviewed.md");
  std::fs::write(&change_path, "---\n\"lib-a\" = \"patch\"\n---\n\nReviewed patch.\n")?;
  std::fs::write(ws.path.join("UNRELATED.md"), "unbound operator dirt\n")?;
  let head = git(&ws.path, &["rev-parse", "HEAD"])?.stdout;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--bump",
      "auto",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let combined = format!(
    "{}\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(!output.status.success(), "{}", combined);
  assert!(combined.contains("unplanned worktree changes"), "{}", combined);
  assert!(combined.contains("UNRELATED.md"), "{}", combined);
  assert!(change_path.exists());
  assert_eq!(git(&ws.path, &["rev-parse", "HEAD"])?.stdout, head);

  let manifest = std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?;
  assert!(manifest.contains("version = \"0.1.0\""), "{}", manifest);

  Ok(())
}

#[test]
fn release_plan_auto_infers_bumps_per_crate() -> Result<()> {
  let ws = TestWorkspace::new_named("release-auto-bump")?;
  write_release_config(&ws, "")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "1.2.3", &[])?;
  ws.commit("Add release crates")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  tag_release(&ws, "lib-b", "1.2.3")?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn breaking_api() {}\n")?;
  ws.commit("feat!: redesign lib-a API")?;
  ws.modify_file("lib-b", "src/lib.rs", "pub fn patched() {}\n")?;
  ws.commit("fix: patch lib-b")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "--all", "--bump", "auto", "--check"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(
    !output.status.success(),
    "--check should report pending release changes\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(
    stdout.contains("0.1.0 → 0.2.0"),
    "pre-1.0 breaking change should default to a minor bump\nstdout:\n{}",
    stdout
  );
  assert!(
    stdout.contains("1.2.3 → 1.2.4"),
    "fix commit should infer a patch bump\nstdout:\n{}",
    stdout
  );
  assert!(
    stdout.contains("auto: conventional commits"),
    "plan should explain auto bump source\nstdout:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn release_plan_auto_honors_pre_1_major_policy() -> Result<()> {
  let ws = TestWorkspace::new_named("release-auto-pre1-major")?;
  write_release_config(&ws, "pre_1_breaking_bump = \"major\"")?;

  ws.add_crate("lib-a", "0.3.1", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "0.3.1")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn new_api() {}\n")?;
  ws.commit("feat!: replace public API")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "lib-a", "--bump", "auto", "--check"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("0.3.1 → 1.0.0"),
    "pre_1_breaking_bump = major should graduate to 1.0.0\nstdout:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn release_plan_auto_respects_changelog_path_filters() -> Result<()> {
  let ws = TestWorkspace::new_named("release-auto-path-filters")?;
  write_release_config(
    &ws,
    r#"
[release.changelog.filters]
exclude_paths = ["crates/lib-a/src/**"]
"#,
  )?;

  ws.add_crate("lib-a", "1.2.3", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "1.2.3")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn filtered_out() {}\n")?;
  // Scoped subject on purpose: a crate-name scope must not resurrect a
  // commit whose files were all excluded by path filters.
  ws.commit("feat(lib-a): filtered lib-a feature")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "lib-a", "--bump", "auto", "--check"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(
    stdout.contains("Summary: 0 crate(s)"),
    "excluded paths should not drive auto bump planning\nstdout:\n{}",
    stdout
  );

  Ok(())
}

/// Run cargo-rail with a shimmed `cargo` whose `semver-checks check-release`
/// branch executes `check_release_script`; every other cargo call passes
/// through to the real binary.
#[cfg(unix)]
fn run_with_semver_shim(ws: &TestWorkspace, check_release_script: &str, args: &[&str]) -> Result<std::process::Output> {
  use std::os::unix::fs::PermissionsExt;
  use std::process::Command;

  let real_cargo = Command::new("sh").args(["-c", "command -v cargo"]).output()?;
  let real_cargo = String::from_utf8_lossy(&real_cargo.stdout).trim().to_string();
  let shim_dir = tempfile::TempDir::new()?;
  let shim = shim_dir.path().join("cargo");
  std::fs::write(
    &shim,
    format!(
      r#"#!/bin/sh
if [ "$1" = "semver-checks" ] && [ "$2" = "--version" ]; then
  echo "cargo-semver-checks 0.99.0"
  exit 0
fi
if [ "$1" = "semver-checks" ] && [ "$2" = "check-release" ]; then
  {}
fi
exec "{}" "$@"
"#,
      check_release_script, real_cargo
    ),
  )?;
  let mut perms = std::fs::metadata(&shim)?.permissions();
  perms.set_mode(0o755);
  std::fs::set_permissions(&shim, perms)?;

  let cargo_rail_bin = env!("CARGO_BIN_EXE_cargo-rail");
  let path = format!(
    "{}:{}",
    shim_dir.path().display(),
    std::env::var("PATH").unwrap_or_default()
  );
  let output = Command::new(cargo_rail_bin)
    .current_dir(&ws.path)
    .env("PATH", path)
    .env("GIT_CONFIG_COUNT", "2")
    .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
    .env("GIT_CONFIG_VALUE_0", "false")
    .env("GIT_CONFIG_KEY_1", "tag.gpgsign")
    .env("GIT_CONFIG_VALUE_1", "false")
    .args(args)
    .output()?;
  Ok(output)
}

#[cfg(unix)]
fn run_with_gh_shim(ws: &TestWorkspace, gh_script: &Path, args: &[&str]) -> Result<std::process::Output> {
  run_with_path_prefix(ws, gh_script.parent().unwrap(), args)
}

#[cfg(unix)]
fn run_with_path_prefix(ws: &TestWorkspace, prefix: &Path, args: &[&str]) -> Result<std::process::Output> {
  let cargo_rail_bin = env!("CARGO_BIN_EXE_cargo-rail");
  let path = format!("{}:{}", prefix.display(), std::env::var("PATH").unwrap_or_default());
  Command::new(cargo_rail_bin)
    .current_dir(&ws.path)
    .env("PATH", path)
    .env("GIT_CONFIG_COUNT", "2")
    .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
    .env("GIT_CONFIG_VALUE_0", "false")
    .env("GIT_CONFIG_KEY_1", "tag.gpgsign")
    .env("GIT_CONFIG_VALUE_1", "false")
    .args(args)
    .output()
    .map_err(Into::into)
}

#[cfg(unix)]
fn registry_shadow_cargo_shim(log_path: &Path, published_path: &Path) -> Result<tempfile::TempDir> {
  use std::os::unix::fs::PermissionsExt;

  let real_cargo = Command::new("sh").args(["-c", "command -v cargo"]).output()?;
  let real_cargo = String::from_utf8_lossy(&real_cargo.stdout).trim().to_string();
  let dir = tempfile::TempDir::new()?;
  let path = dir.path().join("cargo");
  std::fs::write(
    &path,
    format!(
      r#"#!/bin/sh
echo "$*" >> "{}"

if [ "$1" = "search" ]; then
  exit 0
fi

if [ "$1" = "info" ]; then
  case " $* " in
    *" --registry crates-io "*)
      if [ -f "{}" ]; then
        exit 0
      fi
      exit 101
      ;;
  esac

  # Recreate Cargo's local-workspace shadowing: an unqualified lookup of the
  # version being released succeeds even though the registry lacks it.
  exit 0
fi

if [ "$1" = "publish" ]; then
  if git show-ref --verify --quiet refs/tags/v0.1.1; then
    echo "release tag existed before publication became observable" >&2
    exit 1
  fi
  case " $* " in
    *" --allow-dirty "*)
      echo "publish must reject dirty package contents" >&2
      exit 1
      ;;
  esac
  case " $* " in
    *" --locked "*) ;;
    *)
      echo "publish must use the committed lockfile" >&2
      exit 1
      ;;
  esac
  case " $* " in
    *" --registry crates-io "*) ;;
    *)
      echo "publish must explicitly target crates.io" >&2
      exit 1
      ;;
  esac
  case " $* " in
    *" -p registry-shadow "*) ;;
    *)
      echo "publish must select exactly one package" >&2
      exit 1
      ;;
  esac
  touch "{}"
  exit 0
fi

exec "{}" "$@"
"#,
      log_path.display(),
      published_path.display(),
      published_path.display(),
      real_cargo
    ),
  )?;
  let mut perms = std::fs::metadata(&path)?.permissions();
  perms.set_mode(0o755);
  std::fs::set_permissions(&path, perms)?;
  Ok(dir)
}

#[cfg(unix)]
#[test]
fn release_publish_ignores_local_workspace_shadow_and_targets_crates_io() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("registry-shadow", "0.1.0")?;
  ws.write_release_config(
    r#"tag_format = "v{version}"
require_changelog_entries = false
require_clean = false
require_release_notes = false
semver_check = "off"
sign_tags = false
publish_delay = 1
"#,
  )?;
  ws.commit("Configure releases")?;
  ws.tag("v0.1.0", "Release registry-shadow 0.1.0")?;

  let shim_state = tempfile::TempDir::new()?;
  let log_path = shim_state.path().join("cargo.log");
  let published_path = shim_state.path().join("published");
  let shim = registry_shadow_cargo_shim(&log_path, &published_path)?;
  let path = format!(
    "{}:{}",
    shim.path().display(),
    std::env::var("PATH").unwrap_or_default()
  );
  let interrupted = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&ws.path)
    .env("PATH", &path)
    .env("GIT_CONFIG_COUNT", "2")
    .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
    .env("GIT_CONFIG_VALUE_0", "false")
    .env("GIT_CONFIG_KEY_1", "tag.gpgsign")
    .env("GIT_CONFIG_VALUE_1", "false")
    .env(
      "CARGO_RAIL_RELEASE_FAIL_AFTER",
      "journal:publish_intent:registry-shadow",
    )
    .args(["rail", "release", "run", "registry-shadow", "--bump", "patch", "--yes"])
    .output()?;
  assert!(!interrupted.status.success());
  assert!(
    !published_path.exists(),
    "journal failure precedes the publication effect"
  );
  let state_path = only_release_state(&ws.path)?;
  let output = run_with_path_prefix(
    &ws,
    shim.path(),
    &["rail", "release", "resume", state_path.to_str().unwrap()],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    output.status.success(),
    "release should publish despite an unqualified local lookup succeeding\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  let log = std::fs::read_to_string(&log_path)?;
  assert!(
    log
      .lines()
      .any(|line| line == "info --registry crates-io registry-shadow@0.1.1"),
    "registry reconciliation must bypass the local workspace package\ncargo calls:\n{}",
    log
  );
  let publishes = log
    .lines()
    .filter(|line| line.starts_with("publish "))
    .collect::<Vec<_>>();
  assert_eq!(
    publishes,
    vec!["publish -p registry-shadow --locked --registry crates-io"],
    "the release must publish exactly once with fail-closed arguments\ncargo calls:\n{}",
    log
  );
  assert!(published_path.exists(), "the registry shim should record a publication");

  let cleaned = run_with_path_prefix(&ws, shim.path(), &["rail", "clean"])?;
  assert!(cleaned.status.success(), "{}", String::from_utf8_lossy(&cleaned.stderr));
  std::fs::remove_file(&published_path)?;
  let status = run_with_path_prefix(&ws, shim.path(), &["rail", "release", "status", "--format", "json"])?;
  let status: serde_json::Value = serde_json::from_slice(&status.stdout)?;
  assert_eq!(
    status["transactions"][0]["recoverability"], "reconstructable",
    "a matching tag must not substitute for registry truth"
  );
  assert_eq!(status["transactions"][0]["ambiguity"], true);

  Ok(())
}

#[test]
fn release_package_excludes_finder_metadata() -> Result<()> {
  let root = Path::new(env!("CARGO_MANIFEST_DIR"));
  let manifest: toml_edit::DocumentMut = std::fs::read_to_string(root.join("Cargo.toml"))?.parse()?;
  let include = manifest["package"]["include"]
    .as_array()
    .ok_or_else(|| anyhow::anyhow!("package.include must be an array"))?;

  assert!(
    include.iter().any(|value| value.as_str() == Some("!**/.DS_Store")),
    "package.include must exclude Finder metadata even when tests are included"
  );
  Ok(())
}

#[cfg(unix)]
fn gh_shim(log_path: &Path) -> Result<(tempfile::TempDir, PathBuf)> {
  use std::os::unix::fs::PermissionsExt;

  let dir = tempfile::TempDir::new()?;
  let path = dir.path().join("gh");
  std::fs::write(
    &path,
    format!(
      r#"#!/bin/sh
echo "$@" >> "{}"
if [ "$1" = "--version" ]; then
  echo "gh version 0.0.0"
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "create" ]; then
  exit 0
fi
echo "unexpected gh args: $@" >&2
exit 1
"#,
      log_path.display()
    ),
  )?;
  let mut perms = std::fs::metadata(&path)?.permissions();
  perms.set_mode(0o755);
  std::fs::set_permissions(&path, perms)?;
  Ok((dir, path))
}

#[cfg(unix)]
fn glab_shim(log_path: &Path) -> Result<(tempfile::TempDir, PathBuf)> {
  glab_shim_with_status(log_path, "success")
}

#[cfg(unix)]
fn glab_shim_with_status(log_path: &Path, pipeline_status: &str) -> Result<(tempfile::TempDir, PathBuf)> {
  use std::os::unix::fs::PermissionsExt;

  let dir = tempfile::TempDir::new()?;
  let path = dir.path().join("glab");
  std::fs::write(
    &path,
    format!(
      r#"#!/bin/sh
echo "$@" >> "{}"
if [ "$1" = "--version" ]; then
  echo "glab version 0.0.0"
  exit 0
fi
if [ "$1" = "release" ] && [ "$2" = "view" ]; then
  exit 1
fi
if [ "$1" = "release" ] && [ "$2" = "create" ]; then
  exit 0
fi
if [ "$1" = "api" ]; then
  case "$2" in
    projects/:id/pipelines\?sha=*)
      if git show-ref --verify --quiet refs/tags/v0.1.1; then
        echo "release tag existed before exact-SHA readiness" >&2
        exit 1
      fi
      echo '[{{"status":"{}"}}]'
      exit 0
      ;;
  esac
fi
echo "unexpected glab args: $@" >&2
exit 1
"#,
      log_path.display(),
      pipeline_status
    ),
  )?;
  let mut perms = std::fs::metadata(&path)?.permissions();
  perms.set_mode(0o755);
  std::fs::set_permissions(&path, perms)?;
  Ok((dir, path))
}

#[cfg(unix)]
fn run_with_minimal_path_without_forge(ws: &TestWorkspace, args: &[&str]) -> Result<std::process::Output> {
  use std::os::unix::fs::symlink;

  let dir = tempfile::TempDir::new()?;
  for binary in ["cargo", "git", "rustc", "rustdoc"] {
    let output = Command::new("sh")
      .args(["-c", &format!("command -v {binary}")])
      .output()?;
    assert!(
      output.status.success(),
      "could not locate required test binary {}",
      binary
    );
    let real = String::from_utf8_lossy(&output.stdout).trim().to_string();
    symlink(real, dir.path().join(binary))?;
  }

  let cargo_rail_bin = env!("CARGO_BIN_EXE_cargo-rail");
  Command::new(cargo_rail_bin)
    .current_dir(&ws.path)
    .env("PATH", dir.path())
    .env_remove("RUSTC_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
    .env("GIT_CONFIG_COUNT", "2")
    .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
    .env("GIT_CONFIG_VALUE_0", "false")
    .env("GIT_CONFIG_KEY_1", "tag.gpgsign")
    .env("GIT_CONFIG_VALUE_1", "false")
    .args(args)
    .output()
    .map_err(Into::into)
}

#[cfg(unix)]
fn semver_shim_workspace(name: &str) -> Result<TestWorkspace> {
  let ws = TestWorkspace::new_named(name)?;
  ws.write_release_config(
    r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
require_changelog_entries = false
require_clean = false
semver_check = "warn"
"#,
  )?;

  ws.add_crate("lib-a", "1.2.3", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "1.2.3")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn doc_only_bump_signal() {}\n")?;
  ws.commit("docs: update public API notes")?;
  Ok(ws)
}

#[cfg(unix)]
#[test]
fn release_plan_blocks_when_semver_checks_exceeds_reviewed_intent() -> Result<()> {
  let ws = semver_shim_workspace("release-auto-semver-checks")?;
  std::fs::create_dir_all(ws.path.join(".changes"))?;
  std::fs::write(
    ws.path.join(".changes/reviewed.md"),
    "---\n\"lib-a\" = \"minor\"\n---\n\nReviewed a non-breaking API change.\n",
  )?;

  let output = run_with_semver_shim(
    &ws,
    r#"echo "Summary semver requires new major version: 1 major check failed" >&2
  exit 1"#,
    &["rail", "release", "run", "lib-a", "--bump", "auto", "--check"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  let combined = format!("{}\n{}", stdout, stderr);
  assert_eq!(output.status.code(), Some(2), "{}", combined);
  assert!(combined.contains("requires a major release"), "{}", combined);
  assert!(combined.contains("revise the reviewed change entry"), "{}", combined);
  assert!(!combined.contains("1.2.3 → 2.0.0"), "{}", combined);

  Ok(())
}

#[cfg(unix)]
#[test]
fn release_plan_accepts_semver_breakage_covered_by_reviewed_major_intent() -> Result<()> {
  let ws = semver_shim_workspace("release-semver-reviewed-major")?;
  std::fs::create_dir_all(ws.path.join(".changes"))?;
  std::fs::write(
    ws.path.join(".changes/reviewed.md"),
    "---\n\"lib-a\" = \"major\"\n---\n\nReviewed breaking API change.\n",
  )?;

  let output = run_with_semver_shim(
    &ws,
    r#"echo "Summary semver requires new major version: 1 major check failed" >&2
  exit 1"#,
    &["rail", "release", "run", "lib-a", "--check"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert_eq!(output.status.code(), Some(1), "{}", stdout);
  assert!(stdout.contains("1.2.3 → 2.0.0"), "{}", stdout);
  assert!(stdout.contains("reviewed change files -> major"), "{}", stdout);

  Ok(())
}

#[cfg(unix)]
#[test]
fn release_plan_auto_ignores_inconclusive_semver_checks() -> Result<()> {
  // A non-zero exit without the breaking-summary marker is an operational
  // failure (first release: no baseline on crates.io) — never an escalation.
  let ws = semver_shim_workspace("release-auto-semver-inconclusive")?;

  let output = run_with_semver_shim(
    &ws,
    r#"echo "error: the crate lib-a has no published versions to use as a baseline" >&2
  exit 1"#,
    &["rail", "release", "run", "lib-a", "--bump", "auto", "--check"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(
    !stdout.contains("2.0.0"),
    "inconclusive semver-checks must not escalate the bump\nstdout:\n{}",
    stdout
  );
  assert!(
    stdout.contains("Skipped:") && stdout.contains("lib-a"),
    "docs-only crate should be skipped with a trace reason\nstdout:\n{}",
    stdout
  );

  Ok(())
}

#[cfg(unix)]
#[test]
fn release_plan_auto_skips_semver_checks_for_unpublishable_crates() -> Result<()> {
  // publish = false crates have no crates.io baseline; the API check must
  // not run for them even when the checker would report breakage.
  let ws = semver_shim_workspace("release-auto-semver-unpublishable")?;
  ws.write_release_config(
    r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
require_changelog_entries = false
require_clean = false
semver_check = "warn"

[crates.lib-a.release]
publish = false
"#,
  )?;
  ws.commit("Disable publish for lib-a")?;

  let output = run_with_semver_shim(
    &ws,
    r#"echo "Summary semver requires new major version: 1 major check failed" >&2
  exit 1"#,
    &["rail", "release", "run", "lib-a", "--bump", "auto", "--check"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(
    !stdout.contains("2.0.0"),
    "unpublishable crates must never be semver-escalated\nstdout:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn release_plan_auto_reports_skipped_crates_with_reason() -> Result<()> {
  let ws = TestWorkspace::new_named("release-auto-skip-trace")?;
  write_release_config(&ws, "")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add crates")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  tag_release(&ws, "lib-b", "0.1.0")?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn only_a_changed() {}\n")?;
  ws.commit("feat: extend lib-a")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "--all", "--bump", "auto", "--check"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(
    stdout.contains("Skipped:"),
    "plan should list skipped crates\nstdout:\n{}",
    stdout
  );
  assert!(
    stdout.contains("lib-b — auto: no release-worthy changes since lib-b-v0.1.0"),
    "skip trace should name the crate and the range\nstdout:\n{}",
    stdout
  );
  assert!(
    stdout.contains("1 skipped"),
    "summary should count skipped crates\nstdout:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn release_plan_auto_noops_when_all_crates_are_skipped() -> Result<()> {
  let ws = TestWorkspace::new_named("release-auto-noop")?;
  write_release_config(&ws, "")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "0.1.0")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "--all", "--bump", "auto", "--check"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(
    output.status.success(),
    "check mode should succeed when there are no planned release mutations\nstdout:\n{}\nstderr:\n{}",
    stdout,
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(
    stdout.contains("No release-worthy changes detected."),
    "no-op check output should explain that nothing will be applied\nstdout:\n{}",
    stdout
  );
  assert!(
    !stdout.contains("Changes detected."),
    "no-op check output must not report pending changes\nstdout:\n{}",
    stdout
  );

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail", "release", "run", "--all", "--bump", "auto", "--check", "--format", "json",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;

  assert!(output.status.success(), "json no-op check should succeed\n{}", stdout);
  assert_eq!(json["result"], serde_json::json!("no_changes"));
  assert_eq!(json["exit_code"], serde_json::json!(0));
  assert_eq!(json["mutation_plan"]["actions"], serde_json::json!([]));

  Ok(())
}

#[test]
fn release_plan_does_not_print_removed_publish_delay() -> Result<()> {
  let ws = TestWorkspace::new_named("release-no-publish-delay")?;
  write_release_config(&ws, "publish_delay = 37")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add release crates")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "--all", "--bump", "patch", "--check"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert_eq!(
    output.status.code(),
    Some(1),
    "release preview should report pending changes\nstdout:\n{}\nstderr:\n{}",
    stdout,
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(
    !stdout.contains("Publish delay"),
    "inert publish_delay must not appear in release output\nstdout:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn release_plan_projects_exact_sha_checks_publication_and_tags_last() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("release-plan-order", "0.1.0")?;
  ws.write_release_config(
    r#"source = "both"
tag_format = "v{version}"
require_clean = false
require_release_notes = false
remote_effects = "gitlab"
"#,
  )?;
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail", "release", "run", "--all", "--bump", "patch", "--check", "--format", "json",
    ],
  )?;
  assert_eq!(output.status.code(), Some(1));
  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  let codes = json["mutation_plan"]["actions"]
    .as_array()
    .unwrap()
    .iter()
    .filter_map(|action| action["code"].as_str())
    .collect::<Vec<_>>();
  let position = |code: &str| codes.iter().position(|candidate| *candidate == code).unwrap();
  assert!(position("COMMIT_RELEASE") < position("PUSH_RELEASE_COMMIT"));
  assert!(position("PUSH_RELEASE_COMMIT") < position("AWAIT_EXACT_SHA_CHECKS"));
  assert!(position("AWAIT_EXACT_SHA_CHECKS") < position("PUBLISH_CRATE"));
  assert!(position("PUBLISH_CRATE") < position("CREATE_TAG"));
  assert!(position("CREATE_TAG") < position("PUSH_RELEASE_TAGS"));
  assert!(position("PUSH_RELEASE_TAGS") < position("CREATE_FORGE_RELEASE"));
  Ok(())
}

#[test]
fn release_plan_auto_rejects_shallow_clone_but_explicit_bump_works() -> Result<()> {
  let ws = TestWorkspace::new_named("release-auto-shallow-guard")?;
  write_release_config(&ws, "")?;

  ws.add_crate("lib-a", "1.2.3", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "1.2.3")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
  ws.commit("fix: change lib-a")?;

  let (_root, clone_path) = shallow_clone(&ws, "shallow")?;

  let output = run_cargo_rail(
    &clone_path,
    &["rail", "release", "run", "lib-a", "--bump", "auto", "--check"],
  )?;
  let combined = format!(
    "{}\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(output.status.code(), Some(2), "auto bump should fail\n{}", combined);
  assert!(
    combined.contains("--bump auto cannot run in a shallow clone") && combined.contains("git fetch --unshallow --tags"),
    "output:\n{}",
    combined
  );

  let output = run_cargo_rail(
    &clone_path,
    &["rail", "release", "run", "lib-a", "--bump", "patch", "--check"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  let combined = format!("{}\n{}", stdout, stderr);
  assert_eq!(
    output.status.code(),
    Some(1),
    "explicit bump should still produce a normal check plan\n{}",
    combined
  );
  assert!(stdout.contains("1.2.3 → 1.2.4"), "stdout:\n{}", stdout);
  assert!(
    !combined.contains("cannot run in a shallow clone"),
    "output:\n{}",
    combined
  );

  Ok(())
}

#[test]
fn release_check_reports_shallow_clone_in_failure_taxonomy() -> Result<()> {
  let ws = TestWorkspace::new_named("release-check-shallow-guard")?;
  write_release_config(&ws, "")?;

  ws.add_crate("lib-a", "1.2.3", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "1.2.3")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
  ws.commit("fix: change lib-a")?;

  let (_root, clone_path) = shallow_clone(&ws, "shallow")?;
  let output = run_cargo_rail(&clone_path, &["rail", "release", "check", "lib-a"])?;
  let combined = format!(
    "{}\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );

  assert_eq!(output.status.code(), Some(2), "release check should fail\n{}", combined);
  assert!(
    combined.contains("release history")
      && combined.contains("shallow clone")
      && combined.contains("git fetch --unshallow --tags"),
    "output:\n{}",
    combined
  );

  Ok(())
}

#[test]
fn release_plan_auto_names_no_previous_tag_full_history() -> Result<()> {
  let ws = TestWorkspace::new_named("release-auto-no-previous-tag")?;
  write_release_config(&ws, "")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "lib-a", "--bump", "auto", "--check"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("no previous tag: full history"),
    "skip reason should name first-release history range\nstdout:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn version_group_propagates_max_auto_bump_and_surfaces_in_json() -> Result<()> {
  let ws = TestWorkspace::new_named("release-version-group-max")?;
  write_release_config(
    &ws,
    r#"
[release.version_groups]
core = ["lib-a", "lib-b", "lib-c"]
"#,
  )?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.add_crate("lib-c", "0.1.0", &[])?;
  ws.commit("Add grouped crates")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  tag_release(&ws, "lib-b", "0.1.0")?;
  tag_release(&ws, "lib-c", "0.1.0")?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn patch_signal() {}\n")?;
  ws.commit("fix: patch lib-a")?;
  ws.modify_file("lib-b", "src/lib.rs", "pub fn minor_signal() {}\n")?;
  ws.commit("feat: extend lib-b")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "--all", "--bump", "auto", "--check"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert_eq!(
    output.status.code(),
    Some(1),
    "plan should have pending changes\n{}",
    stdout
  );
  assert_eq!(
    stdout.matches("0.1.0 → 0.2.0").count(),
    3,
    "all group members should receive the max minor bump\n{}",
    stdout
  );
  assert!(
    stdout.contains("lib-c") && stdout.contains("version group core -> minor"),
    "group-only member should be planned with a group reason\n{}",
    stdout
  );

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail", "release", "run", "--all", "--bump", "auto", "--check", "--format", "json",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;
  assert_eq!(json["release_plan"]["plan_contract_version"], 4);
  assert!(
    json["release_plan"]["snapshot_id"]
      .as_str()
      .is_some_and(|snapshot| snapshot.starts_with("v1-sha256-"))
  );
  let crates = json["release_plan"]["crates"].as_array().expect("crates array");
  for crate_name in ["lib-a", "lib-b", "lib-c"] {
    let crate_plan = crates
      .iter()
      .find(|entry| entry["name"] == crate_name)
      .unwrap_or_else(|| panic!("missing {}", crate_name));
    assert_eq!(crate_plan["version_group"], "core");
  }

  Ok(())
}

#[test]
fn version_group_partial_selection_rejects_or_expands_by_policy() -> Result<()> {
  let ws = TestWorkspace::new_named("release-version-group-partial")?;
  write_release_config(
    &ws,
    r#"
[release.version_groups]
core = ["lib-a", "lib-b"]
"#,
  )?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add grouped crates")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  tag_release(&ws, "lib-b", "0.1.0")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn minor_signal() {}\n")?;
  ws.commit("feat: extend lib-a")?;

  let rejected = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "lib-a", "--bump", "auto", "--check"],
  )?;
  let combined = format!(
    "{}\n{}",
    String::from_utf8_lossy(&rejected.stdout),
    String::from_utf8_lossy(&rejected.stderr)
  );
  assert_eq!(
    rejected.status.code(),
    Some(2),
    "partial group release should fail\n{}",
    combined
  );
  assert!(
    combined.contains("version group 'core'") && combined.contains("lib-b"),
    "output:\n{}",
    combined
  );

  let expanded = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--bump",
      "auto",
      "--check",
      "--include-dependents",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&expanded.stdout);
  assert_eq!(
    expanded.status.code(),
    Some(1),
    "expanded plan should succeed\n{}",
    stdout
  );
  assert!(
    stdout.contains("lib-a") && stdout.contains("lib-b") && stdout.contains("version group core -> minor"),
    "expanded plan should include the whole group\n{}",
    stdout
  );

  Ok(())
}

#[cfg(unix)]
#[test]
fn release_pr_mode_round_trips_to_finalize_on_merge_commit() -> Result<()> {
  let ws = TestWorkspace::new_named("release-pr-mode")?;
  write_release_config(&ws, "require_release_notes = false")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "0.1.0")?;

  let remote_root = tempfile::TempDir::new()?;
  let remote = remote_root.path().join("origin.git");
  let output = Command::new("git")
    .args(["init", "--bare", remote.to_str().unwrap()])
    .output()?;
  assert!(output.status.success(), "bare remote init failed");
  ws.set_remote(remote.to_str().unwrap())?;
  git(&ws.path, &["push", "-u", "origin", "main"])?;
  install_pre_push_hook(
    &ws,
    r#"#!/bin/sh
context_file="$(dirname "$0")/../release-pr-hook-context"
printf '%s:%s\n' "${CARGO_RAIL_RELEASE_PUSH:-}" "${CARGO_RAIL_OPERATION:-}" >> "$context_file"
if [ "${CARGO_RAIL_RELEASE_PUSH:-}" != "1" ] || [ "${CARGO_RAIL_OPERATION:-}" != "release" ]; then
  echo "release PR push did not provide cargo-rail hook context" >&2
  exit 1
fi
"#,
  )?;

  run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "change",
      "add",
      "lib-a",
      "--bump",
      "minor",
      "--message",
      "Added release PR mode.",
    ],
  )?;
  ws.commit("Add release intent")?;

  let gh_log_dir = tempfile::TempDir::new()?;
  let gh_log = gh_log_dir.path().join("gh.log");
  let (_gh_dir, gh_path) = gh_shim(&gh_log)?;
  let output = run_with_gh_shim(
    &ws,
    &gh_path,
    &["rail", "release", "run", "lib-a", "--bump", "auto", "--pr", "--yes"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    output.status.success(),
    "release PR mode should succeed\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  let branch = String::from_utf8_lossy(&git(&ws.path, &["branch", "--show-current"])?.stdout)
    .trim()
    .to_string();
  assert!(branch.starts_with("rail/release-"), "branch: {}", branch);
  assert!(
    String::from_utf8_lossy(&git(&ws.path, &["tag", "--list", "lib-a-v0.2.0"])?.stdout)
      .trim()
      .is_empty(),
    "PR mode must not create release tags"
  );
  assert!(!ws.path.join(".changes").exists() || std::fs::read_dir(ws.path.join(".changes"))?.next().is_none());
  assert!(std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?.contains("version = \"0.2.0\""));
  assert!(std::fs::read_to_string(&gh_log)?.contains("pr create"));
  assert_eq!(
    std::fs::read_to_string(ws.path.join(".git/release-pr-hook-context"))?,
    "1:release\n",
    "the cargo-rail-owned release PR push must provide the standard hook context"
  );
  let prepared_message = String::from_utf8_lossy(&git(&ws.path, &["log", "-1", "--format=%B"])?.stdout).to_string();
  let transaction = prepared_message
    .lines()
    .find_map(|line| line.strip_prefix("Rail-Release: "))
    .unwrap()
    .to_string();

  git(&ws.path, &["checkout", "main"])?;
  git(&ws.path, &["merge", "--no-ff", &branch, "-m", "Merge release PR"])?;
  let merge_sha = String::from_utf8_lossy(&git(&ws.path, &["rev-parse", "HEAD"])?.stdout)
    .trim()
    .to_string();

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "finalize", "lib-a", "--skip-publish", "--yes"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    output.status.success(),
    "finalize should succeed\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  let tag_target = String::from_utf8_lossy(&git(&ws.path, &["rev-list", "-n", "1", "v0.2.0"])?.stdout)
    .trim()
    .to_string();
  let finalize_sha = String::from_utf8_lossy(&git(&ws.path, &["rev-parse", "HEAD"])?.stdout)
    .trim()
    .to_string();
  assert_eq!(
    tag_target, finalize_sha,
    "finalize should tag its exact transaction commit"
  );
  assert_eq!(
    String::from_utf8_lossy(&git(&ws.path, &["rev-parse", "HEAD^"])?.stdout).trim(),
    merge_sha
  );
  let finalize_message = String::from_utf8_lossy(&git(&ws.path, &["log", "-1", "--format=%B"])?.stdout).to_string();
  assert!(finalize_message.contains(&format!("Rail-Release: {}", transaction)));
  assert!(finalize_message.contains("Rail-Release-Mode: finalize"));

  Ok(())
}

#[test]
fn release_finalize_requires_explicit_target_or_all() -> Result<()> {
  let ws = TestWorkspace::new_named("release-finalize-target-required")?;
  write_release_config(&ws, "require_release_notes = false")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  let output = run_cargo_rail(&ws.path, &["rail", "release", "finalize", "--skip-publish", "--yes"])?;
  let combined = format!(
    "{}\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(output.status.code(), Some(2), "finalize should fail\n{}", combined);
  assert!(
    combined.contains("must specify crate name(s) or --all"),
    "output:\n{}",
    combined
  );

  Ok(())
}

#[test]
fn release_finalize_refuses_without_merged_release_notes() -> Result<()> {
  let ws = TestWorkspace::new_named("release-finalize-refuses-unplanned")?;
  write_release_config(&ws, "")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "finalize", "lib-a", "--skip-publish", "--yes"],
  )?;
  let combined = format!(
    "{}\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(output.status.code(), Some(2), "finalize should fail\n{}", combined);
  assert!(
    combined.contains("release finalize expected lib-a v0.1.0"),
    "output:\n{}",
    combined
  );

  Ok(())
}

#[test]
fn release_rejects_partial_change_file_consumption() -> Result<()> {
  let ws = TestWorkspace::new_named("release-partial-change-file")?;
  write_release_config(&ws, "")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add crates")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  tag_release(&ws, "lib-b", "0.1.0")?;

  std::fs::create_dir_all(ws.path.join(".changes"))?;
  std::fs::write(
    ws.path.join(".changes/shared-change.md"),
    "---\n\"lib-a\" = \"minor\"\n\"lib-b\" = \"patch\"\n---\n\nShared behavior change.\n",
  )?;
  ws.commit("Add change file naming both crates")?;

  // Releasing only lib-a would consume the file and silently destroy
  // lib-b's pending intent — the plan must refuse.
  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "lib-a", "--bump", "auto", "--check"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  let combined = format!("{}\n{}", stdout, stderr);

  assert_eq!(
    output.status.code(),
    Some(2),
    "partial change-file consumption must be an error\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(
    combined.contains("shared-change.md") && combined.contains("lib-b"),
    "error should name the file and the missing crate\noutput:\n{}",
    combined
  );

  // Releasing both crates together consumes the file cleanly.
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "auto",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    output.status.success(),
    "full release should consume the change file\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(
    !ws.path.join(".changes/shared-change.md").exists(),
    "change file should be consumed by the release"
  );

  Ok(())
}

#[test]
fn change_add_and_status_support_json_output() -> Result<()> {
  let ws = TestWorkspace::new_named("change-json-output")?;
  write_release_config(&ws, "")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "change",
      "add",
      "lib-a",
      "--bump",
      "minor",
      "--message",
      "Added a user-facing thing.",
      "--format",
      "json",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(output.status.success(), "change add should succeed\n{}", stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;
  assert_eq!(json["command"], "change");
  assert_eq!(json["mode"], "add");
  assert_eq!(json["crates"][0], "lib-a");
  assert_eq!(json["bump"], "minor");
  let created = json["path"].as_str().expect("path in payload");
  let normalized_created = created.replace('\\', "/");
  assert!(normalized_created.contains(".changes/"));
  assert!(
    created.ends_with(".md") && !created.contains("2026"),
    "created change file should use deterministic slug-hash naming: {}",
    created
  );

  let output = run_cargo_rail(&ws.path, &["rail", "change", "status", "--format", "json"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(output.status.success(), "change status should succeed\n{}", stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)?;
  assert_eq!(json["command"], "change");
  assert_eq!(json["count"], 1);
  assert_eq!(json["crates"][0]["crate_name"], "lib-a");
  assert_eq!(json["crates"][0]["bump"], "minor");
  assert_eq!(json["files"][0]["intents"][0]["crate"], "lib-a");
  assert_eq!(json["files"][0]["intents"][0]["bump"], "minor");

  Ok(())
}

#[test]
fn change_status_names_only_is_empty_without_pending_files() -> Result<()> {
  let ws = TestWorkspace::new_named("change-names-only-empty")?;
  write_release_config(&ws, "")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  let output = run_cargo_rail(&ws.path, &["rail", "change", "status", "--format", "names-only"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(output.status.success(), "change status should succeed\n{}", stdout);
  assert_eq!(stdout, "", "names-only should be empty when no change files exist");

  Ok(())
}

#[test]
fn change_status_names_only_lists_pending_change_paths() -> Result<()> {
  let ws = TestWorkspace::new_named("change-names-only-pending")?;
  write_release_config(&ws, "")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "change",
      "add",
      "lib-a",
      "--bump",
      "minor",
      "--message",
      "Added names-only change status.",
    ],
  )?;
  assert!(
    output.status.success(),
    "change add should succeed\n{}",
    String::from_utf8_lossy(&output.stdout)
  );

  let output = run_cargo_rail(&ws.path, &["rail", "change", "status", "--format", "names-only"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(output.status.success(), "change status should succeed\n{}", stdout);
  let lines: Vec<_> = stdout.lines().collect();
  assert_eq!(lines.len(), 1, "one pending file should be listed\n{}", stdout);
  assert!(
    lines[0].starts_with(".changes/"),
    "path should be workspace-relative: {}",
    lines[0]
  );
  assert!(
    lines[0].ends_with(".md"),
    "path should name a markdown change file: {}",
    lines[0]
  );
  assert!(
    !stdout.contains("no pending change files"),
    "names-only should not include human status text"
  );

  Ok(())
}

#[test]
fn change_check_required_fails_when_changed_crate_lacks_change_file() -> Result<()> {
  let ws = TestWorkspace::new_named("change-check-missing")?;
  write_release_config(&ws, "")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  git(&ws.path, &["branch", "origin/main"])?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() -> bool { true }")?;
  ws.commit("Change lib-a source")?;

  let output = run_cargo_rail(&ws.path, &["rail", "change", "check", "--merge-base", "--required"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert_eq!(
    output.status.code(),
    Some(1),
    "missing change file should fail as a check result\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(stdout.contains("missing change files"), "stdout:\n{}", stdout);
  assert!(stdout.contains("lib-a"), "stdout:\n{}", stdout);

  Ok(())
}

#[test]
fn change_check_required_passes_when_changed_crate_has_change_file() -> Result<()> {
  let ws = TestWorkspace::new_named("change-check-covered")?;
  write_release_config(&ws, "")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  git(&ws.path, &["branch", "origin/main"])?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() -> bool { true }")?;
  ws.commit("Change lib-a source")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "change",
      "add",
      "lib-a",
      "--bump",
      "patch",
      "--message",
      "Documented the source change.",
    ],
  )?;
  assert!(
    output.status.success(),
    "change add should succeed\n{}",
    String::from_utf8_lossy(&output.stdout)
  );

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "change", "check", "--since", "origin/main", "--required"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(output.status.success(), "change check should pass\n{}", stdout);
  assert!(stdout.contains("change files: ok"), "stdout:\n{}", stdout);

  Ok(())
}

#[test]
fn change_add_uses_stable_slug_hash_names_and_rejects_duplicate_intent() -> Result<()> {
  let ws = TestWorkspace::new_named("change-stable-filenames")?;
  write_release_config(&ws, "")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  let args = [
    "rail",
    "change",
    "add",
    "lib-a",
    "--bump",
    "minor",
    "--message",
    "Added deterministic filenames for reviewed release intent.",
  ];
  let output = run_cargo_rail(&ws.path, &args)?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(output.status.success(), "change add should succeed\n{}", stdout);
  let first_path = std::path::PathBuf::from(stdout.trim());
  let first_name = first_path.file_name().and_then(|name| name.to_str()).unwrap();
  assert!(
    first_name.starts_with("added-deterministic-filenames-") && first_name.ends_with(".md"),
    "filename should be slug-hash, got {}",
    first_name
  );
  let slug = first_name
    .trim_end_matches(".md")
    .rsplit_once('-')
    .map(|(slug, _)| slug)
    .unwrap();
  assert!(slug.len() <= 32, "slug should be capped at 32 chars: {}", first_name);
  assert!(
    first_name
      .trim_end_matches(".md")
      .rsplit_once('-')
      .is_some_and(|(_, hash)| hash.len() == 4 && hash.chars().all(|c| c.is_ascii_hexdigit())),
    "filename should end in a 4-hex hash: {}",
    first_name
  );

  let duplicate = run_cargo_rail(&ws.path, &args)?;
  let combined = format!(
    "{}\n{}",
    String::from_utf8_lossy(&duplicate.stdout),
    String::from_utf8_lossy(&duplicate.stderr)
  );
  assert_eq!(
    duplicate.status.code(),
    Some(2),
    "duplicate intent should fail\n{}",
    combined
  );
  assert!(combined.contains("change file already exists"), "output:\n{}", combined);

  std::fs::remove_file(&first_path)?;
  let output = run_cargo_rail(&ws.path, &args)?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let second_path = std::path::PathBuf::from(stdout.trim());
  assert_eq!(
    second_path.file_name(),
    first_path.file_name(),
    "same content should produce the same filename"
  );

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "change",
      "add",
      "lib-a",
      "--bump",
      "patch",
      "--message",
      "Patched another thing.",
      "--name",
      "custom-name",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let custom_path = std::path::PathBuf::from(stdout.trim());
  assert!(
    custom_path
      .file_name()
      .and_then(|name| name.to_str())
      .is_some_and(|name| name.starts_with("custom-name-")),
    "custom --name should override slug: {}",
    custom_path.display()
  );

  Ok(())
}

#[test]
fn legacy_change_directory_guard_reports_git_mv_hint() -> Result<()> {
  let ws = TestWorkspace::new_named("change-legacy-dir-guard")?;
  write_release_config(&ws, "")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  std::fs::create_dir_all(ws.path.join(".rail/changes"))?;
  std::fs::write(
    ws.path.join(".rail/changes/old.md"),
    "---\n\"lib-a\" = \"patch\"\n---\n\nOld pending change.\n",
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "change", "status"])?;
  let combined = format!(
    "{}\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(output.status.code(), Some(2), "legacy guard should fail\n{}", combined);
  assert!(
    combined.contains("move files to .changes/ (git mv .rail/changes .changes)"),
    "output:\n{}",
    combined
  );

  Ok(())
}

#[test]
fn change_add_rejects_change_dir_that_escapes_workspace() -> Result<()> {
  let ws = TestWorkspace::new_named("change-dir-escape")?;
  write_release_config(&ws, "change_dir = \"../outside\"")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "change",
      "add",
      "lib-a",
      "--bump",
      "patch",
      "--message",
      "Should not write outside the workspace.",
    ],
  )?;
  let combined = format!(
    "{}\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(output.status.code(), Some(2), "change add should fail\n{}", combined);
  assert!(
    combined.contains("invalid release.change_dir")
      && combined.contains("change_dir must be a workspace-relative path"),
    "output:\n{}",
    combined
  );
  assert!(!ws.path.parent().unwrap().join("outside").exists());

  Ok(())
}

#[test]
fn change_dir_override_round_trips_through_release_consumption() -> Result<()> {
  let ws = TestWorkspace::new_named("change-dir-override")?;
  write_release_config(&ws, "require_release_notes = false\nchange_dir = \"changes\"")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "0.1.0")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "change",
      "add",
      "lib-a",
      "--bump",
      "minor",
      "--message",
      "Added configurable change directory.",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(output.status.success(), "change add should succeed\n{}", stdout);
  let change_path = std::path::PathBuf::from(stdout.trim());
  assert_eq!(
    change_path
      .parent()
      .and_then(|path| path.file_name())
      .and_then(|name| name.to_str()),
    Some("changes"),
    "path: {}",
    change_path.display()
  );
  assert!(change_path.exists(), "path: {}", change_path.display());

  let status = run_cargo_rail(&ws.path, &["rail", "change", "status"])?;
  let status_stdout = String::from_utf8_lossy(&status.stdout);
  assert!(
    status_stdout.contains("lib-a: minor"),
    "status should read configured change_dir\n{}",
    status_stdout
  );

  let plan = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "lib-a", "--bump", "auto", "--check"],
  )?;
  let plan_stdout = String::from_utf8_lossy(&plan.stdout);
  assert!(
    plan_stdout.contains("0.1.0 → 0.2.0"),
    "plan should read configured change_dir\n{}",
    plan_stdout
  );

  let release = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--bump",
      "auto",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&release.stdout);
  let stderr = String::from_utf8_lossy(&release.stderr);
  assert!(
    release.status.success(),
    "release should consume change file from configured dir\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(
    !change_path.exists(),
    "release should consume {}",
    change_path.display()
  );

  Ok(())
}

#[test]
fn change_status_reports_max_bump_per_crate() -> Result<()> {
  let ws = TestWorkspace::new_named("change-status-max-bump")?;
  write_release_config(&ws, "")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "change",
      "add",
      "lib-a",
      "--bump",
      "patch",
      "--message",
      "Fixed first thing.",
    ],
  )?;
  run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "change",
      "add",
      "lib-a",
      "--bump",
      "minor",
      "--message",
      "Added second thing.",
    ],
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "change", "status"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("resulting bumps:") && stdout.contains("lib-a: minor (2 files)"),
    "status should report max bump across files\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn change_add_without_message_errors_in_non_tty() -> Result<()> {
  let ws = TestWorkspace::new_named("change-non-tty-message")?;
  write_release_config(&ws, "")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  let output = run_cargo_rail(&ws.path, &["rail", "change", "add", "lib-a", "--bump", "patch"])?;
  let combined = format!(
    "{}\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(
    output.status.code(),
    Some(2),
    "non-tty authoring should fail\n{}",
    combined
  );
  assert!(
    combined.contains("requires --message in non-interactive mode"),
    "{}",
    combined
  );

  Ok(())
}

#[test]
fn release_changelog_uses_graph_attribution_for_cross_crate_commits() -> Result<()> {
  let ws = TestWorkspace::new_named("release-graph-attribution")?;
  write_release_config(&ws, "require_release_notes = false")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add crates")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  tag_release(&ws, "lib-b", "0.1.0")?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn cross_a() {}\n")?;
  ws.modify_file("lib-b", "src/lib.rs", "pub fn cross_b() {}\n")?;
  ws.commit("fix: repair shared behavior")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    output.status.success(),
    "release should succeed\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  let changelog_a = std::fs::read_to_string(ws.path.join("crates/lib-a/CHANGELOG.md"))?;
  let changelog_b = std::fs::read_to_string(ws.path.join("crates/lib-b/CHANGELOG.md"))?;
  assert!(changelog_a.contains("repair shared behavior"));
  assert!(changelog_b.contains("repair shared behavior"));

  Ok(())
}

#[test]
fn release_check_denies_unconventional_commits_when_configured() -> Result<()> {
  let ws = TestWorkspace::new_named("release-deny-unconventional")?;
  write_release_config(&ws, "unconventional_commits = \"deny\"")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
  ws.commit("Update lib-a")?;

  let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  let combined = format!("{}\n{}", stdout, stderr);

  assert!(
    !output.status.success(),
    "release check should fail for unconventional commits with deny policy\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(combined.contains("not a conventional commit"), "output:\n{}", combined);

  Ok(())
}

#[test]
fn change_file_drives_auto_bump_and_is_consumed_on_release() -> Result<()> {
  let ws = TestWorkspace::new_named("release-change-file-auto")?;
  write_release_config(&ws, "require_release_notes = false")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "0.1.0")?;

  let add_output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "change",
      "add",
      "lib-a",
      "--bump",
      "minor",
      "--message",
      "Added reviewed release intent.",
    ],
  )?;
  let add_stdout = String::from_utf8_lossy(&add_output.stdout);
  assert!(add_output.status.success(), "change add failed:\n{}", add_stdout);
  let change_path = std::path::PathBuf::from(add_stdout.trim());
  assert!(
    change_path.exists(),
    "change file should exist at {}",
    change_path.display()
  );

  let status_output = run_cargo_rail(&ws.path, &["rail", "change", "status"])?;
  let status_stdout = String::from_utf8_lossy(&status_output.stdout);
  assert!(status_stdout.contains("lib-a: minor"), "status:\n{}", status_stdout);

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--bump",
      "auto",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    output.status.success(),
    "release should succeed from change file\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(
    !change_path.exists(),
    "release should consume {}",
    change_path.display()
  );

  let manifest = std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?;
  assert!(manifest.contains("version = \"0.2.0\""), "manifest:\n{}", manifest);
  let changelog = std::fs::read_to_string(ws.path.join("crates/lib-a/CHANGELOG.md"))?;
  assert!(
    changelog.contains("Added reviewed release intent."),
    "changelog:\n{}",
    changelog
  );

  Ok(())
}

#[test]
fn release_check_enforces_required_change_file_coverage() -> Result<()> {
  let ws = TestWorkspace::new_named("release-change-file-gate")?;
  write_release_config(&ws, "require_change_files = true")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
  ws.commit("fix: change lib-a")?;

  let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  let combined = format!("{}\n{}", stdout, stderr);
  assert!(
    !output.status.success(),
    "release check should fail without required change file\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(combined.contains("missing change files"), "output:\n{}", combined);
  assert!(combined.contains("lib-a"), "output:\n{}", combined);

  Ok(())
}

#[test]
fn release_changelog_generates_links_and_prs() -> Result<()> {
  let ws = TestWorkspace::new_named("release-links")?;
  ws.set_remote("git@github.com:org/repo.git")?;
  write_release_config(&ws, "")?;

  // Create crate and initial tag
  ws.add_crate("lib-a", "0.1.0", &[])?;
  let initial_sha = ws.commit("Add lib-a")?;
  // Single-crate tag format uses plain v{version}
  ws.tag("v0.1.0", "Initial lib-a release")?;

  // Feature commit with PR refs and breaking body
  ws.modify_file("lib-a", "src/lib.rs", "pub fn api_v2() {}")?;
  let feature_sha = ws.commit("feat(api)!: redesign REST endpoints (#123)\n\ncloses #456")?;

  // Run release (skip crates.io but create tag/changelog)
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    output.status.success(),
    "release publish should succeed\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  // Read changelog
  let changelog = std::fs::read_to_string(ws.path.join("crates/lib-a/CHANGELOG.md"))?;

  // Header with compare URL
  let has_compare =
    changelog.contains("compare/v0.1.0...v0.1.1") || changelog.contains("compare/lib-a-v0.1.0...lib-a-v0.1.1");
  assert!(
    has_compare,
    "changelog should contain compare link. Content:\n{}",
    changelog
  );

  // Breaking section and inline marker
  assert!(changelog.contains("BREAKING CHANGES"));
  assert!(changelog.contains("[**breaking**] redesign REST endpoints"));

  // PR links and commit link
  let short_sha = &feature_sha[..7];
  assert!(changelog.contains("https://github.com/org/repo/pull/123"));
  assert!(changelog.contains("https://github.com/org/repo/pull/456"));
  assert!(
    changelog.contains(&format!("https://github.com/org/repo/commit/{}", feature_sha)),
    "should link commit {}",
    feature_sha
  );

  // Ensure release commit didn't get tagged as the only change (initial sha should be excluded from range)
  assert!(changelog.contains(short_sha), "should include feature commit");
  assert!(
    !changelog.contains(&initial_sha[..7]),
    "should not include pre-tag commits"
  );

  Ok(())
}

#[test]
fn release_respects_skip_and_require_flags() -> Result<()> {
  let ws = TestWorkspace::new_named("release-skip-require")?;
  ws.set_remote("git@github.com:org/repo.git")?;
  write_release_config(
    &ws,
    "require_changelog_entries = true\n\n[crates.internal.changelog]\nskip = true",
  )?;

  // Crate with changes
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  ws.tag("lib-a-v0.1.0", "Initial lib-a")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn change() {}")?;
  ws.commit("fix: update lib-a")?;

  // Crate with no changes and no skip (should fail)
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add lib-b")?;
  ws.tag("lib-b-v0.1.0", "Initial lib-b")?;

  // Crate marked as skip (no changelog expected)
  ws.add_crate("internal", "0.1.0", &[])?;
  ws.commit("Add internal crate")?;
  ws.tag("internal-v0.1.0", "Initial internal crate")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(
    !output.status.success(),
    "release should fail because lib-b has no changelog entries and require_changelog_entries = true\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  // On failure, ensure skipped crate did not get a changelog
  assert!(
    !ws.path.join("crates/internal/CHANGELOG.md").exists(),
    "internal crate changelog should be skipped"
  );

  Ok(())
}

#[test]
fn test_release_preflight_requires_release_notes_by_default() -> Result<()> {
  let ws = TestWorkspace::new_named("release-require-notes-default")?;
  ws.write_release_config(
    r#"tag_prefix = "v"
tag_format = "v{version}"
require_changelog_entries = false
require_release_notes = true
require_clean = false
"#,
  )?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  ws.tag("v0.1.0", "Initial lib-a")?;

  // No commits since last tag -> generated changelog entries are empty.
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(
    !output.status.success(),
    "release should fail preflight when release notes are missing\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(
    stderr.contains("no release notes for lib-a v0.1.1") || stdout.contains("no release notes for lib-a v0.1.1"),
    "expected missing release notes error\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  Ok(())
}

#[test]
fn test_release_preflight_can_disable_release_notes_requirement() -> Result<()> {
  let ws = TestWorkspace::new_named("release-require-notes-disabled")?;
  ws.write_release_config(
    r#"tag_prefix = "v"
tag_format = "v{version}"
require_changelog_entries = false
require_release_notes = false
require_clean = false
"#,
  )?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  ws.tag("v0.1.0", "Initial lib-a")?;

  // No commits since last tag, but opt-out should allow release apply.
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(
    output.status.success(),
    "release should succeed when require_release_notes=false\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  Ok(())
}

#[test]
fn test_release_rejects_github_release_without_owned_push() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("unsafe-gh-release", "0.1.0")?;
  ws.write_release_config(
    r#"require_clean = false
create_github_release = true
push = false
"#,
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "release", "run", "--check", "--bump", "patch"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(
    !output.status.success(),
    "unsafe GitHub release config should fail\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(
    stdout.contains("requires release.push = true") || stderr.contains("requires release.push = true"),
    "expected owned-push error\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  Ok(())
}

#[cfg(unix)]
#[test]
fn test_release_creates_gitlab_release_with_glab() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("gitlab-release", "0.1.0")?;
  ws.write_release_config(
    r#"tag_prefix = "v"
tag_format = "v{version}"
require_clean = false
require_release_notes = false
remote_effects = "gitlab"
"#,
  )?;
  ws.tag("v0.1.0", "Initial release")?;
  std::fs::write(ws.path.join("src/lib.rs"), "pub fn changed() {}\n")?;
  ws.commit("fix: update gitlab release test crate")?;

  let remote = tempfile::TempDir::new()?;
  git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
  ws.set_remote(remote.path().to_str().unwrap())?;
  git(&ws.path, &["push", "-u", "origin", "main"])?;

  let glab_log_dir = tempfile::TempDir::new()?;
  let glab_log = glab_log_dir.path().join("glab.log");
  let (_glab_dir, glab_path) = glab_shim(&glab_log)?;
  let output = run_with_path_prefix(
    &ws,
    glab_path.parent().unwrap(),
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    output.status.success(),
    "GitLab release should succeed\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  let glab_log = std::fs::read_to_string(&glab_log)?;
  assert!(
    glab_log.contains("release view v0.1.1") && glab_log.contains("release create v0.1.1"),
    "glab should check then create the release\n{}",
    glab_log
  );
  assert!(
    glab_log.contains("--name gitlab-release v0.1.1") && glab_log.contains("--notes-file"),
    "glab release create args should include the title and notes file\n{}",
    glab_log
  );

  Ok(())
}

#[cfg(unix)]
#[test]
fn test_release_errors_when_gitlab_forge_binary_missing() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("missing-glab", "0.1.0")?;
  ws.write_release_config(
    r#"tag_prefix = "v"
tag_format = "v{version}"
require_clean = false
require_release_notes = false
remote_effects = "gitlab"
"#,
  )?;
  ws.tag("v0.1.0", "Initial release")?;
  std::fs::write(ws.path.join("src/lib.rs"), "pub fn changed() {}\n")?;
  ws.commit("fix: update missing glab test crate")?;

  let output = run_with_minimal_path_without_forge(
    &ws,
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let combined = format!(
    "{}\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(
    output.status.code(),
    Some(2),
    "missing glab should fail before release mutation\n{}",
    combined
  );
  assert!(
    combined.contains("GitLab releases enabled but glab CLI was not found")
      && combined.contains("install glab or set release.remote_effects = \"push\""),
    "output:\n{}",
    combined
  );

  Ok(())
}

#[cfg(unix)]
#[test]
fn test_release_pushes_commit_and_tag_when_push_enabled() -> Result<()> {
  let (ws, _remote) = push_release_workspace("push-release")?;
  ws.write_release_config(
    r#"tag_format = "v{version}"
source = "both"
require_clean = false
require_release_notes = false
remote_effects = "gitlab"
"#,
  )?;
  let glab_log_dir = tempfile::TempDir::new()?;
  let glab_log = glab_log_dir.path().join("glab.log");
  let (_glab_dir, glab_path) = glab_shim(&glab_log)?;

  let hook_counter = ws.path.join(".git/pre-push-count");
  install_pre_push_hook(
    &ws,
    r#"#!/bin/sh
count_file="$(dirname "$0")/../pre-push-count"
count=0
if [ -f "$count_file" ]; then
  count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
if [ "${CARGO_RAIL_TEST_INHERITED:-}" != "from-caller" ]; then
  echo "missing inherited caller environment" >&2
  exit 1
fi
if [ "${CARGO_RAIL_RELEASE_PUSH:-}" != "1" ]; then
  echo "missing CARGO_RAIL_RELEASE_PUSH" >&2
  exit 1
fi
if [ "${CARGO_RAIL_OPERATION:-}" != "release" ]; then
  echo "missing CARGO_RAIL_OPERATION" >&2
  exit 1
fi
echo "release hook context accepted"
"#,
  )?;

  let trace_dir = tempfile::TempDir::new()?;
  let trace_path = trace_dir.path().join("git-trace.log");
  let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&ws.path)
    .env(
      "PATH",
      format!(
        "{}:{}",
        glab_path.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
      ),
    )
    .env("CARGO_RAIL_TEST_INHERITED", "from-caller")
    .env("GIT_DIR", ws.path.join("ambient-wrong-repository"))
    .env("GIT_TRACE", &trace_path)
    .args([
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ])
    .output()?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(
    output.status.success(),
    "release should push commit and tag\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  let remote_tags = git(&ws.path, &["ls-remote", "--tags", "origin", "v0.1.1"])?;
  assert!(
    !remote_tags.stdout.is_empty(),
    "remote should contain pushed release tag"
  );

  let remote_head = git(&ws.path, &["ls-remote", "origin", "refs/heads/main"])?;
  let local_head = git(&ws.path, &["rev-parse", "HEAD"])?;
  assert_eq!(
    String::from_utf8_lossy(&remote_head.stdout).split_whitespace().next(),
    Some(String::from_utf8_lossy(&local_head.stdout).trim())
  );
  assert!(
    !stdout.contains("git push origin"),
    "owned push should not print manual push follow-up"
  );
  assert!(
    stdout.contains("release hook context accepted"),
    "successful hook diagnostics should stream to stdout\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  let hook_runs = std::fs::read_to_string(&hook_counter)?;
  assert_eq!(
    hook_runs.trim(),
    "2",
    "commit and tag pushes are separate Git transitions, and preflight must not run hooks"
  );
  let trace = std::fs::read_to_string(&trace_path)?;
  assert!(
    trace.contains("push --atomic"),
    "release must retain its atomic push\n{}",
    trace
  );
  let glab_log = std::fs::read_to_string(&glab_log)?;
  let readiness = glab_log.find("api projects/:id/pipelines?sha=").unwrap();
  let release = glab_log.find("release create v0.1.1").unwrap();
  assert!(
    readiness < release,
    "exact-SHA readiness must precede release creation\n{}",
    glab_log
  );
  assert!(
    !trace.contains("--no-verify"),
    "cargo-rail must never bypass repository hooks\n{}",
    trace
  );

  Ok(())
}

#[cfg(unix)]
#[test]
fn release_reconstructs_missing_journal_from_git_in_a_second_checkout() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("cross-checkout", "0.1.0")?;
  ws.write_release_config(
    r#"tag_format = "v{version}"
source = "both"
require_clean = false
require_release_notes = false
remote_effects = "gitlab"
"#,
  )?;
  ws.commit("Configure release reconstruction")?;
  ws.tag("v0.1.0", "Initial release")?;
  let remote = tempfile::TempDir::new()?;
  git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
  ws.set_remote(remote.path().to_str().unwrap())?;
  git(&ws.path, &["push", "-u", "origin", "main"])?;

  let shim_state = tempfile::TempDir::new()?;
  let pending_log = shim_state.path().join("pending.log");
  let (_pending_dir, pending_glab) = glab_shim_with_status(&pending_log, "running")?;
  let interrupted = run_with_path_prefix(
    &ws,
    pending_glab.parent().unwrap(),
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;
  assert!(!interrupted.status.success());
  assert!(String::from_utf8_lossy(&interrupted.stderr).contains("awaiting exact-SHA checks"));
  assert!(
    git(&ws.path, &["ls-remote", "--tags", "origin", "v0.1.1"])?
      .stdout
      .is_empty()
  );
  let remote_head = git(&ws.path, &["ls-remote", "origin", "refs/heads/main"])?.stdout;

  let clone_root = tempfile::TempDir::new()?;
  let clone = clone_root.path().join("checkout");
  let cloned = Command::new("git")
    .args(["clone", remote.path().to_str().unwrap(), clone.to_str().unwrap()])
    .output()?;
  assert!(cloned.status.success(), "{}", String::from_utf8_lossy(&cloned.stderr));
  git(&clone, &["config", "user.name", "Cargo Rail Test"])?;
  git(&clone, &["config", "user.email", "cargo-rail@example.com"])?;

  let status = run_cargo_rail(&clone, &["rail", "release", "status", "--format", "json"])?;
  assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
  let status: serde_json::Value = serde_json::from_slice(&status.stdout)?;
  let transaction = status["transactions"][0]["transaction_id"].as_str().unwrap();
  assert_eq!(status["transactions"][0]["recoverability"], "reconstructable");
  assert_eq!(
    status["transactions"][0]["exact_sha"].as_str().unwrap().as_bytes(),
    remote_head.split(|b| *b == b'\t').next().unwrap()
  );

  let green_log = shim_state.path().join("green.log");
  let (_green_dir, green_glab) = glab_shim_with_status(&green_log, "success")?;
  let path = format!(
    "{}:{}",
    green_glab.parent().unwrap().display(),
    std::env::var("PATH").unwrap_or_default()
  );
  let resumed = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&clone)
    .env("PATH", path)
    .env("GIT_CONFIG_COUNT", "2")
    .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
    .env("GIT_CONFIG_VALUE_0", "false")
    .env("GIT_CONFIG_KEY_1", "tag.gpgsign")
    .env("GIT_CONFIG_VALUE_1", "false")
    .args(["rail", "release", "resume", transaction])
    .output()?;
  assert!(
    resumed.status.success(),
    "second-checkout resume failed\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&resumed.stdout),
    String::from_utf8_lossy(&resumed.stderr)
  );
  assert!(
    !git(&clone, &["ls-remote", "--tags", "origin", "v0.1.1"])?
      .stdout
      .is_empty()
  );
  assert_eq!(
    git(&clone, &["ls-remote", "origin", "refs/heads/main"])?.stdout,
    remote_head,
    "reconstruction must not create another release commit"
  );
  Ok(())
}

#[test]
fn test_release_hook_failure_streams_and_preserves_both_output_streams() -> Result<()> {
  let (ws, _remote) = push_release_workspace("push-hook-diagnostics")?;
  install_pre_push_hook(
    &ws,
    r#"#!/bin/sh
echo "hook stdout: release intent was rejected"
echo "hook stderr: policy details" >&2
exit 1
"#,
  )?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
      "--skip-tag",
      "--yes",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert_eq!(output.status.code(), Some(2), "rejected release push must fail");
  assert!(
    stdout.contains("hook stdout: release intent was rejected"),
    "hook stdout should stream while Git runs\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(
    stderr.contains("hook stdout: release intent was rejected") && stderr.contains("hook stderr: policy details"),
    "the final Git error must preserve both streams\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  Ok(())
}

#[test]
fn test_release_hook_failure_json_captures_structured_diagnostics() -> Result<()> {
  let (ws, _remote) = push_release_workspace("push-hook-json")?;
  install_pre_push_hook(
    &ws,
    r#"#!/bin/sh
echo "hook stdout: machine-readable release rejection"
echo "hook stderr: machine-readable policy details" >&2
exit 1
"#,
  )?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
      "--skip-tag",
      "--yes",
      "--json",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)
    .unwrap_or_else(|error| panic!("release failure must remain valid JSON: {}\n{}", error, stdout));

  assert_eq!(output.status.code(), Some(2), "rejected release push must fail");
  let message = json["message"].as_str().unwrap_or_default();
  assert!(
    message.contains("stdout:\nhook stdout: machine-readable release rejection"),
    "JSON errors must retain and label Git stdout\n{}",
    stdout
  );
  assert!(
    message.contains("stderr:\nhook stderr: machine-readable policy details"),
    "JSON errors must retain and label Git stderr\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_release_resume_reconciles_push_that_completed_before_failure() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("push-resume", "0.1.0")?;
  let remote = tempfile::TempDir::new()?;
  git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
  ws.set_remote(remote.path().to_str().unwrap())?;
  git(&ws.path, &["push", "-u", "origin", "main"])?;
  ws.write_release_config(
    r#"tag_format = "v{version}"
require_clean = false
require_release_notes = false
remote_effects = "push"
"#,
  )?;

  let interrupted = run_release_with_fault(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
      "--skip-tag",
      "--yes",
    ],
    "push",
  )?;
  assert!(!interrupted.status.success());
  let state_path = only_release_state(&ws.path)?;
  let remote_before = git(&ws.path, &["ls-remote", "origin", "refs/heads/main"])?;

  let resumed = run_cargo_rail(&ws.path, &["rail", "release", "resume", state_path.to_str().unwrap()])?;
  assert!(
    resumed.status.success(),
    "resume stderr:\n{}",
    String::from_utf8_lossy(&resumed.stderr)
  );
  let remote_after = git(&ws.path, &["ls-remote", "origin", "refs/heads/main"])?;
  assert_eq!(
    remote_before.stdout, remote_after.stdout,
    "resume should reconcile, not create another commit"
  );
  let state: serde_json::Value = serde_json::from_slice(&std::fs::read(state_path)?)?;
  assert_eq!(state["status"], "complete");
  Ok(())
}

#[test]
fn test_release_abort_reconciles_push_rejected_by_local_hook() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("push-abort", "0.1.0")?;
  let remote = tempfile::TempDir::new()?;
  git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
  ws.set_remote(remote.path().to_str().unwrap())?;
  git(&ws.path, &["push", "-u", "origin", "main"])?;
  let initial = git(&ws.path, &["rev-parse", "HEAD"])?;
  let initial = String::from_utf8_lossy(&initial.stdout).trim().to_string();

  let hook_path = ws.path.join(".git/hooks/pre-push");
  std::fs::write(&hook_path, "#!/bin/sh\nexit 1\n")?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = std::fs::metadata(&hook_path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&hook_path, perms)?;
  }
  ws.write_release_config(
    r#"tag_format = "v{version}"
require_clean = false
require_release_notes = false
remote_effects = "push"
"#,
  )?;

  let interrupted = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
      "--skip-tag",
      "--yes",
    ],
  )?;
  assert!(
    !interrupted.status.success(),
    "local pre-push hook should reject release push"
  );
  let state_path = only_release_state(&ws.path)?;

  let aborted = run_cargo_rail(
    &ws.path,
    &["rail", "release", "abort", state_path.to_str().unwrap(), "--yes"],
  )?;
  assert!(
    aborted.status.success(),
    "abort stderr:\n{}",
    String::from_utf8_lossy(&aborted.stderr)
  );
  assert_eq!(
    git(&ws.path, &["rev-parse", "HEAD"])?.stdout,
    format!("{}\n", initial).as_bytes()
  );
  assert!(git(&ws.path, &["tag", "--list", "v0.1.1"])?.stdout.is_empty());
  assert!(std::fs::read_to_string(ws.path.join("Cargo.toml"))?.contains("version = \"0.1.0\""));

  Ok(())
}

#[test]
fn test_release_notes_override_satisfies_required_notes() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("manual-notes", "0.1.0")?;
  ws.write_release_config(
    r#"tag_format = "v{version}"
require_clean = false
require_release_notes = true
"#,
  )?;
  ws.tag("v0.1.0", "Initial manual-notes")?;
  std::fs::create_dir_all(ws.path.join("release-notes"))?;
  std::fs::write(
    ws.path.join("release-notes/v0.1.1.md"),
    "## manual-notes v0.1.1\n\n- curated release notes\n",
  )?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(
    output.status.success(),
    "manual release notes should satisfy required release notes\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  Ok(())
}

/// Test release --json output format
#[test]
fn test_release_json_output() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("json-release", "0.1.0")?;

  // Configure release
  ws.write_release_config("require_clean = false\n")?;

  // Run release plan with --json
  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "--check", "--json", "--bump", "patch"],
  )?;
  assert_eq!(
    output.status.code(),
    Some(1),
    "release run --check --json should exit 1 when changes are pending"
  );
  let stdout = String::from_utf8_lossy(&output.stdout);
  let json: serde_json::Value = serde_json::from_str(&stdout)
    .unwrap_or_else(|_| panic!("release --json should output valid JSON. stdout: {}", stdout));
  assert_eq!(json["schema_version"], serde_json::json!(1));
  assert_eq!(json["command"], serde_json::json!("release"));
  assert_eq!(json["mode"], serde_json::json!("check"));
  assert_eq!(json["result"], serde_json::json!("pending_changes"));
  assert_eq!(json["exit_code"], serde_json::json!(1));
  assert!(json.get("release_plan").is_some());
  assert!(json.get("mutation_plan").is_some());

  Ok(())
}

/// Test release --skip-tag flag
#[test]
fn test_release_skip_tag_flag() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("skip-tag-crate", "0.1.0")?;

  // Configure release
  ws.write_release_config("require_clean = false\n")?;

  // Run release plan with --skip-tag
  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "--check", "--skip-tag", "--bump", "patch"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Exit code 1 = check found pending changes (correct behavior)
  assert!(
    output.status.code() == Some(1),
    "release --check should exit 1 when release pending"
  );
  assert!(
    stdout.contains("--skip-tag") || !stdout.contains("Tag:") || stdout.contains("skip"),
    "Should indicate tags are skipped in output.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

/// Test release --skip-publish flag reflects in plan
#[test]
fn test_release_skip_publish_in_plan() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("skip-pub-crate", "0.1.0")?;

  // Configure release
  ws.write_release_config("require_clean = false\n")?;

  // Run release plan with --skip-publish
  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "--check", "--skip-publish", "--bump", "patch"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Exit code 1 = check found pending changes (correct behavior)
  assert!(
    output.status.code() == Some(1),
    "release --check should exit 1 when release pending"
  );
  assert!(
    stdout.contains("--skip-publish") || stdout.contains("0 to publish"),
    "Should reflect skip-publish in plan.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

/// Test explicit version bump (e.g., "1.2.3" instead of "patch")
#[test]
fn test_release_explicit_version() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("explicit-ver", "0.1.0")?;

  // Configure release
  ws.write_release_config("require_clean = false\n")?;

  // Run release with explicit version
  let output = run_cargo_rail(&ws.path, &["rail", "release", "run", "--check", "--bump", "2.0.0"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Exit code 1 = check found pending changes (correct behavior)
  assert!(
    output.status.code() == Some(1),
    "release --check should exit 1 when release pending"
  );
  assert!(
    stdout.contains("2.0.0"),
    "Should show explicit version in plan.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

// release.changelog.relative_to tests
/// Test default changelog relative_to behavior (crate-relative)
#[test]
fn test_changelog_relative_to_crate_default() -> Result<()> {
  let ws = TestWorkspace::new_named("changelog-crate-rel")?;
  ws.set_remote("git@github.com:org/repo.git")?;

  // Don't set relative_to - should default to "crate"
  ws.write_release_config(
    r#"source = "commits"
require_clean = false

[release.changelog]
path = "CHANGELOG.md"
"#,
  )?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  ws.tag("lib-a-v0.1.0", "Initial lib-a")?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn v2() {}")?;
  ws.commit("feat: add v2 function")?;

  // Run release
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    output.status.success(),
    "release should succeed\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  // Changelog should be at crates/lib-a/CHANGELOG.md (crate-relative)
  let crate_changelog = ws.path.join("crates/lib-a/CHANGELOG.md");
  let workspace_changelog = ws.path.join("CHANGELOG.md");

  assert!(
    crate_changelog.exists(),
    "Changelog should exist at crate-relative path: {}",
    crate_changelog.display()
  );
  assert!(
    !workspace_changelog.exists(),
    "Changelog should NOT exist at workspace root when using crate-relative"
  );

  Ok(())
}

/// Test release.changelog.relative_to = "workspace" creates changelog at workspace root
#[test]
fn test_changelog_relative_to_workspace() -> Result<()> {
  let ws = TestWorkspace::new_named("changelog-ws-rel")?;
  ws.set_remote("git@github.com:org/repo.git")?;

  // Explicitly set relative_to = "workspace"
  ws.write_release_config(
    r#"source = "commits"
require_clean = false

[release.changelog]
path = "CHANGELOG.md"
relative_to = "workspace"
"#,
  )?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  ws.tag("lib-a-v0.1.0", "Initial lib-a")?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn v2() {}")?;
  ws.commit("feat: add v2 function")?;

  // Run release
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    output.status.success(),
    "release should succeed\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  // Changelog should be at workspace root (workspace-relative)
  let workspace_changelog = ws.path.join("CHANGELOG.md");
  let crate_changelog = ws.path.join("crates/lib-a/CHANGELOG.md");

  assert!(
    workspace_changelog.exists(),
    "Changelog should exist at workspace root: {}",
    workspace_changelog.display()
  );
  assert!(
    !crate_changelog.exists(),
    "Changelog should NOT exist at crate directory when using workspace-relative"
  );

  // Verify changelog content
  let content = std::fs::read_to_string(&workspace_changelog)?;
  assert!(
    content.contains("lib-a") || content.contains("0.1.1"),
    "Changelog should contain release info. Content:\n{}",
    content
  );

  Ok(())
}

#[test]
fn release_rejects_an_absolute_changelog_path_outside_the_workspace() -> Result<()> {
  let ws = TestWorkspace::new_named("changelog-outside-workspace")?;
  let outside = tempfile::TempDir::new()?;
  let outside_path = outside.path().join("CHANGELOG.md");
  ws.write_release_config(&format!(
    r#"source = "commits"
require_clean = false

[release.changelog]
path = "{}"
relative_to = "workspace"
"#,
    outside_path.display().to_string().replace('\\', "\\\\")
  ))?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  ws.tag("lib-a-v0.1.0", "Initial lib-a")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}")?;
  ws.commit("feat: change lib-a")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "lib-a", "--bump", "patch", "--check"],
  )?;
  assert!(!output.status.success());
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("escapes workspace") || stderr.contains("outside git worktree"),
    "outside changelog path should fail before mutation\nstderr:\n{}",
    stderr
  );
  assert!(!outside_path.exists());
  Ok(())
}

#[cfg(unix)]
#[test]
fn release_rejects_a_symlink_changelog_path() -> Result<()> {
  use std::os::unix::fs::symlink;

  let ws = TestWorkspace::new_named("changelog-symlink")?;
  ws.write_release_config(
    r#"source = "commits"
require_clean = false

[release.changelog]
path = "CHANGELOG.md"
relative_to = "workspace"
"#,
  )?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  ws.tag("lib-a-v0.1.0", "Initial lib-a")?;
  let outside = tempfile::TempDir::new()?;
  let victim = outside.path().join("victim");
  std::fs::write(&victim, "outside\n")?;
  symlink(&victim, ws.path.join("CHANGELOG.md"))?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}")?;
  ws.commit("feat: change lib-a")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "lib-a", "--bump", "patch", "--check"],
  )?;
  assert!(!output.status.success());
  assert_eq!(std::fs::read_to_string(victim)?, "outside\n");
  Ok(())
}

/// Test that parent directories are auto-created for changelog paths
#[test]
fn test_changelog_parent_directories_auto_created() -> Result<()> {
  let ws = TestWorkspace::new_named("changelog-auto-mkdir")?;
  ws.set_remote("git@github.com:org/repo.git")?;

  // Use a nested path that doesn't exist
  ws.write_release_config(
    r#"source = "commits"
require_clean = false

[release.changelog]
path = "docs/changelogs/CHANGELOG.md"
relative_to = "workspace"
"#,
  )?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  ws.tag("lib-a-v0.1.0", "Initial lib-a")?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn v2() {}")?;
  ws.commit("feat: add v2 function")?;

  // docs/changelogs/ doesn't exist yet - should be auto-created
  assert!(!ws.path.join("docs/changelogs").exists());

  // Run release
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    output.status.success(),
    "release should succeed with auto-created directories\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  // Verify directory and changelog were created
  let changelog_path = ws.path.join("docs/changelogs/CHANGELOG.md");
  assert!(
    changelog_path.exists(),
    "Changelog should exist at nested path: {}",
    changelog_path.display()
  );
  assert!(
    ws.path.join("docs/changelogs").is_dir(),
    "Parent directories should be auto-created"
  );

  Ok(())
}

/// Test release.changelog.relative_to = "crate" with custom path creates in crate subdir
#[test]
fn test_changelog_relative_to_crate_custom_path() -> Result<()> {
  let ws = TestWorkspace::new_named("changelog-crate-custom")?;
  ws.set_remote("git@github.com:org/repo.git")?;

  // Use custom path with crate-relative
  ws.write_release_config(
    r#"source = "commits"
require_clean = false

[release.changelog]
path = "docs/CHANGES.md"
relative_to = "crate"
"#,
  )?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  ws.tag("lib-a-v0.1.0", "Initial lib-a")?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn v2() {}")?;
  ws.commit("feat: add v2 function")?;

  // Run release
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    output.status.success(),
    "release should succeed\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  // Changelog should be at crates/lib-a/docs/CHANGES.md
  let changelog_path = ws.path.join("crates/lib-a/docs/CHANGES.md");
  assert!(
    changelog_path.exists(),
    "Changelog should exist at custom crate-relative path: {}",
    changelog_path.display()
  );

  Ok(())
}

// Prerelease Bump Tests

/// Test --bump prerelease from stable version
#[test]
fn test_bump_prerelease_from_stable() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("prerelease-test", "1.0.0")?;
  ws.write_release_config("require_clean = false\n")?;

  // Run release plan with --bump prerelease
  let output = run_cargo_rail(&ws.path, &["rail", "release", "run", "--check", "--bump", "prerelease"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show 1.0.0 -> 1.0.0-rc.1
  assert!(
    stdout.contains("1.0.0-rc.1"),
    "Should bump to rc.1 prerelease.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

/// Test --bump prerelease increments existing prerelease
#[test]
fn test_bump_prerelease_increment() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("prerelease-inc", "2.0.0-rc.1")?;
  ws.write_release_config("require_clean = false\n")?;

  // Run release plan with --bump prerelease
  let output = run_cargo_rail(&ws.path, &["rail", "release", "run", "--check", "--bump", "prerelease"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show 2.0.0-rc.1 -> 2.0.0-rc.2
  assert!(
    stdout.contains("2.0.0-rc.2"),
    "Should increment to rc.2.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

/// Test --bump release strips prerelease suffix
#[test]
fn test_bump_release_strips_prerelease() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("release-strip", "1.5.0-beta.3")?;
  ws.write_release_config("require_clean = false\n")?;

  // Run release plan with --bump release
  let output = run_cargo_rail(&ws.path, &["rail", "release", "run", "--check", "--bump", "release"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show 1.5.0-beta.3 -> 1.5.0
  // The output contains both versions in format "1.5.0-beta.3 → 1.5.0"
  assert!(
    stdout.contains("1.5.0-beta.3") && stdout.contains("→ 1.5.0"),
    "Should strip prerelease to 1.5.0.\nOutput:\n{}",
    stdout
  );

  Ok(())
}

// Extended Check Tests

/// Test release check --extended runs dry-run publish validation
#[test]
fn test_release_check_extended_validates_publish() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("ext-check", "0.1.0")?;
  ws.write_release_config("require_clean = false\n")?;

  // Run release check with --extended --all (single-crate needs explicit crate name or --all)
  let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--extended", "--all"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  // Should run the extended checks
  assert!(
    stdout.contains("extended") || stdout.contains("publish-dry-run") || stdout.contains("msrv"),
    "Extended check should run dry-run and/or msrv checks.\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  Ok(())
}

/// Test release check --extended with JSON output
#[test]
fn test_release_check_extended_json() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("ext-json", "0.1.0")?;
  ws.write_release_config("require_clean = false\n")?;

  // Run release check with --extended --json --all
  let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--extended", "--json", "--all"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should be valid JSON with extended field
  let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
  assert!(
    parsed.is_ok(),
    "Extended check --json should output valid JSON.\nstdout:\n{}",
    stdout
  );

  let json = parsed.unwrap();
  assert_eq!(json["schema_version"], serde_json::json!(1));
  assert_eq!(json["command"], serde_json::json!("release"));
  assert_eq!(json["mode"], serde_json::json!("validate"));
  assert!(json["result"] == serde_json::json!("success") || json["result"] == serde_json::json!("failed"));
  assert!(
    json["exit_code"] == serde_json::json!(0) || json["exit_code"] == serde_json::json!(2),
    "release check extended should report exit_code 0 or 2"
  );
  assert!(
    json.get("extended").is_some(),
    "JSON should contain 'extended' field.\nJSON:\n{}",
    serde_json::to_string_pretty(&json).unwrap_or_default()
  );

  Ok(())
}

// Release Safety Tests (Branch Detection)

#[test]
fn release_rejects_unsafe_tag_names_before_mutation() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("unsafe-release-tag", "0.1.0")?;
  ws.write_release_config(
    r#"tag_prefix = "-"
source = "both"
require_clean = false
require_release_notes = false
"#,
  )?;
  ws.commit("Configure unsafe release tag")?;
  std::fs::write(ws.path.join("src/lib.rs"), "pub fn changed() {}")?;
  let initial_head = ws.commit("Change unsafe release tag crate")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;
  assert!(!output.status.success());
  assert!(String::from_utf8_lossy(&output.stderr).contains("is not a safe Git ref name"));
  assert_eq!(
    String::from_utf8_lossy(&git(&ws.path, &["rev-parse", "HEAD"])?.stdout).trim(),
    initial_head
  );
  assert!(git(&ws.path, &["tag", "--list"])?.stdout.is_empty());
  assert!(
    !ws.path.join("target/cargo-rail/releases").exists(),
    "invalid tag configuration must fail before journal creation"
  );
  Ok(())
}

#[test]
fn test_release_resume_reconciles_tag_created_before_failure() -> Result<()> {
  let ws = TestWorkspace::new_named("release-resume-tag")?;
  write_release_config(&ws, "")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  ws.tag("lib-a-v0.1.0", "Initial release")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn resumed() {}")?;
  ws.commit("feat: resumable release")?;

  let interrupted = run_release_with_fault(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
    "tag",
  )?;
  assert!(!interrupted.status.success());
  assert!(String::from_utf8_lossy(&interrupted.stderr).contains("cargo rail release resume"));
  let state_path = only_release_state(&ws.path)?;
  let before = git(&ws.path, &["rev-list", "--count", "HEAD"])?;

  let resumed = run_cargo_rail(&ws.path, &["rail", "release", "resume", state_path.to_str().unwrap()])?;
  assert!(
    resumed.status.success(),
    "resume failed:\n{}",
    String::from_utf8_lossy(&resumed.stderr)
  );
  let after = git(&ws.path, &["rev-list", "--count", "HEAD"])?;
  assert_eq!(
    before.stdout, after.stdout,
    "resume must not duplicate the release commit"
  );
  let tags = git(&ws.path, &["tag", "--list", "v0.1.1"])?;
  assert_eq!(String::from_utf8_lossy(&tags.stdout).lines().count(), 1);
  let state: serde_json::Value = serde_json::from_slice(&std::fs::read(state_path)?)?;
  assert_eq!(state["status"], "complete");
  Ok(())
}

#[test]
fn release_resume_rejects_same_branch_head_movement() -> Result<()> {
  let ws = TestWorkspace::new_named("release-resume-head-drift")?;
  write_release_config(&ws, "")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  ws.tag("lib-a-v0.1.0", "Initial release")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}")?;
  ws.commit("feat: prepare release")?;

  let interrupted = run_release_with_fault(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
    "tag",
  )?;
  assert!(!interrupted.status.success());
  let state_path = only_release_state(&ws.path)?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn moved_after_release() {}")?;
  ws.commit("feat: move release branch")?;

  let resumed = run_cargo_rail(&ws.path, &["rail", "release", "resume", state_path.to_str().unwrap()])?;
  assert!(!resumed.status.success());
  let stderr = String::from_utf8_lossy(&resumed.stderr);
  assert!(
    stderr.contains("persisted release commit"),
    "resume should reject same-branch HEAD drift\nstderr:\n{}",
    stderr
  );
  Ok(())
}

#[test]
fn release_recovery_survives_invalid_metadata_and_clean_refuses_active_state() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("release-status-active", "0.1.0")?;
  ws.write_release_config(
    r#"source = "both"
tag_format = "v{version}"
require_clean = false
require_release_notes = false
"#,
  )?;
  let interrupted = run_release_with_before_fault(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
      "--skip-tag",
      "--yes",
    ],
    "commit:release-status-active",
  )?;
  assert!(!interrupted.status.success());
  let state_path = only_release_state(&ws.path)?;
  let manifest_path = ws.path.join("Cargo.toml");
  let manifest = std::fs::read_to_string(&manifest_path)?;
  std::fs::write(&manifest_path, "not valid Cargo metadata\n")?;

  let status = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "status",
      state_path.to_str().unwrap(),
      "--format",
      "json",
    ],
  )?;
  assert!(
    status.status.success(),
    "status must not load broken Cargo metadata: {}",
    String::from_utf8_lossy(&status.stderr)
  );
  let status: serde_json::Value = serde_json::from_slice(&status.stdout)?;
  assert_eq!(status["transactions"][0]["state"], "planned:active");
  assert_eq!(status["transactions"][0]["ambiguity"], true);
  assert!(
    status["transactions"][0]["safe_operator_command"]
      .as_str()
      .unwrap_or_default()
      .contains("release resume")
  );

  std::fs::write(&manifest_path, manifest)?;
  let clean = run_cargo_rail(&ws.path, &["rail", "clean"])?;
  assert!(!clean.status.success(), "clean must refuse an active journal");
  assert!(String::from_utf8_lossy(&clean.stderr).contains("clean refused active release transaction"));

  std::fs::write(&manifest_path, "not valid Cargo metadata again\n")?;
  let config_path = ws.path.join(".config/rail.toml");
  assert!(config_path.exists(), "test release config disappeared before recovery");
  let resumed = run_cargo_rail(&ws.path, &["rail", "release", "resume", state_path.to_str().unwrap()])?;
  assert!(resumed.status.success(), "{}", String::from_utf8_lossy(&resumed.stderr));
  let cleaned = run_cargo_rail(&ws.path, &["rail", "clean"])?;
  assert!(cleaned.status.success(), "{}", String::from_utf8_lossy(&cleaned.stderr));
  assert!(!state_path.exists(), "clean should prune the completed journal");
  Ok(())
}

#[test]
fn release_resume_reconciles_a_journal_write_that_failed_after_persistence() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("journal-fault", "0.1.0")?;
  ws.write_release_config(
    r#"source = "both"
tag_format = "v{version}"
require_clean = false
require_release_notes = false
"#,
  )?;
  let before = git(&ws.path, &["rev-list", "--count", "HEAD"])?;
  let interrupted = run_release_with_fault_env(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
      "--skip-tag",
      "--yes",
    ],
    "CARGO_RAIL_RELEASE_FAIL_AFTER",
    "journal:commit_observed:journal-fault",
  )?;
  assert!(!interrupted.status.success());
  let state_path = only_release_state(&ws.path)?;
  let after_fault = git(&ws.path, &["rev-list", "--count", "HEAD"])?;
  assert_ne!(
    before.stdout, after_fault.stdout,
    "the commit effect should have completed"
  );

  let resumed = run_cargo_rail(&ws.path, &["rail", "release", "resume", state_path.to_str().unwrap()])?;
  assert!(resumed.status.success(), "{}", String::from_utf8_lossy(&resumed.stderr));
  let after_resume = git(&ws.path, &["rev-list", "--count", "HEAD"])?;
  assert_eq!(
    after_fault.stdout, after_resume.stdout,
    "resume must not duplicate the commit"
  );
  Ok(())
}

#[test]
fn clean_prunes_a_planned_journal_superseded_before_any_effect() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("superseded-journal", "0.1.0")?;
  ws.write_release_config(
    r#"source = "both"
tag_format = "v{version}"
require_clean = false
require_release_notes = false
"#,
  )?;
  let interrupted = run_release_with_fault_env(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
      "--skip-tag",
      "--yes",
    ],
    "CARGO_RAIL_RELEASE_FAIL_AFTER",
    "journal:planned",
  )?;
  assert!(!interrupted.status.success());
  let state_path = only_release_state(&ws.path)?;
  std::fs::write(ws.path.join("superseding.txt"), "new release input\n")?;
  ws.commit("Supersede unstarted release plan")?;

  let cleaned = run_cargo_rail(&ws.path, &["rail", "clean"])?;
  assert!(cleaned.status.success(), "{}", String::from_utf8_lossy(&cleaned.stderr));
  assert!(
    !state_path.exists(),
    "clean should prune the effect-free superseded journal"
  );
  Ok(())
}

#[test]
fn release_transaction_id_is_recorded_in_commits_and_terminal_status() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("release-transaction", "0.1.0")?;
  ws.write_release_config(
    r#"source = "both"
tag_format = "v{version}"
require_clean = false
require_release_notes = false
"#,
  )?;
  let released = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;
  assert!(
    released.status.success(),
    "{}",
    String::from_utf8_lossy(&released.stderr)
  );
  let state_path = only_release_state(&ws.path)?;
  let state: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
  let transaction_id = state["transaction_id"].as_str().unwrap();
  assert_eq!(state["phase"], "released");
  let message = git(&ws.path, &["log", "-1", "--format=%B"])?;
  assert!(
    String::from_utf8_lossy(&message.stdout).contains(&format!("Rail-Release: {}", transaction_id)),
    "release commit must carry the plan-bound transaction identity"
  );

  let status = run_cargo_rail(&ws.path, &["rail", "release", "status", "--format", "json"])?;
  let status: serde_json::Value = serde_json::from_slice(&status.stdout)?;
  assert_eq!(status["transactions"][0]["state"], "released:complete");
  assert_eq!(status["transactions"][0]["recoverability"], "terminal");
  let cleaned = run_cargo_rail(&ws.path, &["rail", "clean"])?;
  assert!(cleaned.status.success(), "{}", String::from_utf8_lossy(&cleaned.stderr));
  let reconstructed = run_cargo_rail(&ws.path, &["rail", "release", "status", "--format", "json"])?;
  let reconstructed: serde_json::Value = serde_json::from_slice(&reconstructed.stdout)?;
  assert_eq!(reconstructed["transactions"][0]["state"], "released:git");
  assert_eq!(reconstructed["transactions"][0]["recoverability"], "terminal");
  Ok(())
}

#[test]
fn test_release_abort_restores_local_state_before_remote_side_effects() -> Result<()> {
  let ws = TestWorkspace::new_named("release-abort-local")?;
  write_release_config(&ws, "")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  ws.tag("lib-a-v0.1.0", "Initial release")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn abortable() {}")?;
  let initial = ws.commit("feat: abortable release")?;

  let interrupted = run_release_with_fault(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
    "commit:lib-a",
  )?;
  assert!(!interrupted.status.success());
  let state_path = only_release_state(&ws.path)?;
  let aborted = run_cargo_rail(
    &ws.path,
    &["rail", "release", "abort", state_path.to_str().unwrap(), "--yes"],
  )?;
  assert!(
    aborted.status.success(),
    "abort stderr:\n{}",
    String::from_utf8_lossy(&aborted.stderr)
  );
  let head = git(&ws.path, &["rev-parse", "HEAD"])?;
  assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), initial);
  assert!(!String::from_utf8_lossy(&git(&ws.path, &["tag", "--list", "v0.1.1"])?.stdout).contains("v0.1.1"));
  let manifest = std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?;
  assert!(manifest.contains("version = \"0.1.0\""));
  Ok(())
}

/// Test that release apply requires explicit confirmation in non-interactive mode
#[test]
fn test_release_requires_explicit_confirmation_non_interactive() -> Result<()> {
  let ws = TestWorkspace::new_named("release-confirmation-gate")?;
  write_release_config(&ws, "")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  ws.tag("lib-a-v0.1.0", "Initial release")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn gate() {}")?;
  ws.commit("feat: add release-gated change")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "lib-a", "--bump", "patch", "--skip-publish"],
  )?;
  assert!(
    !output.status.success(),
    "release should fail without --yes/--plan in non-interactive mode"
  );

  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("explicit confirmation") && stderr.contains("--yes") && stderr.contains("--plan"),
    "safety gate message missing expected guidance.\nstderr:\n{}",
    stderr
  );

  Ok(())
}

/// Test that release fails from detached HEAD
#[test]
fn test_release_detached_head_fails() -> Result<()> {
  let ws = TestWorkspace::new_named("release-detached")?;
  write_release_config(&ws, "")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  let commit_sha = ws.commit("Add lib-a")?;

  // Checkout detached HEAD
  crate::helpers::git(&ws.path, &["checkout", &commit_sha])?;

  // Run release (should fail with detached HEAD error)
  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "lib-a", "--bump", "patch", "--skip-publish"],
  );

  // Should fail (non-zero exit)
  let output = output?;
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(!output.status.success(), "Release from detached HEAD should fail");
  assert!(
    stderr.contains("detached HEAD") || stderr.contains("Detached HEAD"),
    "Error should mention detached HEAD.\nstderr:\n{}",
    stderr
  );

  Ok(())
}

/// Test that release from non-default branch fails without --yes
#[test]
fn test_release_non_default_branch_fails_without_yes() -> Result<()> {
  let ws = TestWorkspace::new_named("release-branch")?;
  write_release_config(&ws, "")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  // Create and switch to a feature branch
  crate::helpers::git(&ws.path, &["checkout", "-b", "feature-branch"])?;

  // Run release without --yes (should fail)
  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "lib-a", "--bump", "patch", "--skip-publish"],
  )?;

  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(
    !output.status.success(),
    "Release from non-default branch should fail without --yes.\nstderr:\n{}",
    stderr
  );
  assert!(
    stderr.contains("feature-branch") || stderr.contains("not default branch") || stderr.contains("--yes"),
    "Error should mention branch name or --yes flag.\nstderr:\n{}",
    stderr
  );

  Ok(())
}

/// Test that release from non-default branch succeeds with --yes
#[test]
fn test_release_non_default_branch_succeeds_with_yes() -> Result<()> {
  let ws = TestWorkspace::new_named("release-branch-yes")?;
  write_release_config(&ws, "")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  ws.tag("lib-a-v0.1.0", "Initial release")?;

  // Create and switch to a feature branch
  crate::helpers::git(&ws.path, &["checkout", "-b", "hotfix-1.0"])?;

  // Make a change for the release
  ws.modify_file("lib-a", "src/lib.rs", "pub fn hotfix() {}")?;
  ws.commit("feat: add hotfix function")?;

  // Run release with --yes (should succeed)
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;

  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(
    output.status.success(),
    "Release with --yes should succeed.\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  // Should show warning about non-default branch
  assert!(
    stderr.contains("warning") && stderr.contains("hotfix-1.0"),
    "Should warn about non-default branch.\nstderr:\n{}",
    stderr
  );

  let branch = git(&ws.path, &["branch", "--show-current"])?;
  assert!(
    String::from_utf8_lossy(&branch.stdout).trim() == "hotfix-1.0",
    "release should remain on the explicitly accepted branch"
  );
  let tag = git(&ws.path, &["rev-list", "-n", "1", "v0.1.1"])?;
  let head = git(&ws.path, &["rev-parse", "HEAD"])?;
  assert_eq!(
    String::from_utf8_lossy(&tag.stdout).trim(),
    String::from_utf8_lossy(&head.stdout).trim(),
    "local-only release tag should target the release commit"
  );
  assert!(
    !stderr.contains("git push origin"),
    "local-only completion must not suggest a push outside the journaled release protocol.\nstderr:\n{}",
    stderr
  );

  Ok(())
}

/// Helper to create a crate with publish = false in Cargo.toml
fn add_unpublishable_crate(ws: &TestWorkspace, name: &str, version: &str) -> Result<()> {
  let crate_path = ws.path.join("crates").join(name);
  std::fs::create_dir_all(&crate_path)?;
  std::fs::create_dir_all(crate_path.join("src"))?;

  // Cargo.toml with publish = false
  let cargo_toml = format!(
    r#"[package]
name = "{}"
version = "{}"
edition = "2024"
publish = false

[dependencies]
"#,
    name, version
  );
  std::fs::write(crate_path.join("Cargo.toml"), cargo_toml)?;

  // Add a basic `lib.rs`
  std::fs::write(crate_path.join("src/lib.rs"), "pub fn hello() {}\n")?;

  Ok(())
}

fn add_workspace_dependency(ws: &TestWorkspace, name: &str, version: &str) -> Result<()> {
  let root_manifest = ws.path.join("Cargo.toml");
  let manifest = std::fs::read_to_string(&root_manifest)?;
  let needle = "[workspace.dependencies]\n";
  let replacement = format!(
    "{}{} = {{ version = \"{}\", path = \"crates/{}\" }}\n",
    needle, name, version, name
  );
  let updated = manifest.replacen(needle, &replacement, 1);
  std::fs::write(root_manifest, updated)?;
  Ok(())
}

fn tag_release(ws: &TestWorkspace, crate_name: &str, version: &str) -> Result<()> {
  ws.tag(
    &format!("{}-v{}", crate_name, version),
    &format!("Release {} {}", crate_name, version),
  )
}

/// Helper to add a crate with a path-only dep
fn add_crate_with_path_dep(ws: &TestWorkspace, name: &str, version: &str, dep_name: &str, publish: bool) -> Result<()> {
  let crate_path = ws.path.join("crates").join(name);
  std::fs::create_dir_all(&crate_path)?;
  std::fs::create_dir_all(crate_path.join("src"))?;

  let publish_line = if publish { "" } else { "publish = false\n" };
  let cargo_toml = format!(
    r#"[package]
name = "{}"
version = "{}"
edition = "2021"
{}
[dependencies]
{} = {{ path = "../{}" }}
"#,
    name, version, publish_line, dep_name, dep_name
  );
  std::fs::write(crate_path.join("Cargo.toml"), cargo_toml)?;
  std::fs::write(crate_path.join("src/lib.rs"), "pub fn hello() {}\n")?;

  Ok(())
}

/// Test that --all skips crates with publish = false in Cargo.toml
#[test]
fn test_release_check_all_skips_unpublishable_cargo_toml() -> Result<()> {
  let ws = TestWorkspace::new_named("check-skip-unpub")?;
  write_release_config(&ws, "")?;

  // Add a publishable crate
  ws.add_crate("lib-pub", "0.1.0", &[])?;

  // Add an unpublishable crate (publish = false in Cargo.toml)
  add_unpublishable_crate(&ws, "lib-internal", "0.1.0")?;

  ws.commit("Add crates")?;

  // Run release check --all
  let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--all"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  // Should succeed
  assert!(
    output.status.success(),
    "release check --all should succeed.\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  // Should show lib-pub as ready
  assert!(
    stdout.contains("lib-pub: ready"),
    "Should report lib-pub as ready.\nstdout:\n{}",
    stdout
  );

  // Should report lib-internal as skipped (in stderr)
  assert!(
    stderr.contains("skipped") && stderr.contains("lib-internal"),
    "Should report lib-internal as skipped.\nstderr:\n{}",
    stderr
  );

  // Should mention publish = false
  assert!(
    stderr.contains("publish = false"),
    "Should explain why crate was skipped.\nstderr:\n{}",
    stderr
  );

  Ok(())
}

/// Test that path-only deps are allowed for crates with publish = false
#[test]
fn test_release_check_path_deps_allowed_for_unpublishable() -> Result<()> {
  let ws = TestWorkspace::new_named("path-dep-unpub")?;
  write_release_config(&ws, "")?;

  // Add a publishable crate
  ws.add_crate("lib-core", "0.1.0", &[])?;

  // Add an unpublishable crate with a path-only dep
  add_crate_with_path_dep(&ws, "wasm-bindings", "0.1.0", "lib-core", false)?;

  ws.commit("Add crates")?;

  // Run release check --all
  let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--all"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  // Should succeed - NOT error on path-only dep
  assert!(
    output.status.success(),
    "Should NOT error on path-only dep in unpublishable crate.\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  // Should NOT contain the path-only dependency error
  assert!(
    !stderr.contains("path-only dependency"),
    "Should not complain about path-only deps for unpublishable crates.\nstderr:\n{}",
    stderr
  );

  Ok(())
}

/// Test that explicitly naming an unpublishable crate reports its status
#[test]
fn test_release_check_explicit_unpublishable_crate() -> Result<()> {
  let ws = TestWorkspace::new_named("explicit-unpub")?;
  write_release_config(&ws, "")?;

  // Add an unpublishable crate
  add_unpublishable_crate(&ws, "internal-tool", "0.1.0")?;
  ws.commit("Add crates")?;

  // Run release check on the specific crate
  let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "internal-tool"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should succeed and report the crate as not publishable
  assert!(
    output.status.success(),
    "Should succeed when explicitly checking unpublishable crate.\nstdout:\n{}",
    stdout
  );

  assert!(
    stdout.contains("not publishable") || stdout.contains("publish = false"),
    "Should report crate as not publishable.\nstdout:\n{}",
    stdout
  );

  Ok(())
}

/// Test JSON output includes skipped crates
#[test]
fn test_release_check_json_includes_skipped() -> Result<()> {
  let ws = TestWorkspace::new_named("json-skipped")?;
  write_release_config(&ws, "")?;

  // Add publishable and unpublishable crates
  ws.add_crate("lib-pub", "0.1.0", &[])?;
  add_unpublishable_crate(&ws, "lib-internal", "0.1.0")?;
  ws.commit("Add crates")?;

  // Run release check --all --json
  let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--all", "--json"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Parse JSON
  let json: serde_json::Value =
    serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("Should be valid JSON.\nstdout:\n{}", stdout));

  // Should have skipped array
  assert!(
    json.get("skipped").is_some(),
    "JSON should contain 'skipped' field.\nJSON:\n{}",
    serde_json::to_string_pretty(&json).unwrap_or_default()
  );

  // Skipped should contain lib-internal
  let skipped = json["skipped"].as_array().expect("skipped should be array");
  let has_internal = skipped.iter().any(|s| {
    s.get("crate")
      .and_then(|c| c.as_str())
      .map(|c| c == "lib-internal")
      .unwrap_or(false)
  });

  assert!(
    has_internal,
    "Skipped should include lib-internal.\nJSON:\n{}",
    serde_json::to_string_pretty(&json).unwrap_or_default()
  );

  Ok(())
}

/// Test that rail.toml publish = false is respected
#[test]
fn test_release_check_respects_rail_toml_publish_false() -> Result<()> {
  let ws = TestWorkspace::new_named("rail-toml-unpub")?;

  // Add crates (both publishable in Cargo.toml)
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add crates")?;

  // Configure lib-b as non-publishable in rail.toml
  ws.write_release_config(
    r#"source = "commits"
require_clean = false

[crates.lib-b.release]
publish = false
"#,
  )?;

  // Run release check --all
  let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--all"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  // Should succeed
  assert!(
    output.status.success(),
    "Should succeed.\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  // Should show lib-a as ready
  assert!(
    stdout.contains("lib-a: ready"),
    "lib-a should be ready.\nstdout:\n{}",
    stdout
  );

  // Should report lib-b as skipped due to rail.toml
  assert!(
    stderr.contains("lib-b") && stderr.contains("rail.toml"),
    "lib-b should be skipped due to rail.toml.\nstderr:\n{}",
    stderr
  );

  Ok(())
}

#[test]
fn test_release_run_rejects_partial_dependent_closure() -> Result<()> {
  let ws = TestWorkspace::new_named("release-partial-closure")?;
  write_release_config(&ws, "")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate(
    "lib-b",
    "0.1.0",
    &[("lib-a", "{ version = \"^0.1.0\", path = \"../lib-a\" }")],
  )?;
  ws.add_crate(
    "lib-c",
    "0.1.0",
    &[("lib-b", "{ version = \"^0.1.0\", path = \"../lib-b\" }")],
  )?;
  ws.commit("Add release closure crates")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  tag_release(&ws, "lib-b", "0.1.0")?;
  tag_release(&ws, "lib-c", "0.1.0")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
  ws.commit("feat: change lib-a")?;

  let output = run_cargo_rail(&ws.path, &["rail", "release", "run", "lib-a", "--check"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  let combined = format!("{}\n{}", stdout, stderr);

  assert!(
    !output.status.success(),
    "partial subset release should be rejected.\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(
    combined.contains("partial release would leave dependent crate(s) out of sync"),
    "expected partial closure error.\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(
    combined.contains("lib-b") && combined.contains("lib-c"),
    "expected missing dependent closure in error output.\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(
    combined.contains("--include-dependents"),
    "expected opt-in guidance for dependent closure.\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  Ok(())
}

#[test]
fn test_release_run_include_dependents_expands_full_closure() -> Result<()> {
  let ws = TestWorkspace::new_named("release-include-dependents")?;
  write_release_config(&ws, "")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate(
    "lib-b",
    "0.1.0",
    &[("lib-a", "{ version = \"^0.1.0\", path = \"../lib-a\" }")],
  )?;
  ws.add_crate(
    "lib-c",
    "0.1.0",
    &[("lib-b", "{ version = \"^0.1.0\", path = \"../lib-b\" }")],
  )?;
  ws.commit("Add release closure crates")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  tag_release(&ws, "lib-b", "0.1.0")?;
  tag_release(&ws, "lib-c", "0.1.0")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
  ws.commit("feat: change lib-a")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "lib-a", "--check", "--include-dependents"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert_eq!(
    output.status.code(),
    Some(1),
    "check mode should exit with pending changes.\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(
    stdout.contains("lib-a") && stdout.contains("lib-b") && stdout.contains("lib-c"),
    "expected full dependent closure in plan output.\nstdout:\n{}",
    stdout
  );
  let lib_a_idx = stdout.find("1. lib-a").expect("expected lib-a first in release plan");
  let lib_b_idx = stdout.find("2. lib-b").expect("expected lib-b second in release plan");
  let lib_c_idx = stdout.find("3. lib-c").expect("expected lib-c third in release plan");
  assert!(
    lib_a_idx < lib_b_idx && lib_b_idx < lib_c_idx,
    "dependent closure should be released in dependency order.\nstdout:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_subset_release_only_mutates_selected_closure_tags_and_changelogs() -> Result<()> {
  let ws = TestWorkspace::new_named("release-subset-apply")?;
  write_release_config(&ws, "require_release_notes = false")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate(
    "lib-b",
    "0.1.0",
    &[("lib-a", "{ version = \"^0.1.0\", path = \"../lib-a\" }")],
  )?;
  ws.add_crate("lib-c", "0.1.0", &[])?;
  ws.commit("Add release subset crates")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  tag_release(&ws, "lib-b", "0.1.0")?;
  tag_release(&ws, "lib-c", "0.1.0")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
  ws.commit("feat: change lib-a")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--include-dependents",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(
    output.status.success(),
    "subset release should succeed.\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  let lib_a_manifest = std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?;
  let lib_b_manifest = std::fs::read_to_string(ws.path.join("crates/lib-b/Cargo.toml"))?;
  let lib_c_manifest = std::fs::read_to_string(ws.path.join("crates/lib-c/Cargo.toml"))?;
  assert!(lib_a_manifest.contains("version = \"0.1.1\""));
  assert!(lib_b_manifest.contains("version = \"0.1.1\""));
  assert!(lib_b_manifest.contains("^0.1.1"));
  assert!(lib_c_manifest.contains("version = \"0.1.0\""));

  let tags = String::from_utf8_lossy(&git(&ws.path, &["tag", "--list"])?.stdout).to_string();
  assert!(tags.contains("lib-a-v0.1.1"), "missing lib-a tag.\ntags:\n{}", tags);
  assert!(tags.contains("lib-b-v0.1.1"), "missing lib-b tag.\ntags:\n{}", tags);
  assert!(
    !tags.contains("lib-c-v0.1.1"),
    "unrelated crate should not be tagged.\ntags:\n{}",
    tags
  );

  assert!(ws.path.join("crates/lib-a/CHANGELOG.md").exists());
  assert!(ws.path.join("crates/lib-b/CHANGELOG.md").exists());
  assert!(
    !ws.path.join("crates/lib-c/CHANGELOG.md").exists(),
    "unrelated crate should not get a changelog"
  );

  Ok(())
}

#[test]
fn test_release_run_apply_supports_publish_false_from_rail_toml() -> Result<()> {
  let ws = TestWorkspace::new_named("release-run-publish-false")?;
  ws.add_crate("internal-tool", "0.1.0", &[])?;
  ws.commit("Add internal-tool")?;
  ws.write_release_config(
    r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
source = "commits"
require_clean = false
require_release_notes = false

[crates.internal-tool.release]
publish = false
"#,
  )?;
  tag_release(&ws, "internal-tool", "0.1.0")?;
  ws.modify_file("internal-tool", "src/lib.rs", "pub fn changed() {}\n")?;
  ws.commit("feat: update internal-tool")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "release", "run", "internal-tool", "--bump", "patch", "--yes"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  let combined = format!("{}\n{}", stdout, stderr);

  assert!(
    output.status.success(),
    "publish = false release should succeed without crates.io publish.\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(
    combined.contains("skipped publish (publish = false)"),
    "expected publish = false skip message.\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(ws.path.join("crates/internal-tool/CHANGELOG.md").exists());
  let tags = String::from_utf8_lossy(&git(&ws.path, &["tag", "--list"])?.stdout).to_string();
  assert!(tags.contains("v0.1.1"));

  Ok(())
}

#[test]
fn test_subset_release_updates_shared_workspace_dependency_versions() -> Result<()> {
  let ws = TestWorkspace::new_named("release-workspace-deps")?;
  write_release_config(&ws, "require_release_notes = false")?;

  ws.add_crate("lib-a", "0.1.0", &[])?;
  add_workspace_dependency(&ws, "lib-a", "0.1.0")?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", "{ workspace = true }")])?;
  ws.commit("Add workspace dependency crates")?;
  tag_release(&ws, "lib-a", "0.1.0")?;
  tag_release(&ws, "lib-b", "0.1.0")?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
  ws.commit("feat: change lib-a")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "release",
      "run",
      "lib-a",
      "--include-dependents",
      "--bump",
      "patch",
      "--skip-publish",
      "--yes",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(
    output.status.success(),
    "subset release with workspace dependencies should succeed.\nstdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );

  let root_manifest = std::fs::read_to_string(ws.path.join("Cargo.toml"))?;
  let lib_b_manifest = std::fs::read_to_string(ws.path.join("crates/lib-b/Cargo.toml"))?;
  assert!(
    root_manifest.contains("lib-a = { version = \"0.1.1\", path = \"crates/lib-a\" }"),
    "workspace dependency should be bumped.\nCargo.toml:\n{}",
    root_manifest
  );
  assert!(
    lib_b_manifest.contains("version = \"0.1.1\""),
    "dependent crate should be version bumped as part of the approved closure.\nCargo.toml:\n{}",
    lib_b_manifest
  );
  assert!(
    lib_b_manifest.contains("lib-a = { workspace = true }"),
    "workspace dependency declaration should remain workspace-based.\nCargo.toml:\n{}",
    lib_b_manifest
  );

  Ok(())
}
