//! Integration tests for release + changelog generation
//!
//! Covers:
//! - Tag pattern detection ({crate}-v*)
//! - Compare URLs with GitHub remote
//! - Commit/PR links and breaking markers
//! - skip_changelog_for and require_changelog_entries flags

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;

fn write_release_config(ws: &TestWorkspace, extras: &str) -> Result<()> {
  ws.write_release_config(&format!(
    r#"tag_prefix = "v"
tag_format = "{{crate}}-v{{version}}"
skip_changelog_for = []
require_changelog_entries = false
require_clean = false
{}
"#,
    extras
  ))?;
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
  let output = run_cargo_rail(&ws.path, &["rail", "release", "--dry-run", "--bump", "patch"])?;
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
    &["rail", "release", "lib-a", "--bump", "patch", "--skip-publish"],
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
    "skip_changelog_for = [\"internal\"]\nrequire_changelog_entries = true",
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
      "publish",
      "--all",
      "--bump",
      "patch",
      "--skip-publish",
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
