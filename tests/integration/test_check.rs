//! Integration tests for `cargo rail release --dry-run` selection and preconditions

use super::helpers::{TestWorkspace, cargo_command, run_cargo_rail};
use anyhow::Result;

fn write_release_config(ws: &TestWorkspace, extras: &str) -> Result<()> {
    std::fs::write(
        ws.path.join(".config/rail.toml"),
        format!("[release]\nsemver_check = \"off\"\n{extras}"),
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

/// Commit the fixture with its Cargo.lock and publish the branch and tags to `origin`.
fn publish_to_origin(ws: &TestWorkspace, message: &str) -> Result<()> {
    let lockfile = cargo_command(&ws.path)
        .args(["generate-lockfile", "--offline"])
        .output()?;
    anyhow::ensure!(lockfile.status.success(), "{lockfile:?}");
    ws.commit(message)?;
    ws.with_origin()?;
    Ok(())
}

/// Assert one exact stdout line, so a crate name in an unrelated line cannot satisfy it.
fn assert_line(stdout: &str, expected: &str) {
    assert!(
        stdout.lines().any(|line| line == expected),
        "missing line {expected:?}.\nstdout:\n{stdout}"
    );
}

/// Test that a release validates the named crate exists
#[test]
fn test_check_validates_crate_exists() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("check-exists")?;
        ws.add_crate("real-crate", "0.1.0", &[])?;
        write_release_config(&ws, "publish = \"none\"\n")?;
        publish_to_origin(&ws, "Add real-crate with release config")?;

        // A dry run for a non-existent crate should fail
        let output = run_cargo_rail(&ws.path, &["rail", "release", "--dry-run", "nonexistent"])?;
        assert!(!output.status.success(), "a release of a nonexistent crate should fail");

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

/// A dry run runs every precondition of a release, including a clean worktree.
#[test]
fn test_dry_run_requires_a_clean_worktree() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("check-dirty")?;
        ws.add_crate("dirty-crate", "0.1.0", &[])?;
        write_release_config(&ws, "publish = \"none\"\n")?;
        add_release_intent(&ws, &["dirty-crate"], "patch")?;
        publish_to_origin(&ws, "Add dirty-crate with config")?;

        // Create uncommitted change
        std::fs::write(
            ws.path.join("crates/dirty-crate/src/lib.rs"),
            "pub fn hello() { /* modified */ }",
        )?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "--dry-run", "dirty-crate"])?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(
            stderr.contains("a release starts from a clean worktree")
                && stderr.contains("crates/dirty-crate/src/lib.rs"),
            "a dry run must name the uncommitted change.\nstderr:\n{stderr}"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test a dry run reports an unpublishable crate without publishing it
///
/// When explicitly naming an unpublishable crate, the release succeeds
/// and reports that the crate is not published rather than erroring.
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

        // Crates.io is the default registry; Cargo's publish = false keeps this crate off it.
        write_release_config(&ws, "")?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        std::fs::write(
            ws.path.join(".changes/private-crate.md"),
            "---\n\"private-crate\" = \"patch\"\n---\n\nRelease the private crate.\n",
        )?;
        publish_to_origin(&ws, "Add private-crate with config")?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "--dry-run", "private-crate"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert_eq!(
            output.status.code(),
            Some(0),
            "a dry run exits 0 when every check passes.\nstdout:\n{}\nstderr:\n{}",
            stdout,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_line(&stdout, "    registry       not published");
        assert!(
            !stdout.contains("publishes to"),
            "a plan that publishes nothing must not claim publication.\nstdout:\n{stdout}"
        );

        let json = run_cargo_rail(
            &ws.path,
            &["rail", "release", "--dry-run", "private-crate", "--format", "json"],
        )?;
        assert_eq!(json.status.code(), Some(0), "{json:?}");
        let json: serde_json::Value = serde_json::from_slice(&json.stdout)?;
        assert_eq!(json["crates"][0]["name"], "private-crate");
        assert_eq!(json["crates"][0]["registry"], "skipped");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Without crate names, a release selects every crate with release-worthy changes
#[test]
fn test_dry_run_without_crate_names_selects_every_changed_crate() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("check-no-args")?;
        ws.add_crate("any-crate", "0.1.0", &[])?;
        ws.add_crate("other-crate", "0.1.0", &[])?;
        write_release_config(&ws, "publish = \"none\"\n")?;
        publish_to_origin(&ws, "Add crates with config")?;
        ws.tag("any-crate-v0.1.0", "Release any-crate")?;
        ws.tag("other-crate-v0.1.0", "Release other-crate")?;
        ws.push()?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "--dry-run"])?;
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("nothing to release"),
            "{output:?}"
        );

        add_release_intent(&ws, &["any-crate"], "patch")?;
        ws.commit("Review any-crate")?;
        ws.push()?;
        let output = run_cargo_rail(&ws.path, &["rail", "release", "--dry-run", "--format", "json"])?;
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let names = json["crates"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry["name"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["any-crate"], "{json}");
        assert_eq!(json["skipped"][0]["name"], "other-crate", "{json}");

        Ok(())
    })();
    super::helpers::finish_test(result);
}
