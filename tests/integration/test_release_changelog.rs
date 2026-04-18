//! Integration tests for release + changelog generation
//!
//! Covers:
//! - Tag pattern detection ({crate}-v*)
//! - Compare URLs with GitHub remote
//! - Commit/PR links and breaking markers
//! - skip_changelog_for and require_changelog_entries flags

use crate::helpers::{TestWorkspace, git, run_cargo_rail};
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
skip_changelog_for = []
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
skip_changelog_for = []
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

/// Test release --json output format
#[test]
fn test_release_json_output() -> Result<()> {
  let ws = TestWorkspace::new_single_crate("json-release", "0.1.0")?;

  // Configure release
  ws.write_release_config("require_clean = false\n")?;

  // Run release plan with --json
  let output = run_cargo_rail(&ws.path, &["rail", "release", "run", "--check", "--json"])?;
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

// changelog_relative_to Tests
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

  // Should show the actual branch name in next steps (progress output goes to stderr)
  assert!(
    stderr.contains("git push origin hotfix-1.0"),
    "Next steps should show actual branch name.\nstderr:\n{}",
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
    r#"require_clean = false

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
require_clean = false
require_release_notes = false

[crates.internal-tool.release]
publish = false
"#,
  )?;
  tag_release(&ws, "internal-tool", "0.1.0")?;
  ws.modify_file("internal-tool", "src/lib.rs", "pub fn changed() {}\n")?;
  ws.commit("feat: update internal-tool")?;

  let output = run_cargo_rail(&ws.path, &["rail", "release", "run", "internal-tool", "--yes"])?;
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
