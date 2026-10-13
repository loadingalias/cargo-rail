//! Integration tests for the release check command

use super::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;

fn write_publication_config(ws: &TestWorkspace) -> Result<()> {
    std::fs::write(
        ws.path.join(".config/rail.toml"),
        r#"[release]
remote_effects = "push"
registry_publication = "crates-io"
"#,
    )?;
    Ok(())
}

fn add_release_intent(ws: &TestWorkspace, crate_names: &[&str], bump: &str) -> Result<()> {
    let entries = crate_names
        .iter()
        .map(|crate_name| format!("\"{}\" = \"{bump}\"", crate_name))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::create_dir_all(ws.path.join(".changes"))?;
    std::fs::write(
        ws.path.join(".changes/check-coverage.md"),
        format!("---\n{}\n---\n\nReviewed release intent.\n", entries),
    )?;
    Ok(())
}

/// Assert one exact stdout line, so a crate name in an unrelated line cannot satisfy it.
fn assert_line(stdout: &str, expected: &str) {
    assert!(
        stdout.lines().any(|line| line == expected),
        "missing line {expected:?}.\nstdout:\n{stdout}"
    );
}

/// Test check command validates crate exists
#[test]
fn test_check_validates_crate_exists() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("check-exists")?;

        // Add a crate without path deps (use workspace deps)
        ws.add_crate("real-crate", "0.1.0", &[("anyhow", "{ workspace = true }")])?;

        // Configure release
        write_publication_config(&ws)?;

        ws.commit("Add real-crate with release config")?;

        // Check for non-existent crate should fail
        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--publication", "nonexistent"])?;
        assert!(!output.status.success(), "check for nonexistent crate should fail");

        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Crate 'nonexistent' not found. Available workspace crates: real-crate"),
            "Should name the missing crate and the available ones. stderr: {}",
            stderr
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test check command passes for valid crate
#[test]
fn test_check_passes_for_valid_crate() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("check-valid")?;

        // Add a simple publishable crate using workspace deps (no path deps)
        ws.add_crate("valid-crate", "0.1.0", &[("anyhow", "{ workspace = true }")])?;

        // Configure release
        write_publication_config(&ws)?;

        add_release_intent(&ws, &["valid-crate"], "patch")?;
        ws.commit("Add valid-crate with release config")?;

        // A pending release that passes publication validation exits 1.
        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--publication", "valid-crate"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert_eq!(
            output.status.code(),
            Some(1),
            "check should report a pending release. stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_line(&stdout, "valid-crate: ready for crates-io publication");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test check command with --all flag
#[test]
fn test_check_all_crates() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("check-all")?;

        // Add multiple simple crates with workspace deps
        ws.add_crate("crate-a", "0.1.0", &[("anyhow", "{ workspace = true }")])?;
        ws.add_crate("crate-b", "0.1.0", &[("serde", "{ workspace = true }")])?;

        // Configure release
        write_publication_config(&ws)?;

        add_release_intent(&ws, &["crate-a", "crate-b"], "patch")?;
        ws.commit("Add crates with release config")?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--publication", "--all"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert_eq!(
            output.status.code(),
            Some(1),
            "check --all should report a pending release. stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_line(&stdout, "crate-a: ready for crates-io publication");
        assert_line(&stdout, "crate-b: ready for crates-io publication");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test check remains a read-only preview when the worktree is dirty.
#[test]
fn test_check_tolerates_dirty_preview() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("check-dirty")?;

        // Add a crate
        ws.add_crate("dirty-crate", "0.1.0", &[("anyhow", "{ workspace = true }")])?;

        // Configure release policy.
        write_publication_config(&ws)?;

        add_release_intent(&ws, &["dirty-crate"], "none")?;
        ws.commit("Add dirty-crate with config")?;

        // Create uncommitted change
        std::fs::write(
            ws.path.join("crates/dirty-crate/src/lib.rs"),
            "pub fn hello() { /* modified */ }",
        )?;

        // Preview should succeed. Exact dirt is enforced against the bound release
        // plan at the first write, not by this read-only command.
        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--publication", "dirty-crate"])?;
        assert!(
            output.status.success(),
            "read-only release check should tolerate dirt.\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test check reports status for crate with publish = false
///
/// When explicitly checking an unpublishable crate, the command succeeds
/// but reports the crate as "not publishable" rather than erroring.
#[test]
fn test_check_reports_unpublishable_crate_status() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("check-unpublish")?;

        // Add a crate with publish = false
        let crate_path = ws.path.join("crates/private-crate");
        std::fs::create_dir_all(&crate_path)?;
        std::fs::create_dir_all(crate_path.join("src"))?;
        std::fs::write(
            crate_path.join("Cargo.toml"),
            r#"[package]
name = "private-crate"
version = "0.1.0"
edition = "2021"
license = "MIT"
publish = false

[dependencies]
"#,
        )?;
        std::fs::write(crate_path.join("src/lib.rs"), "pub fn hello() {}")?;

        // Configure release
        write_publication_config(&ws)?;

        ws.commit("Add private-crate with config")?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        std::fs::write(
            ws.path.join(".changes/private-crate.md"),
            "---\n\"private-crate\" = \"patch\"\n---\n\nRelease the private crate.\n",
        )?;

        // Check should succeed and report the crate as not publishable
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "check", "--publication", "private-crate"],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert_eq!(
            output.status.code(),
            Some(1),
            "pending release check should exit 1.\nstdout:\n{}\nstderr:\n{}",
            stdout,
            String::from_utf8_lossy(&output.stderr)
        );

        // Should report crate as not publishable
        assert_line(
            &stdout,
            "private-crate: not publishable (publish = false in Cargo.toml)",
        );
        assert!(
            stdout.contains("no selected crate publishes to crates.io"),
            "a plan that publishes nothing must not claim publication readiness.\nstdout:\n{stdout}"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Publication readiness requires explicit remote-effect authority in configuration.
#[test]
fn test_check_requires_release_config() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("check-no-config")?;
        ws.add_crate("some-crate", "0.1.0", &[])?;
        add_release_intent(&ws, &["some-crate"], "patch")?;
        ws.commit("Add some-crate without release authority")?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--publication", "some-crate"])?;
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("--publication cannot be combined with release.remote_effects = \"none\""),
            "the error must name the flag the user passed. stderr: {stderr}"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test check command requires crate name or --all
#[test]
fn test_check_requires_crate_or_all() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("check-no-args")?;

        // Add a crate
        ws.add_crate("any-crate", "0.1.0", &[])?;

        // Configure release
        write_publication_config(&ws)?;

        ws.commit("Add any-crate with config")?;

        // Check with no args should fail
        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--publication"])?;
        assert!(!output.status.success(), "check with no args should fail");

        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("must specify crate name(s) or --all"),
            "Should require a crate name or --all. stderr: {}",
            stderr
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}
