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
    &["rail", "release", "run", "lib-a", "--bump", "patch", "--skip-publish"],
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
    &["rail", "release", "run", "--all", "--bump", "patch", "--skip-publish"],
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

/// Test release --json output format
#[test]
fn test_release_json_output() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("json-release", "0.1.0")?;

  // Configure release
  ws.write_release_config("require_clean = false\n")?;

  // Run release plan with --json
  let output = run_cargo_rail(&ws.path, &["rail", "release", "run", "--check", "--json"])?;

  if output.status.success() {
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should be valid JSON
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
    assert!(
      parsed.is_ok(),
      "release --json should output valid JSON. stdout: {}",
      stdout
    );
  }

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

// ============================================================================
// changelog_relative_to Tests (Issue #19)
// ============================================================================

/// Test default changelog_relative_to behavior (crate-relative, backward compatible)
#[test]
fn test_changelog_relative_to_crate_default() -> Result<()> {
  let ws = TestWorkspace::new_named("changelog-crate-rel")?;
  ws.set_remote("git@github.com:org/repo.git")?;

  // Don't set changelog_relative_to - should default to "crate"
  ws.write_release_config(
    r#"require_clean = false
changelog_path = "CHANGELOG.md"
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
    &["rail", "release", "run", "lib-a", "--bump", "patch", "--skip-publish"],
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

/// Test changelog_relative_to = "workspace" creates changelog at workspace root
#[test]
fn test_changelog_relative_to_workspace() -> Result<()> {
  let ws = TestWorkspace::new_named("changelog-ws-rel")?;
  ws.set_remote("git@github.com:org/repo.git")?;

  // Explicitly set changelog_relative_to = "workspace"
  ws.write_release_config(
    r#"require_clean = false
changelog_path = "CHANGELOG.md"
changelog_relative_to = "workspace"
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
    &["rail", "release", "run", "lib-a", "--bump", "patch", "--skip-publish"],
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

/// Test that parent directories are auto-created for changelog paths
#[test]
fn test_changelog_parent_directories_auto_created() -> Result<()> {
  let ws = TestWorkspace::new_named("changelog-auto-mkdir")?;
  ws.set_remote("git@github.com:org/repo.git")?;

  // Use a nested path that doesn't exist
  ws.write_release_config(
    r#"require_clean = false
changelog_path = "docs/changelogs/CHANGELOG.md"
changelog_relative_to = "workspace"
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
    &["rail", "release", "run", "lib-a", "--bump", "patch", "--skip-publish"],
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

/// Test changelog_relative_to = "crate" with custom path creates in crate subdir
#[test]
fn test_changelog_relative_to_crate_custom_path() -> Result<()> {
  let ws = TestWorkspace::new_named("changelog-crate-custom")?;
  ws.set_remote("git@github.com:org/repo.git")?;

  // Use custom path with crate-relative
  ws.write_release_config(
    r#"require_clean = false
changelog_path = "docs/CHANGES.md"
changelog_relative_to = "crate"
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
    &["rail", "release", "run", "lib-a", "--bump", "patch", "--skip-publish"],
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

// ============================================================================
// Prerelease Bump Tests
// ============================================================================

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

// ============================================================================
// Extended Check Tests
// ============================================================================

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
  assert!(
    json.get("extended").is_some(),
    "JSON should contain 'extended' field.\nJSON:\n{}",
    serde_json::to_string_pretty(&json).unwrap_or_default()
  );

  Ok(())
}

// ============================================================================
// Release Safety Tests (Branch Detection)
// ============================================================================

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

  // Should show the actual branch name in next steps
  assert!(
    stdout.contains("git push origin hotfix-1.0"),
    "Next steps should show actual branch name.\nstdout:\n{}",
    stdout
  );

  Ok(())
}
