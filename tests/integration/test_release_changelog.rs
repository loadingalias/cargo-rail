//! Integration tests for release + changelog generation
//!
//! Covers:
//! - Tag pattern detection ({crate}-v*)
//! - Compare URLs with GitHub remote
//! - Commit/PR links and breaking markers
//! - per-crate changelog skip and require_changelog_entries flags

#[cfg(unix)]
use crate::helpers::isolated_cargo_rail_command;
use crate::helpers::{NestedWorkspace, TestWorkspace, cargo_command, cargo_rail_command, git, run_cargo_rail};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;

fn generate_lockfile(workspace: &Path) -> Result<()> {
    let output = cargo_command(workspace)
        .args(["generate-lockfile", "--manifest-path", "Cargo.toml"])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "Cargo.lock generation failed in '{}': {}",
        workspace.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    anyhow::ensure!(
        workspace.join("Cargo.lock").is_file(),
        "Cargo succeeded without creating '{}'",
        workspace.join("Cargo.lock").display()
    );
    Ok(())
}

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
            clone_path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("shallow clone path is not UTF-8"))?,
        ])
        .output()?;
    anyhow::ensure!(
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
    Ok(cargo_rail_command(cwd)?.env(variable, fault).args(args).output()?)
}

fn only_release_state(workspace: &Path) -> Result<PathBuf> {
    std::fs::read_dir(workspace.join("target/cargo-rail/releases"))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| path.extension().is_some_and(|extension| extension == "json"))
        .ok_or_else(|| anyhow::anyhow!("missing release state"))
}

fn add_auxiliary_cargo_workspace(ws: &TestWorkspace, name: &str, dependency: &str) -> Result<PathBuf> {
    add_auxiliary_cargo_workspace_with_dependencies(ws, name, &[(dependency, "..")])
}

fn add_auxiliary_cargo_workspace_with_dependencies(
    ws: &TestWorkspace,
    name: &str,
    dependencies: &[(&str, &str)],
) -> Result<PathBuf> {
    let root = ws.path.join(name);
    std::fs::create_dir_all(root.join("src"))?;
    let dependencies = dependencies
        .iter()
        .map(|(dependency, path)| format!("{dependency} = {{ path = {} }}\n", toml_edit::Value::from(*path)))
        .collect::<String>();
    std::fs::write(
        root.join("Cargo.toml"),
        format!(
            r#"[package]
name = "{name}"
version = "0.0.0"
edition = "2021"
publish = false

[workspace]

[dependencies]
{dependencies}
"#,
        ),
    )?;
    std::fs::write(root.join("src/lib.rs"), "pub fn auxiliary() {}\n")?;
    generate_lockfile(&root)?;
    Ok(root)
}

fn configured_auxiliary_release(crate_name: &str) -> Result<TestWorkspace> {
    let ws = TestWorkspace::new_single_crate(crate_name, "0.1.0")?;
    add_auxiliary_cargo_workspace(&ws, "aux", crate_name)?;
    ws.write_release_config(
        r#"source = "both"
tag_format = "v{version}"
require_release_notes = false
auxiliary_cargo_manifests = ["aux/Cargo.toml"]
"#,
    )?;
    ws.commit("Configure auxiliary Cargo release projection")?;
    Ok(ws)
}

fn check_auxiliary_release(ws: &TestWorkspace) -> Result<std::process::Output> {
    run_cargo_rail(
        &ws.path,
        &[
            "rail",
            "release",
            "run",
            "--all",
            "--bump",
            "patch",
            "--check",
            "--skip-publish",
            "--skip-tag",
        ],
    )
}

fn assert_only_crlf(path: &Path) -> Result<()> {
    let bytes = std::fs::read(path)?;
    anyhow::ensure!(
        bytes.windows(2).any(|window| window == b"\r\n"),
        "{} was not CRLF",
        path.display()
    );
    anyhow::ensure!(
        bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| *byte != b'\n' || index > 0 && bytes[index - 1] == b'\r'),
        "{} contains a non-CRLF newline",
        path.display()
    );
    Ok(())
}

fn assert_external_auxiliary_dependency_rejected(absolute: bool) -> Result<()> {
    let ws = TestWorkspace::new_single_crate(if absolute { "aux-absolute" } else { "aux-escaping" }, "0.1.0")?;
    let external = tempfile::TempDir::new_in(
        ws.path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("test workspace has no parent"))?,
    )?;
    std::fs::create_dir_all(external.path().join("src"))?;
    std::fs::write(
        external.path().join("Cargo.toml"),
        r#"[package]
name = "external-path-package"
version = "0.1.0"
edition = "2021"
"#,
    )?;
    std::fs::write(external.path().join("src/lib.rs"), "pub fn external() {}\n")?;
    let dependency_path = if absolute {
        external.path().to_path_buf()
    } else {
        PathBuf::from("../..").join(
            external
                .path()
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("external package has no file name"))?,
        )
    };
    let dependency_path = dependency_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("external package path is not UTF-8"))?;
    add_auxiliary_cargo_workspace_with_dependencies(&ws, "aux", &[("external-path-package", dependency_path)])?;
    ws.write_release_config(
        r#"source = "both"
tag_format = "v{version}"
require_release_notes = false
auxiliary_cargo_manifests = ["aux/Cargo.toml"]
"#,
    )?;
    let initial_head = ws.commit("Configure external auxiliary path dependency")?;

    let check = check_auxiliary_release(&ws)?;
    anyhow::ensure!(
        !check.status.success(),
        "release check unexpectedly accepted an external auxiliary dependency\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let stderr = String::from_utf8_lossy(&check.stderr);
    if absolute {
        anyhow::ensure!(
            stderr.contains("outside the captured source") && stderr.contains(&external.path().display().to_string()),
            "{stderr}"
        );
    } else {
        anyhow::ensure!(stderr.contains("cargo metadata --locked failed"), "{stderr}");
    }
    let final_head = git(&ws.path, &["rev-parse", "HEAD"])?.stdout;
    anyhow::ensure!(
        final_head == format!("{initial_head}\n").as_bytes(),
        "release check moved HEAD from {initial_head} to {}",
        String::from_utf8_lossy(&final_head).trim()
    );
    Ok(())
}

fn push_release_workspace(crate_name: &str) -> Result<(TestWorkspace, tempfile::TempDir)> {
    let ws = TestWorkspace::new_single_crate(crate_name, "0.1.0")?;
    let remote = tempfile::TempDir::new()?;
    git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
    ws.set_remote(
        remote
            .path()
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("bare release remote path is not UTF-8"))?,
    )?;
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
fn release_plan_works_on_single_crate_repo() {
    let result: Result<()> = (|| {
        // Test that release plan works on a split repo (single-crate, non-workspace)
        let ws = TestWorkspace::new_single_crate("private-tool", "0.1.0")?;

        // Add release config (what a split repo would have)
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "v{version}"
source = "commits"
require_clean = false
require_release_notes = false
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_source_defaults_to_reviewed_changes_only() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_commit_source_is_explicit_compatibility_mode() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn no_release_change_intent_satisfies_default_coverage_without_a_bump() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_retains_unconsumed_no_release_intent_from_a_shared_file() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn reviewed_changes_require_repository_wide_coverage_by_default() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_apply_accepts_the_untracked_change_entry_bound_by_its_plan() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_abort_restores_untracked_reviewed_input_after_a_local_fault() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_apply_rejects_unrelated_dirt_before_the_first_write() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plan_auto_infers_bumps_per_crate() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plan_auto_honors_pre_1_major_policy() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plan_auto_respects_changelog_path_filters() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
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

    let path = format!(
        "{}:{}",
        shim_dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = cargo_rail_command(&ws.path)?.env("PATH", path).args(args).output()?;
    Ok(output)
}

#[cfg(unix)]
fn run_with_gh_shim(ws: &TestWorkspace, gh_script: &Path, args: &[&str]) -> Result<std::process::Output> {
    run_with_path_prefix(
        ws,
        gh_script
            .parent()
            .ok_or_else(|| anyhow::anyhow!("GitHub shim has no parent directory"))?,
        args,
    )
}

#[cfg(unix)]
fn run_with_path_prefix(ws: &TestWorkspace, prefix: &Path, args: &[&str]) -> Result<std::process::Output> {
    let path = format!("{}:{}", prefix.display(), std::env::var("PATH").unwrap_or_default());
    let mut command = if matches!(args.get(1), Some(&"cache" | &"clean")) {
        isolated_cargo_rail_command(&ws.path)?
    } else {
        cargo_rail_command(&ws.path)?
    };
    command.env("PATH", path).args(args).output().map_err(Into::into)
}

#[cfg(unix)]
fn registry_shadow_cargo_shim(log_path: &Path, published_path: &Path) -> Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt;

    let real_cargo = Command::new("sh").args(["-c", "command -v cargo"]).output()?;
    let real_cargo = String::from_utf8_lossy(&real_cargo.stdout).trim().to_string();
    let real_git = Command::new("sh").args(["-c", "command -v git"]).output()?;
    let real_git = String::from_utf8_lossy(&real_git.stdout).trim().to_string();
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

    let git_path = dir.path().join("git");
    let remote_head = dir.path().join("remote-head");
    let remote_tags = dir.path().join("remote-tags");
    std::fs::write(
        &git_path,
        format!(
            r#"#!/bin/sh
case " $* " in
  *" remote get-url "*)
    echo "https://github.com/loadingalias/registry-shadow.git"
    exit 0
    ;;
  *" ls-remote "*)
    ref=""
    for argument in "$@"; do ref="$argument"; done
    case "$ref" in
      refs/heads/*)
        [ -f "{}" ] && printf '%s\t%s\n' "$(cat "{}")" "$ref"
        ;;
      refs/tags/*)
        [ -f "{}" ] && printf '%s\t%s\n' "$(cat "{}")" "$ref"
        ;;
    esac
    exit 0
    ;;
  *" push "*)
    repository=.
    previous=""
    for argument in "$@"; do
      [ "$previous" = "-C" ] && repository="$argument"
      previous="$argument"
    done
    head=$("{}" -C "$repository" rev-parse HEAD)
    case " $* " in
      *" refs/tags/"*) printf '%s\n' "$head" > "{}" ;;
      *) printf '%s\n' "$head" > "{}" ;;
    esac
    exit 0
    ;;
esac
exec "{}" "$@"
"#,
            remote_head.display(),
            remote_head.display(),
            remote_tags.display(),
            remote_tags.display(),
            real_git,
            remote_tags.display(),
            remote_head.display(),
            real_git,
        ),
    )?;
    let mut perms = std::fs::metadata(&git_path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&git_path, perms)?;

    let gh_path = dir.path().join("gh");
    std::fs::write(
        &gh_path,
        r#"#!/bin/sh
if [ "$1" = "api" ] && [ "$2" = "graphql" ]; then
  printf '%s\n' '{"data":{"repository":{"object":{"statusCheckRollup":{"contexts":{"totalCount":1,"checkRunCount":1,"checkRunCountsByState":[{"state":"SUCCESS","count":1}],"statusContextCount":0,"statusContextCountsByState":[]}}}}}}'
fi
exit 0
"#,
    )?;
    let mut perms = std::fs::metadata(&gh_path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&gh_path, perms)?;
    Ok(dir)
}

#[cfg(unix)]
#[test]
fn release_publish_ignores_local_workspace_shadow_and_targets_crates_io() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("registry-shadow", "0.1.0")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
require_changelog_entries = false
require_clean = false
require_release_notes = false
semver_check = "off"
sign_tags = false
publish_delay = 1
remote_effects = "push"
registry_publication = "crates-io"
"#,
        )?;
        ws.set_remote("https://github.com/loadingalias/registry-shadow.git")?;
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
        let interrupted = cargo_rail_command(&ws.path)?
            .env("PATH", &path)
            .env(
                "CARGO_RAIL_RELEASE_FAIL_AFTER",
                "journal:publish_intent:registry-shadow",
            )
            .args([
                "rail",
                "release",
                "run",
                "registry-shadow",
                "--bump",
                "patch",
                "--publish",
                "--yes",
            ])
            .output()?;
        assert!(!interrupted.status.success());
        assert!(
            !published_path.exists(),
            "journal failure precedes the publication effect"
        );
        anyhow::ensure!(
            ws.path.join("target/cargo-rail/releases").is_dir(),
            "release failed before journal creation\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&interrupted.stdout),
            String::from_utf8_lossy(&interrupted.stderr)
        );
        let state_path = only_release_state(&ws.path)?;
        let output = run_with_path_prefix(
            &ws,
            shim.path(),
            &["rail", "release", "resume", state_path.to_str().unwrap()],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::ensure!(
            output.status.success(),
            "release should publish despite an unqualified local lookup succeeding\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );

        let log = std::fs::read_to_string(&log_path)?;
        assert!(
            log.lines()
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
        let state: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
        assert_eq!(state["schema_version"], 5);
        assert_eq!(state["plan"]["plan_contract_version"], 5);
        assert_eq!(state["publish_registry"], "crates-io");
        assert_eq!(state["release_config"]["registry_publication"], "crates-io");
        assert_eq!(
            state["crates"][0]["publication"]["object"],
            "crates-io:registry-shadow@0.1.1"
        );

        let transaction_id = state["transaction_id"].as_str().unwrap();
        let cleaned = run_with_path_prefix(
            &ws,
            shim.path(),
            &["rail", "clean", "--release-journal", transaction_id],
        )?;
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_package_excludes_finder_metadata() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
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
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
  exit 0
fi
if [ "$1" = "api" ] && [ "$2" = "graphql" ]; then
  echo '{{"data":{{"repository":{{"object":{{"statusCheckRollup":{{"contexts":{{"totalCount":1,"checkRunCount":1,"checkRunCountsByState":[{{"state":"SUCCESS","count":1}}],"statusContextCount":0,"statusContextCountsByState":[]}}}}}}}}}}}}'
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
        anyhow::ensure!(
            output.status.success(),
            "could not locate required test binary {}",
            binary
        );
        let real = String::from_utf8_lossy(&output.stdout).trim().to_string();
        symlink(real, dir.path().join(binary))?;
    }

    cargo_rail_command(&ws.path)?
        .env("PATH", dir.path())
        .env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
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
fn release_plan_blocks_when_semver_checks_exceeds_reviewed_intent() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_plan_accepts_semver_breakage_covered_by_reviewed_major_intent() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_plan_auto_ignores_inconclusive_semver_checks() {
    let result: Result<()> = (|| {
        // A non-zero exit without the breaking-summary marker is an operational
        // failure (first release: no baseline on crates.io) — never an escalation.
        let ws = semver_shim_workspace("release-auto-semver-inconclusive")?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        std::fs::write(
            ws.path.join(".changes/no-release.md"),
            "---\n\"lib-a\" = \"none\"\n---\n\nReviewed documentation-only change.\n",
        )?;

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
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_plan_auto_skips_semver_checks_for_unpublishable_crates() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plan_auto_reports_skipped_crates_with_reason() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plan_auto_noops_when_all_crates_are_skipped() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plan_does_not_print_removed_publish_delay() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plans_and_commits_exact_auxiliary_cargo_lockfiles() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-release", "0.1.0")?;
        add_auxiliary_cargo_workspace(&ws, "aux-one", "aux-release")?;
        add_auxiliary_cargo_workspace(&ws, "aux-two", "aux-release")?;
        ws.write_release_config(
            r#"source = "both"
tag_format = "v{version}"
require_release_notes = false
auxiliary_cargo_manifests = ["aux-one/Cargo.toml", "aux-two/Cargo.toml"]
"#,
        )?;
        ws.commit("Add auxiliary Cargo release projections")?;

        let check = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--check",
                "--skip-publish",
                "--skip-tag",
                "--format",
                "json",
            ],
        )?;
        assert_eq!(
            check.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
        let check: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        assert_eq!(check["release_plan"]["plan_contract_version"], 5);
        let projections = check["release_plan"]["auxiliary_lockfiles"]
            .as_array()
            .expect("auxiliary lockfile projections");
        assert_eq!(projections.len(), 2);
        for (index, name) in ["aux-one", "aux-two"].into_iter().enumerate() {
            assert_eq!(projections[index]["manifest_path"], format!("{name}/Cargo.toml"));
            assert_eq!(projections[index]["lockfile_path"], format!("{name}/Cargo.lock"));
            assert!(
                projections[index]["before_digest"]
                    .as_str()
                    .is_some_and(|digest| digest.starts_with("sha256:"))
            );
            assert!(
                projections[index]["after_digest"]
                    .as_str()
                    .is_some_and(|digest| digest.starts_with("sha256:"))
            );
            assert_ne!(projections[index]["before_digest"], projections[index]["after_digest"]);
        }
        let auxiliary_actions = check["mutation_plan"]["actions"]
            .as_array()
            .expect("mutation actions")
            .iter()
            .filter(|action| action["code"] == "UPDATE_AUXILIARY_LOCKFILE")
            .collect::<Vec<_>>();
        assert_eq!(auxiliary_actions.len(), 2);
        assert_eq!(
            auxiliary_actions[0]["expected_mutations"][0]["path"],
            "aux-one/Cargo.lock"
        );

        let before = std::fs::read(ws.path.join("aux-one/Cargo.lock"))?;
        let apply = run_cargo_rail(
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
            apply.status.success(),
            "release apply failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&apply.stdout),
            String::from_utf8_lossy(&apply.stderr)
        );
        for name in ["aux-one", "aux-two"] {
            let lockfile = std::fs::read(ws.path.join(name).join("Cargo.lock"))?;
            assert_ne!(lockfile, before, "{name} lockfile was not projected");
            let committed = git(&ws.path, &["show", &format!("HEAD:{name}/Cargo.lock")])?;
            assert_eq!(committed.stdout, lockfile, "{name} lockfile was not committed exactly");
            let text = String::from_utf8(lockfile)?;
            assert!(text.contains("name = \"aux-release\"\nversion = \"0.1.1\""));
        }
        let status = git(&ws.path, &["status", "--porcelain"])?;
        assert!(status.stdout.is_empty(), "release left unstaged paths");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_updates_all_packages_in_one_auxiliary_cargo_invocation() {
    use std::os::unix::fs::PermissionsExt as _;

    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("dual-release-one", "0.1.0", &[])?;
        ws.add_crate("dual-release-two", "0.1.0", &[])?;
        generate_lockfile(&ws.path)?;
        add_auxiliary_cargo_workspace_with_dependencies(
            &ws,
            "aux-dual",
            &[
                ("dual-release-one", "../crates/dual-release-one"),
                ("dual-release-two", "../crates/dual-release-two"),
            ],
        )?;
        ws.write_release_config(
            r#"source = "both"
tag_format = "{crate}-v{version}"
require_release_notes = false
auxiliary_cargo_manifests = ["aux-dual/Cargo.toml"]
"#,
        )?;
        ws.commit("Configure one auxiliary update for two releases")?;

        let wrapper_dir = tempfile::TempDir::new()?;
        let marker = wrapper_dir.path().join("update-called");
        let wrapper = wrapper_dir.path().join("cargo-wrapper");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nif [ \"$1\" = update ]; then\n  if [ -e \"{}\" ]; then exit 97; fi\n  printf 'one\\n' > \"{}\"\nfi\nexec \"{}\" \"$@\"\n",
                marker.display(),
                marker.display(),
                PathBuf::from(cargo).display()
            ),
        )?;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;

        let check = cargo_rail_command(&ws.path)?
            .env("CARGO", &wrapper)
            .args([
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--check",
                "--skip-publish",
                "--skip-tag",
                "--format",
                "json",
            ])
            .output()?;
        assert_eq!(
            check.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
        assert_eq!(std::fs::read_to_string(&marker)?, "one\n");
        let check: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        let candidate = check["release_plan"]["auxiliary_lockfiles"][0]["content"]
            .as_str()
            .expect("planned Cargo.lock content");
        assert!(candidate.contains("name = \"dual-release-one\"\nversion = \"0.1.1\""));
        assert!(candidate.contains("name = \"dual-release-two\"\nversion = \"0.1.1\""));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_accepts_git_clean_crlf_checkout() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-crlf", "0.1.0")?;
        git(&ws.path, &["config", "core.autocrlf", "true"])?;
        std::fs::write(
            ws.path.join(".gitattributes"),
            "**/Cargo.toml text eol=crlf\n**/Cargo.lock text eol=crlf\n",
        )?;
        add_auxiliary_cargo_workspace(&ws, "aux", "aux-crlf")?;
        ws.write_release_config(
            r#"source = "both"
tag_format = "v{version}"
require_release_notes = false
auxiliary_cargo_manifests = ["aux/Cargo.toml"]
"#,
        )?;
        ws.commit("Configure a CRLF auxiliary Cargo projection")?;

        for path in ["aux/Cargo.toml", "aux/Cargo.lock"] {
            std::fs::remove_file(ws.path.join(path))?;
        }
        git(
            &ws.path,
            &["checkout-index", "--force", "--", "aux/Cargo.toml", "aux/Cargo.lock"],
        )?;
        git(&ws.path, &["add", "--", "aux/Cargo.toml", "aux/Cargo.lock"])?;
        for path in ["aux/Cargo.toml", "aux/Cargo.lock"] {
            assert_only_crlf(&ws.path.join(path))?;
        }
        let status = git(&ws.path, &["status", "--porcelain", "--untracked-files=no"])?;
        assert!(status.stdout.is_empty(), "CRLF checkout is not Git-clean");

        let check = cargo_rail_command(&ws.path)?
            .args([
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--check",
                "--skip-publish",
                "--skip-tag",
                "--format",
                "json",
            ])
            .output()?;
        assert_eq!(
            check.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
        let check: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        assert_eq!(
            check["release_plan"]["auxiliary_lockfiles"].as_array().unwrap().len(),
            1
        );
        let status = git(&ws.path, &["status", "--porcelain", "--untracked-files=no"])?;
        assert!(status.stdout.is_empty(), "planning changed the CRLF checkout");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_accepts_git_clean_crlf_in_nested_workspace() {
    let result: Result<()> = (|| {
        let ws = NestedWorkspace::new("rust")?;
        git(&ws.git_root, &["config", "core.autocrlf", "true"])?;
        std::fs::write(
            ws.git_root.join(".gitattributes"),
            "rust/aux/Cargo.toml text eol=crlf\nrust/aux/Cargo.lock text eol=crlf\n",
        )?;
        ws.add_crate("nested-crlf", "0.1.0")?;
        let auxiliary = ws.workspace_root.join("aux");
        std::fs::create_dir_all(auxiliary.join("src"))?;
        std::fs::write(
            auxiliary.join("Cargo.toml"),
            r#"[package]
name = "nested-crlf-aux"
version = "0.0.0"
edition = "2021"
publish = false

[workspace]

[dependencies]
nested-crlf = { path = "../crates/nested-crlf" }
"#,
        )?;
        std::fs::write(auxiliary.join("src/lib.rs"), "pub fn auxiliary() {}\n")?;
        for workspace in [&ws.workspace_root, &auxiliary] {
            generate_lockfile(workspace)?;
        }
        std::fs::write(
            ws.workspace_root.join(".config/rail.toml"),
            r#"[release]
source = "both"
tag_format = "v{version}"
require_release_notes = false
auxiliary_cargo_manifests = ["aux/Cargo.toml"]
"#,
        )?;
        ws.commit("Configure a nested CRLF auxiliary Cargo projection")?;

        for path in ["rust/aux/Cargo.toml", "rust/aux/Cargo.lock"] {
            std::fs::remove_file(ws.git_root.join(path))?;
        }
        git(
            &ws.git_root,
            &[
                "checkout-index",
                "--force",
                "--",
                "rust/aux/Cargo.toml",
                "rust/aux/Cargo.lock",
            ],
        )?;
        git(
            &ws.git_root,
            &["add", "--", "rust/aux/Cargo.toml", "rust/aux/Cargo.lock"],
        )?;
        for path in ["rust/aux/Cargo.toml", "rust/aux/Cargo.lock"] {
            assert_only_crlf(&ws.git_root.join(path))?;
        }
        let status = git(&ws.git_root, &["status", "--porcelain", "--untracked-files=no"])?;
        assert!(status.stdout.is_empty(), "nested CRLF checkout is not Git-clean");

        let check = cargo_rail_command(&ws.workspace_root)?
            .args([
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--check",
                "--skip-publish",
                "--skip-tag",
                "--format",
                "json",
            ])
            .output()?;
        assert_eq!(
            check.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
        let check: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        assert_eq!(
            check["release_plan"]["auxiliary_lockfiles"].as_array().unwrap().len(),
            1
        );
        let status = git(&ws.git_root, &["status", "--porcelain", "--untracked-files=no"])?;
        assert!(status.stdout.is_empty(), "planning changed the nested CRLF checkout");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_manifest_must_match_head() {
    let result: Result<()> = (|| {
        let ws = configured_auxiliary_release("aux-dirty-manifest")?;
        let manifest = ws.path.join("aux/Cargo.toml");
        let mut changed = std::fs::read_to_string(&manifest)?;
        changed.push_str("\n# uncommitted\n");
        std::fs::write(&manifest, changed)?;

        let check = check_auxiliary_release(&ws)?;
        assert!(!check.status.success());
        let stderr = String::from_utf8_lossy(&check.stderr);
        assert!(
            stderr.contains("auxiliary Cargo manifest 'aux/Cargo.toml' does not exactly match HEAD")
                && stderr.contains("filter-cleaned worktree content or executable mode differs"),
            "{stderr}"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_lockfile_must_match_head() {
    let result: Result<()> = (|| {
        let ws = configured_auxiliary_release("aux-dirty-lock")?;
        let lockfile = ws.path.join("aux/Cargo.lock");
        let mut changed = std::fs::read_to_string(&lockfile)?;
        changed.push_str("\n# uncommitted\n");
        std::fs::write(&lockfile, changed)?;

        let check = check_auxiliary_release(&ws)?;
        assert!(!check.status.success());
        let stderr = String::from_utf8_lossy(&check.stderr);
        assert!(
            stderr.contains("auxiliary Cargo lockfile 'aux/Cargo.lock' does not exactly match HEAD")
                && stderr.contains("filter-cleaned worktree content or executable mode differs"),
            "{stderr}"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_manifest_rejects_index_only_changes() {
    let result: Result<()> = (|| {
        let ws = configured_auxiliary_release("aux-index-manifest")?;
        let manifest = ws.path.join("aux/Cargo.toml");
        let original = std::fs::read(&manifest)?;
        let mut changed = original.clone();
        changed.extend_from_slice(b"\n# staged only\n");
        std::fs::write(&manifest, changed)?;
        git(&ws.path, &["add", "--", "aux/Cargo.toml"])?;
        std::fs::write(&manifest, original)?;

        let check = check_auxiliary_release(&ws)?;
        assert!(!check.status.success());
        let stderr = String::from_utf8_lossy(&check.stderr);
        assert!(
            stderr.contains("auxiliary Cargo manifest 'aux/Cargo.toml' does not exactly match HEAD")
                && stderr.contains("index entry differs from HEAD"),
            "{stderr}"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_manifest_rejects_intent_to_add() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-intent-manifest", "0.1.0")?;
        ws.write_release_config(
            r#"source = "both"
tag_format = "v{version}"
require_release_notes = false
auxiliary_cargo_manifests = ["aux/Cargo.toml"]
"#,
        )?;
        ws.commit("Configure an unmaterialized auxiliary workspace")?;
        add_auxiliary_cargo_workspace(&ws, "aux", "aux-intent-manifest")?;
        git(
            &ws.path,
            &["add", "--intent-to-add", "--", "aux/Cargo.toml", "aux/Cargo.lock"],
        )?;

        let check = check_auxiliary_release(&ws)?;
        assert!(!check.status.success());
        let stderr = String::from_utf8_lossy(&check.stderr);
        assert!(
            stderr.contains("auxiliary Cargo manifest 'aux/Cargo.toml' does not exactly match HEAD")
                && stderr.contains("HEAD has no matching regular-file entry"),
            "{stderr}"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_rejects_absolute_and_escaping_path_dependencies() {
    let result: Result<()> = (|| {
        assert_external_auxiliary_dependency_rejected(true)?;
        assert_external_auxiliary_dependency_rejected(false)?;
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plans_auxiliary_lockfile_from_nested_workspace_root() {
    let result: Result<()> = (|| {
        let ws = NestedWorkspace::new("rust")?;
        ws.add_crate("nested-release", "0.1.0")?;
        let auxiliary = ws.workspace_root.join("aux");
        std::fs::create_dir_all(auxiliary.join("src"))?;
        std::fs::write(
            auxiliary.join("Cargo.toml"),
            r#"[package]
name = "nested-aux"
version = "0.0.0"
edition = "2021"
publish = false

[workspace]

[dependencies]
nested-release = { path = "../crates/nested-release" }
"#,
        )?;
        std::fs::write(auxiliary.join("src/lib.rs"), "pub fn auxiliary() {}\n")?;
        for workspace in [&ws.workspace_root, &auxiliary] {
            generate_lockfile(workspace)?;
        }
        std::fs::write(
            ws.workspace_root.join(".config/rail.toml"),
            r#"[release]
source = "both"
tag_format = "v{version}"
require_release_notes = false
auxiliary_cargo_manifests = ["aux/Cargo.toml"]
"#,
        )?;
        ws.commit("Configure nested auxiliary Cargo projection")?;

        let check = run_cargo_rail(
            &ws.workspace_root,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--check",
                "--skip-publish",
                "--skip-tag",
                "--format",
                "json",
            ],
        )?;
        assert_eq!(
            check.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
        let check: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        assert_eq!(
            check["release_plan"]["auxiliary_lockfiles"][0]["manifest_path"],
            "aux/Cargo.toml"
        );
        assert_eq!(
            check["release_plan"]["auxiliary_lockfiles"][0]["lockfile_path"],
            "aux/Cargo.lock"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_lockfile_plan_rejects_drift_before_mutation() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-drift", "0.1.0")?;
        add_auxiliary_cargo_workspace(&ws, "aux", "aux-drift")?;
        ws.write_release_config(
            r#"source = "both"
tag_format = "v{version}"
require_release_notes = false
auxiliary_cargo_manifests = ["aux/Cargo.toml"]
"#,
        )?;
        let initial_head = ws.commit("Configure auxiliary Cargo projection")?;
        let check = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--check",
                "--skip-publish",
                "--skip-tag",
                "--format",
                "json",
            ],
        )?;
        assert_eq!(check.status.code(), Some(1));
        let plan_dir = tempfile::TempDir::new()?;
        let plan_path = plan_dir.path().join("release-plan.json");
        std::fs::write(&plan_path, &check.stdout)?;
        let lockfile = ws.path.join("aux/Cargo.lock");
        let mut changed = std::fs::read_to_string(&lockfile)?;
        changed.push('\n');
        std::fs::write(&lockfile, changed)?;

        let apply = run_cargo_rail(
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
                "--plan",
                plan_path.to_str().unwrap(),
            ],
        )?;
        assert!(!apply.status.success());
        let stderr = String::from_utf8_lossy(&apply.stderr);
        assert!(
            stderr.contains("auxiliary Cargo lockfile 'aux/Cargo.lock' does not exactly match HEAD"),
            "{stderr}"
        );
        assert_eq!(
            git(&ws.path, &["rev-parse", "HEAD"])?.stdout,
            format!("{initial_head}\n").as_bytes()
        );
        assert!(std::fs::read_to_string(ws.path.join("Cargo.toml"))?.contains("version = \"0.1.0\""));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_cargo_failure_leaves_the_worktree_untouched() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-invalid", "0.1.0")?;
        std::fs::create_dir_all(ws.path.join("aux"))?;
        std::fs::write(ws.path.join("aux/Cargo.toml"), "this is not Cargo TOML\n")?;
        std::fs::write(ws.path.join("aux/Cargo.lock"), "version = 4\n")?;
        ws.write_release_config(
            r#"source = "both"
tag_format = "v{version}"
require_release_notes = false
auxiliary_cargo_manifests = ["aux/Cargo.toml"]
"#,
        )?;
        let initial_head = ws.commit("Add invalid auxiliary Cargo projection")?;
        let manifest = std::fs::read(ws.path.join("Cargo.toml"))?;
        let lockfile = std::fs::read(ws.path.join("aux/Cargo.lock"))?;

        let check = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--check",
                "--skip-publish",
                "--skip-tag",
            ],
        )?;
        assert!(!check.status.success());
        assert!(String::from_utf8_lossy(&check.stderr).contains("cargo locate-project failed"));
        assert_eq!(std::fs::read(ws.path.join("Cargo.toml"))?, manifest);
        assert_eq!(std::fs::read(ws.path.join("aux/Cargo.lock"))?, lockfile);
        assert_eq!(
            git(&ws.path, &["rev-parse", "HEAD"])?.stdout,
            format!("{initial_head}\n").as_bytes()
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_auxiliary_cargo_rejects_undeclared_command_mutation() {
    use std::os::unix::fs::PermissionsExt as _;

    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-command-boundary", "0.1.0")?;
        add_auxiliary_cargo_workspace(&ws, "aux", "aux-command-boundary")?;
        ws.write_release_config(
            r#"source = "both"
tag_format = "v{version}"
require_release_notes = false
auxiliary_cargo_manifests = ["aux/Cargo.toml"]
"#,
        )?;
        let initial_head = ws.commit("Configure bounded auxiliary Cargo projection")?;
        let manifest = std::fs::read(ws.path.join("Cargo.toml"))?;
        let lockfile = std::fs::read(ws.path.join("aux/Cargo.lock"))?;

        let wrapper_dir = tempfile::TempDir::new()?;
        let wrapper = wrapper_dir.path().join("cargo-wrapper");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nif [ \"$1\" = update ]; then printf 'unexpected\\n' > undeclared-by-cargo; fi\nexec \"{}\" \"$@\"\n",
                PathBuf::from(cargo).display()
            ),
        )?;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;

        let check = cargo_rail_command(&ws.path)?
            .env("CARGO", &wrapper)
            .args([
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--check",
                "--skip-publish",
                "--skip-tag",
            ])
            .output()?;
        assert!(!check.status.success());
        let stderr = String::from_utf8_lossy(&check.stderr);
        assert!(
            stderr.contains("created undeclared paths") && stderr.contains("undeclared-by-cargo"),
            "{stderr}"
        );
        assert!(!ws.path.join("undeclared-by-cargo").exists());
        assert_eq!(std::fs::read(ws.path.join("Cargo.toml"))?, manifest);
        assert_eq!(std::fs::read(ws.path.join("aux/Cargo.lock"))?, lockfile);
        assert_eq!(
            git(&ws.path, &["rev-parse", "HEAD"])?.stdout,
            format!("{initial_head}\n").as_bytes()
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_auxiliary_cargo_rejects_late_mutation_of_bound_lockfile() {
    use std::os::unix::fs::PermissionsExt as _;

    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-bound-candidate", "0.1.0")?;
        add_auxiliary_cargo_workspace(&ws, "aux-one", "aux-bound-candidate")?;
        add_auxiliary_cargo_workspace(&ws, "aux-two", "aux-bound-candidate")?;
        ws.write_release_config(
            r#"source = "both"
tag_format = "v{version}"
require_release_notes = false
auxiliary_cargo_manifests = ["aux-one/Cargo.toml", "aux-two/Cargo.toml"]
"#,
        )?;
        let initial_head = ws.commit("Configure exact auxiliary Cargo candidates")?;
        let manifest = std::fs::read(ws.path.join("Cargo.toml"))?;
        let first_lockfile = std::fs::read(ws.path.join("aux-one/Cargo.lock"))?;
        let second_lockfile = std::fs::read(ws.path.join("aux-two/Cargo.lock"))?;

        let wrapper_dir = tempfile::TempDir::new()?;
        let wrapper = wrapper_dir.path().join("cargo-wrapper");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\n\"{}\" \"$@\"\nstatus=$?\nif [ \"$status\" -eq 0 ] && [ \"$1\" = update ]; then\n  case \"$*\" in\n    *aux-two/Cargo.toml*) printf '\\n# late mutation\\n' >> aux-one/Cargo.lock ;;\n  esac\nfi\nexit \"$status\"\n",
                PathBuf::from(cargo).display()
            ),
        )?;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;

        let check = cargo_rail_command(&ws.path)?
            .env("CARGO", &wrapper)
            .args([
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--check",
                "--skip-publish",
                "--skip-tag",
            ])
            .output()?;
        assert!(!check.status.success());
        let stderr = String::from_utf8_lossy(&check.stderr);
        assert!(
            stderr.contains("mutated planned path 'aux-one/Cargo.lock' after binding its candidate bytes"),
            "{stderr}"
        );
        assert_eq!(std::fs::read(ws.path.join("Cargo.toml"))?, manifest);
        assert_eq!(std::fs::read(ws.path.join("aux-one/Cargo.lock"))?, first_lockfile);
        assert_eq!(std::fs::read(ws.path.join("aux-two/Cargo.lock"))?, second_lockfile);
        assert_eq!(
            git(&ws.path, &["rev-parse", "HEAD"])?.stdout,
            format!("{initial_head}\n").as_bytes()
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_lockfile_recovers_before_the_first_commit() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-recovery", "0.1.0")?;
        add_auxiliary_cargo_workspace(&ws, "aux", "aux-recovery")?;
        ws.write_release_config(
            r#"source = "both"
tag_format = "v{version}"
require_release_notes = false
auxiliary_cargo_manifests = ["aux/Cargo.toml"]
"#,
        )?;
        let initial_head = ws.commit("Configure auxiliary Cargo recovery")?;
        let before = std::fs::read(ws.path.join("aux/Cargo.lock"))?;

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
            "commit:aux-recovery",
        )?;
        assert!(!interrupted.status.success());
        assert_eq!(std::fs::read(ws.path.join("aux/Cargo.lock"))?, before);
        assert!(std::fs::read_to_string(ws.path.join("Cargo.toml"))?.contains("version = \"0.1.0\""));

        let state_path = only_release_state(&ws.path)?;
        let state: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
        assert_eq!(state["schema_version"], 5);
        assert_eq!(state["plan"]["plan_contract_version"], 5);
        assert_eq!(state["plan"]["auxiliary_lockfiles"].as_array().unwrap().len(), 1);
        let resumed = run_cargo_rail(&ws.path, &["rail", "release", "resume", state_path.to_str().unwrap()])?;
        assert!(resumed.status.success(), "{}", String::from_utf8_lossy(&resumed.stderr));
        let after = std::fs::read_to_string(ws.path.join("aux/Cargo.lock"))?;
        assert!(after.contains("name = \"aux-recovery\"\nversion = \"0.1.1\""));
        assert_eq!(
            git(&ws.path, &["rev-list", "--count", &format!("{initial_head}..HEAD")])?.stdout,
            b"1\n"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plan_projects_exact_sha_checks_publication_and_tags_last() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("release-plan-order", "0.1.0")?;
        ws.write_release_config(
            r#"source = "both"
tag_format = "v{version}"
require_clean = false
require_release_notes = false
remote_effects = "gitlab"
registry_publication = "crates-io"
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
                "--check",
                "--publish",
                "--format",
                "json",
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
        let publication = json["mutation_plan"]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|action| action["code"] == "PUBLISH_CRATE")
            .unwrap();
        assert_eq!(publication["payload"]["registry"], "crates-io");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plan_auto_rejects_shallow_clone_but_explicit_bump_works() {
    let result: Result<()> = (|| {
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
            combined.contains("--bump auto cannot run in a shallow clone")
                && combined.contains("git fetch --unshallow --tags"),
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_check_reports_shallow_clone_in_failure_taxonomy() {
    let result: Result<()> = (|| {
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
            combined.contains("--bump auto cannot run in a shallow clone")
                && combined.contains("git fetch --unshallow --tags"),
            "output:\n{}",
            combined
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plan_auto_names_no_previous_tag_full_history() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn version_group_propagates_max_auto_bump_and_surfaces_in_json() {
    let result: Result<()> = (|| {
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
        assert_eq!(json["release_plan"]["plan_contract_version"], 5);
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn version_group_partial_selection_rejects_or_expands_by_policy() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_pr_mode_round_trips_to_finalize_on_merge_commit() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-pr-mode")?;
        write_release_config(&ws, "require_release_notes = false\nremote_effects = \"push\"")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;

        let remote_root = tempfile::TempDir::new()?;
        let remote = remote_root.path().join("origin.git");
        std::fs::create_dir_all(remote.parent().unwrap())?;
        let output = Command::new("git")
            .args(["init", "--bare", remote.to_str().unwrap()])
            .output()?;
        assert!(output.status.success(), "bare remote init failed");
        let ssh = remote_root.path().join("ssh");
        std::fs::write(
            &ssh,
            format!(
                r#"#!/bin/sh
case "$*" in
  *git-receive-pack*) exec git-receive-pack "{}" ;;
  *git-upload-pack*) exec git-upload-pack "{}" ;;
esac
exit 1
"#,
                remote.display(),
                remote.display()
            ),
        )?;
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&ssh)?.permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&ssh, permissions)?;
        }
        ws.set_remote("git@github.com:org/repo.git")?;
        git(&ws.path, &["config", "core.sshCommand", ssh.to_str().unwrap()])?;
        git(&ws.path, &["push", "-u", "origin", "main"])?;
        install_pre_push_hook(
            &ws,
            r#"#!/bin/sh
context_file="$(dirname "$0")/../release-pr-hook-context"
printf '%s:%s\n' "$CARGO_RAIL_RELEASE_PUSH" "$CARGO_RAIL_OPERATION" >> "$context_file"
if [ "$CARGO_RAIL_RELEASE_PUSH" != "1" ] || [ "$CARGO_RAIL_OPERATION" != "release" ]; then
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
        let gh_commands = std::fs::read_to_string(&gh_log)?;
        assert!(gh_commands.contains("pr create") && gh_commands.contains("--repo org/repo"));
        assert_eq!(
            std::fs::read_to_string(ws.path.join(".git/release-pr-hook-context"))?,
            "1:release\n",
            "the cargo-rail-owned release PR push must provide the standard hook context"
        );
        let prepared_message =
            String::from_utf8_lossy(&git(&ws.path, &["log", "-1", "--format=%B"])?.stdout).to_string();
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
        git(&remote, &["fetch", ws.path.to_str().unwrap(), &merge_sha])?;
        git(&remote, &["update-ref", "refs/heads/main", &merge_sha])?;
        install_pre_push_hook(
            &ws,
            r#"#!/bin/sh
while read -r _local_ref _local_sha remote_ref _remote_sha; do
  case "$remote_ref" in
    refs/heads/*)
      echo "protected branch update rejected" >&2
      exit 1
      ;;
  esac
done
"#,
        )?;

        let output = run_with_path_prefix(
            &ws,
            gh_path.parent().unwrap(),
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
        let finalized_head = String::from_utf8_lossy(&git(&ws.path, &["rev-parse", "HEAD"])?.stdout)
            .trim()
            .to_string();
        assert_eq!(
            tag_target, merge_sha,
            "finalize should tag the merged commit covered by release evidence"
        );
        assert_eq!(finalized_head, merge_sha, "finalize must not manufacture a new commit");
        let remote_head = String::from_utf8_lossy(&git(&remote, &["rev-parse", "refs/heads/main"])?.stdout)
            .trim()
            .to_string();
        assert_eq!(
            remote_head, merge_sha,
            "finalize must not push a protected branch update"
        );
        let remote_tag = String::from_utf8_lossy(&git(&remote, &["rev-list", "-n", "1", "v0.2.0"])?.stdout)
            .trim()
            .to_string();
        assert_eq!(remote_tag, merge_sha, "the pushed tag must retain the proven commit");
        let gh_commands = std::fs::read_to_string(&gh_log)?;
        assert!(
            gh_commands.contains("api graphql --hostname github.com"),
            "GitHub readiness must target the bound host\n{}",
            gh_commands
        );
        assert!(
            !transaction.is_empty(),
            "prepare transaction identity should be preserved in the journal"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_finalize_requires_explicit_target_or_all() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_finalize_refuses_without_merged_release_notes() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_rejects_partial_change_file_consumption() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_add_and_status_support_json_output() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_status_names_only_is_empty_without_pending_files() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("change-names-only-empty")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;

        let output = run_cargo_rail(&ws.path, &["rail", "change", "status", "--format", "names-only"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "change status should succeed\n{}", stdout);
        assert_eq!(stdout, "", "names-only should be empty when no change files exist");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_status_names_only_lists_pending_change_paths() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_check_required_fails_when_changed_crate_lacks_change_file() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_check_required_passes_when_changed_crate_has_change_file() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_add_uses_stable_slug_hash_names_and_rejects_duplicate_intent() {
    let result: Result<()> = (|| {
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
            "--format",
            "names-only",
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn legacy_change_directory_guard_reports_git_mv_hint() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_add_rejects_change_dir_that_escapes_workspace() {
    let result: Result<()> = (|| {
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
            combined.contains("invalid configuration for 'release.change_dir'")
                && combined.contains("change_dir must be a workspace-relative path"),
            "output:\n{}",
            combined
        );
        assert!(!ws.path.parent().unwrap().join("outside").exists());

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_dir_override_round_trips_through_release_consumption() {
    let result: Result<()> = (|| {
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
                "--format",
                "names-only",
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_status_reports_max_bump_per_crate() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_add_without_message_errors_in_non_tty() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_changelog_uses_graph_attribution_for_cross_crate_commits() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_check_denies_unconventional_commits_when_configured() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_file_drives_auto_bump_and_is_consumed_on_release() {
    let result: Result<()> = (|| {
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
                "--format",
                "names-only",
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_check_enforces_required_change_file_coverage() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_changelog_generates_links_and_prs() {
    let result: Result<()> = (|| {
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
        let short_sha = feature_sha
            .get(..7)
            .ok_or_else(|| anyhow::anyhow!("feature commit SHA is shorter than seven bytes"))?;
        let initial_short_sha = initial_sha
            .get(..7)
            .ok_or_else(|| anyhow::anyhow!("initial commit SHA is shorter than seven bytes"))?;
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
            !changelog.contains(initial_short_sha),
            "should not include pre-tag commits"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_respects_skip_and_require_flags() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_preflight_requires_release_notes_by_default() {
    let result: Result<()> = (|| {
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

        let check = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a", "--bump", "patch"])?;
        let check_combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
        assert_eq!(
            check.status.code(),
            Some(2),
            "release check should fail\n{check_combined}"
        );
        assert!(
            check_combined.contains("no release notes for lib-a v0.1.1"),
            "expected missing release notes error\n{check_combined}"
        );

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
            stderr.contains("no release notes for lib-a v0.1.1")
                || stdout.contains("no release notes for lib-a v0.1.1"),
            "expected missing release notes error\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_preflight_can_disable_release_notes_requirement() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_rejects_github_release_without_owned_push() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_release_creates_gitlab_release_with_glab() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("gitlab-release", "0.1.0")?;
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "v{version}"
source = "commits"
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
        assert!(
            glab_log.contains("--repo "),
            "glab commands must target the bound repository\n{}",
            glab_log
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_release_errors_when_gitlab_forge_binary_missing() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("missing-glab", "0.1.0")?;
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "v{version}"
source = "commits"
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
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_release_pushes_commit_and_tag_when_push_enabled() {
    let result: Result<()> = (|| {
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
if [ "$CARGO_RAIL_TEST_INHERITED" != "from-caller" ]; then
  echo "missing inherited caller environment" >&2
  exit 1
fi
if [ "$CARGO_RAIL_RELEASE_PUSH" != "1" ]; then
  echo "missing CARGO_RAIL_RELEASE_PUSH" >&2
  exit 1
fi
if [ "$CARGO_RAIL_OPERATION" != "release" ]; then
  echo "missing CARGO_RAIL_OPERATION" >&2
  exit 1
fi
echo "release hook context accepted"
"#,
        )?;

        let trace_dir = tempfile::TempDir::new()?;
        let trace_path = trace_dir.path().join("git-trace.log");
        let output = cargo_rail_command(&ws.path)?
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_rejects_a_different_origin_push_repository_before_mutation() {
    let result: Result<()> = (|| {
        let (ws, _fetch_remote) = push_release_workspace("divergent-push")?;
        let push_remote = tempfile::TempDir::new()?;
        git(push_remote.path(), &["init", "--bare", "--initial-branch=main"])?;
        git(
            &ws.path,
            &["config", "remote.origin.pushurl", push_remote.path().to_str().unwrap()],
        )?;
        let head_before = git(&ws.path, &["rev-parse", "HEAD"])?.stdout;

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
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.status.code(), Some(2), "{combined}");
        assert!(
            combined.contains("origin fetches from") && combined.contains("but pushes to"),
            "{combined}"
        );
        assert_eq!(git(&ws.path, &["rev-parse", "HEAD"])?.stdout, head_before);
        assert!(!ws.path.join("target/cargo-rail/releases").exists());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_rejects_multiple_origin_push_repositories() {
    let result: Result<()> = (|| {
        let (ws, _fetch_remote) = push_release_workspace("multiple-pushes")?;
        let first = tempfile::TempDir::new()?;
        let second = tempfile::TempDir::new()?;
        for remote in [&first, &second] {
            git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
            git(
                &ws.path,
                &[
                    "config",
                    "--add",
                    "remote.origin.pushurl",
                    remote.path().to_str().unwrap(),
                ],
            )?;
        }

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
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.status.code(), Some(2), "{combined}");
        assert!(combined.contains("2 effective push URLs"), "{combined}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_reconstructs_missing_journal_from_git_in_a_second_checkout() {
    let result: Result<()> = (|| {
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
        git(&clone, &["config", "user.name", "Cargo-Rail Test"])?;
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
        let resumed = cargo_rail_command(&clone)?
            .env("PATH", path)
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_hook_failure_streams_and_preserves_both_output_streams() {
    let result: Result<()> = (|| {
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
            stderr.contains("hook stdout: release intent was rejected")
                && stderr.contains("hook stderr: policy details"),
            "the final Git error must preserve both streams\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_hook_failure_json_captures_structured_diagnostics() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_resume_reconciles_push_that_completed_before_failure() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("push-resume", "0.1.0")?;
        let remote = tempfile::TempDir::new()?;
        git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
        ws.set_remote(remote.path().to_str().unwrap())?;
        git(&ws.path, &["push", "-u", "origin", "main"])?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
source = "commits"
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_resume_rejects_remote_repository_drift() {
    let result: Result<()> = (|| {
        let (ws, _original_remote) = push_release_workspace("push-target-drift")?;
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
        let replacement = tempfile::TempDir::new()?;
        git(replacement.path(), &["init", "--bare", "--initial-branch=main"])?;
        git(
            &ws.path,
            &["remote", "set-url", "origin", replacement.path().to_str().unwrap()],
        )?;

        let resumed = run_cargo_rail(&ws.path, &["rail", "release", "resume", state_path.to_str().unwrap()])?;
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&resumed.stdout),
            String::from_utf8_lossy(&resumed.stderr)
        );
        assert_eq!(resumed.status.code(), Some(2), "{combined}");
        assert!(combined.contains("release repository changed from"), "{combined}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_abort_remains_local_before_a_push_after_origin_drift() {
    let result: Result<()> = (|| {
        let (ws, _original_remote) = push_release_workspace("abort-before-push-drift")?;
        let initial_head = git(&ws.path, &["rev-parse", "HEAD"])?.stdout;
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
            "commit:abort-before-push-drift",
        )?;
        assert!(!interrupted.status.success());
        let state_path = only_release_state(&ws.path)?;
        let replacement = tempfile::TempDir::new()?;
        git(replacement.path(), &["init", "--bare", "--initial-branch=main"])?;
        git(
            &ws.path,
            &["remote", "set-url", "origin", replacement.path().to_str().unwrap()],
        )?;

        let aborted = run_cargo_rail(
            &ws.path,
            &["rail", "release", "abort", state_path.to_str().unwrap(), "--yes"],
        )?;
        assert!(
            aborted.status.success(),
            "purely local abort must not depend on the current origin\n{}\n{}",
            String::from_utf8_lossy(&aborted.stdout),
            String::from_utf8_lossy(&aborted.stderr)
        );
        assert_eq!(git(&ws.path, &["rev-parse", "HEAD"])?.stdout, initial_head);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_abort_reconciles_push_rejected_by_local_hook() {
    let result: Result<()> = (|| {
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
source = "commits"
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_notes_override_satisfies_required_notes() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("manual-notes", "0.1.0")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
source = "commits"
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
    })();
    super::helpers::finish_test(result);
}

/// Test release --json output format
#[test]
fn test_release_json_output() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("json-release", "0.1.0")?;

        // Configure release
        ws.write_release_config("source = \"commits\"\nrequire_clean = false\nrequire_release_notes = false\n")?;

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
    })();
    super::helpers::finish_test(result);
}

/// Test release --skip-tag flag
#[test]
fn test_release_skip_tag_flag() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("skip-tag-crate", "0.1.0")?;

        // Configure release
        ws.write_release_config("source = \"commits\"\nrequire_clean = false\nrequire_release_notes = false\n")?;

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
    })();
    super::helpers::finish_test(result);
}

/// Registry publication is absent unless the operator authorizes it positively.
#[test]
fn test_release_publication_is_default_deny() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("skip-pub-crate", "0.1.0")?;

        // Configure release
        ws.write_release_config("source = \"commits\"\nrequire_clean = false\nrequire_release_notes = false\n")?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "run", "--check", "--bump", "patch"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Exit code 1 = check found pending changes (correct behavior)
        assert!(
            output.status.code() == Some(1),
            "release --check should exit 1 when release pending"
        );
        assert!(
            stdout.contains("not authorized; pass --publish") && stdout.contains("0 to publish"),
            "release preview must show that publication lacks positive authorization.\nOutput:\n{}",
            stdout
        );

        ws.write_release_config(
            "source = \"commits\"\nrequire_clean = false\nrequire_release_notes = false\nremote_effects = \"push\"\n",
        )?;
        let missing_config_authority = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "--check", "--bump", "patch", "--publish"],
        )?;
        assert_eq!(missing_config_authority.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&missing_config_authority.stderr)
                .contains("--publish requires release.registry_publication = \"crates-io\"")
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_local_effects_never_plan_registry_or_remote_actions() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("local-release-authority", "0.1.0")?;
        ws.write_release_config(
            "source = \"commits\"\nrequire_clean = false\nrequire_release_notes = false\nremote_effects = \"none\"\n",
        )?;

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail", "release", "run", "--check", "--bump", "patch", "--format", "json",
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
        for external in [
            "PUBLISH_CRATE",
            "PUSH_RELEASE_COMMIT",
            "AWAIT_EXACT_SHA_CHECKS",
            "PUSH_RELEASE_TAGS",
            "CREATE_FORGE_RELEASE",
            "PUBLISH_FORGE_RELEASE",
        ] {
            assert!(
                !codes.contains(&external),
                "local-only plan contains {external}: {codes:?}"
            );
        }

        let rejected = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "--check", "--bump", "patch", "--publish"],
        )?;
        assert_eq!(rejected.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&rejected.stderr)
                .contains("--publish cannot be combined with release.remote_effects = \"none\"")
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn cargo_publish_authority_cannot_be_widened_by_rail_config() {
    let result: Result<()> = (|| {
        for (name, publish_line) in [
            ("manifest-private", "publish = false"),
            ("registry-private", "publish = [\"private\"]"),
        ] {
            let ws = TestWorkspace::new_single_crate(name, "0.1.0")?;
            let manifest_path = ws.path.join("Cargo.toml");
            let manifest = std::fs::read_to_string(&manifest_path)?;
            let manifest = manifest.replacen("edition = \"2021\"", &format!("edition = \"2021\"\n{publish_line}"), 1);
            anyhow::ensure!(manifest.contains(publish_line));
            std::fs::write(&manifest_path, manifest)?;
            ws.write_release_config(&format!(
                "require_clean = false\nrequire_release_notes = false\nremote_effects = \"push\"\nregistry_publication = \"crates-io\"\n\n[crates.{name}.release]\npublish = true\n"
            ))?;
            std::fs::create_dir_all(ws.path.join(".changes"))?;
            std::fs::write(
                ws.path.join(".changes/publish-authority.md"),
                format!("---\n\"{name}\" = \"patch\"\n---\n\nExercise Cargo publication authority.\n"),
            )?;

            let output = run_cargo_rail(
                &ws.path,
                &[
                    "rail",
                    "release",
                    "run",
                    name,
                    "--check",
                    "--bump",
                    "patch",
                    "--publish",
                    "--format",
                    "json",
                ],
            )?;
            assert_eq!(output.status.code(), Some(1));
            let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            let publishes = json["mutation_plan"]["actions"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|action| action["code"] == "PUBLISH_CRATE")
                .count();
            assert_eq!(publishes, 0, "Cargo registry authority was widened for {name}");
            assert_eq!(json["release_plan"]["crates"][0]["publish"], false);
        }

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test explicit version bump (e.g., "1.2.3" instead of "patch")
#[test]
fn test_release_explicit_version() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("explicit-ver", "0.1.0")?;

        // Configure release
        ws.write_release_config("source = \"commits\"\nrequire_clean = false\nrequire_release_notes = false\n")?;

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
    })();
    super::helpers::finish_test(result);
}

// release.changelog.relative_to tests
/// Test default changelog relative_to behavior (crate-relative)
#[test]
fn test_changelog_relative_to_crate_default() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

/// Test release.changelog.relative_to = "workspace" creates changelog at workspace root
#[test]
fn test_changelog_relative_to_workspace() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_rejects_an_absolute_changelog_path_outside_the_workspace() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_rejects_a_symlink_changelog_path() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

/// Test that parent directories are auto-created for changelog paths
#[test]
fn test_changelog_parent_directories_auto_created() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

/// Test release.changelog.relative_to = "crate" with custom path creates in crate subdir
#[test]
fn test_changelog_relative_to_crate_custom_path() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

// Prerelease Bump Tests

/// Test --bump prerelease from stable version
#[test]
fn test_bump_prerelease_from_stable() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("prerelease-test", "1.0.0")?;
        ws.write_release_config("source = \"commits\"\nrequire_clean = false\nrequire_release_notes = false\n")?;

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
    })();
    super::helpers::finish_test(result);
}

/// Test --bump prerelease increments existing prerelease
#[test]
fn test_bump_prerelease_increment() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("prerelease-inc", "2.0.0-rc.1")?;
        ws.write_release_config("source = \"commits\"\nrequire_clean = false\nrequire_release_notes = false\n")?;

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
    })();
    super::helpers::finish_test(result);
}

/// Test --bump release strips prerelease suffix
#[test]
fn test_bump_release_strips_prerelease() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("release-strip", "1.5.0-beta.3")?;
        ws.write_release_config("source = \"commits\"\nrequire_clean = false\nrequire_release_notes = false\n")?;

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
    })();
    super::helpers::finish_test(result);
}

// Extended Check Tests

/// Test release check --extended runs dry-run publish validation
#[test]
fn test_release_check_extended_validates_publish() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("ext-check", "0.1.0")?;
        ws.write_release_config("require_clean = false\n")?;

        // Run release check with --extended --all (single-crate needs explicit crate name or --all)
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "check", "--publication", "--extended", "--all"],
        )?;
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
    })();
    super::helpers::finish_test(result);
}

/// Test release check --extended with JSON output
#[test]
fn test_release_check_extended_json() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("ext-json", "0.1.0")?;
        ws.write_release_config("require_clean = false\n")?;

        // Run release check with --extended --json --all
        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "check",
                "--publication",
                "--extended",
                "--json",
                "--all",
            ],
        )?;
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
    })();
    super::helpers::finish_test(result);
}

// Release Safety Tests (Branch Detection)

#[test]
fn release_rejects_unsafe_tag_names_before_mutation() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_resume_reconciles_tag_created_before_failure() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_resume_rejects_same_branch_head_movement() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_recovery_survives_invalid_metadata_and_clean_refuses_active_state() {
    let result: Result<()> = (|| {
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
        let clean = run_cargo_rail(
            &ws.path,
            &["rail", "clean", "--release-journal", state_path.to_str().unwrap()],
        )?;
        assert!(!clean.status.success(), "clean must refuse an active journal");
        assert!(String::from_utf8_lossy(&clean.stderr).contains("clean refused active release transaction"));

        std::fs::write(&manifest_path, "not valid Cargo metadata again\n")?;
        let config_path = ws.path.join(".config/rail.toml");
        assert!(config_path.exists(), "test release config disappeared before recovery");
        let resumed = run_cargo_rail(&ws.path, &["rail", "release", "resume", state_path.to_str().unwrap()])?;
        assert!(resumed.status.success(), "{}", String::from_utf8_lossy(&resumed.stderr));
        let cleaned = run_cargo_rail(
            &ws.path,
            &["rail", "clean", "--release-journal", state_path.to_str().unwrap()],
        )?;
        assert!(cleaned.status.success(), "{}", String::from_utf8_lossy(&cleaned.stderr));
        assert!(!state_path.exists(), "clean should prune the completed journal");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_resume_reconciles_a_journal_write_that_failed_after_persistence() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn clean_prunes_a_planned_journal_superseded_before_any_effect() {
    let result: Result<()> = (|| {
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

        let cleaned = run_cargo_rail(
            &ws.path,
            &["rail", "clean", "--release-journal", state_path.to_str().unwrap()],
        )?;
        assert_eq!(cleaned.status.code(), Some(2));
        assert!(
            state_path.exists(),
            "active superseded state is not terminal cleanup authority"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_transaction_id_is_recorded_in_commits_and_terminal_status() {
    let result: Result<()> = (|| {
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

        let active = run_cargo_rail(&ws.path, &["rail", "release", "status", "--format", "json"])?;
        let active: serde_json::Value = serde_json::from_slice(&active.stdout)?;
        assert_eq!(active["transactions"], serde_json::json!([]));

        let status = run_cargo_rail(
            &ws.path,
            &["rail", "release", "status", "--history", "--format", "json"],
        )?;
        let status: serde_json::Value = serde_json::from_slice(&status.stdout)?;
        assert_eq!(status["transactions"][0]["state"], "released:complete");
        assert_eq!(status["transactions"][0]["recoverability"], "terminal");
        assert_eq!(
            status["transactions"][0]["safe_operator_command"],
            format!("cargo rail clean --release-journal {transaction_id}")
        );
        let cleaned = run_cargo_rail(&ws.path, &["rail", "clean", "--release-journal", transaction_id])?;
        assert!(cleaned.status.success(), "{}", String::from_utf8_lossy(&cleaned.stderr));
        let reconstructed = run_cargo_rail(
            &ws.path,
            &["rail", "release", "status", "--history", "--format", "json"],
        )?;
        let reconstructed: serde_json::Value = serde_json::from_slice(&reconstructed.stdout)?;
        assert_eq!(reconstructed["transactions"][0]["state"], "released:git");
        assert_eq!(reconstructed["transactions"][0]["recoverability"], "terminal");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn historical_prepare_with_merged_descendant_tag_is_terminal_not_finalizable() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("historical-prepare", "0.1.0")?;
        let manifest = std::fs::read_to_string(ws.path.join("Cargo.toml"))?.replace("0.1.0", "0.1.1");
        std::fs::write(ws.path.join("Cargo.toml"), manifest)?;
        let transaction = "release-historical-prepare";
        let prepare_sha = ws.commit(&format!(
            "chore(release): prepare\n\nRail-Release: {transaction}\nRail-Release-Mode: prepare\nRail-Release-Publish: false\nRail-Release-Tag: true\nRail-Release-Remote: none\nRail-Release-Crate: historical-prepare@0.1.1\nRail-Release-Tag-Name: historical-prepare=v0.1.1\nRail-Release-Crate-Publish: historical-prepare=false"
        ))?;
        std::fs::write(ws.path.join("merged-release-evidence"), "merged\n")?;
        let merged_sha = ws.commit("Merge prepared release")?;
        git(&ws.path, &["tag", "-a", "v0.1.1", "-m", "Release v0.1.1", &merged_sha])?;

        let status = run_cargo_rail(
            &ws.path,
            &["rail", "release", "status", "--history", "--format", "json"],
        )?;
        assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
        let status: serde_json::Value = serde_json::from_slice(&status.stdout)?;
        let reconstructed = status["transactions"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("release transactions are unavailable"))?
            .iter()
            .find(|entry| entry["transaction_id"] == transaction)
            .ok_or_else(|| anyhow::anyhow!("historical prepare transaction is unavailable"))?;
        assert_eq!(reconstructed["state"], "released:git");
        assert_eq!(reconstructed["recoverability"], "terminal");
        assert_eq!(reconstructed["ambiguity"], false);
        assert_eq!(reconstructed["exact_sha"], merged_sha);
        assert_eq!(
            reconstructed["safe_operator_command"],
            "none (transaction is terminal; no release journal exists)"
        );
        assert_ne!(prepare_sha, merged_sha);
        assert!(
            reconstructed["observations"]
                .as_array()
                .is_some_and(|observations| observations.iter().any(|value| {
                    value
                        .as_str()
                        .is_some_and(|value| value == format!("prepare:merged_target={merged_sha}"))
                })),
            "merged release target was not retained in status: {reconstructed}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_abort_restores_local_state_before_remote_side_effects() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

/// Test that release apply requires explicit confirmation in non-interactive mode
#[test]
fn test_release_requires_explicit_confirmation_non_interactive() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_confirmation_pty_accepts_only_yes_and_keeps_the_prompt_on_stderr() {
    fn fixture(name: &str) -> Result<TestWorkspace> {
        let ws = TestWorkspace::new_named(name)?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        ws.tag("lib-a-v0.1.0", "Initial release")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn pty_confirmation() {}")?;
        ws.commit("feat: add PTY confirmation fixture")?;
        Ok(ws)
    }

    fn run_in_pty(ws: &TestWorkspace, answer: &[u8]) -> Result<(String, String)> {
        const PTY_RUNNER: &str = r#"
import os
import pty
import subprocess
import sys
import time

binary, root, stdout_path, stderr_path, answer_hex = sys.argv[1:]
master, slave = pty.openpty()
try:
    with open(stdout_path, "wb") as stdout, open(stderr_path, "wb") as stderr:
        child = subprocess.Popen(
            [binary, "rail", "release", "run", "lib-a", "--bump", "patch", "--skip-publish", "--skip-tag"],
            cwd=root,
            stdin=slave,
            stdout=stdout,
            stderr=stderr,
            close_fds=True,
        )
    os.close(slave)
    slave = -1
    deadline = time.monotonic() + 30
    prompt = b"Proceed? [y/N] "
    while time.monotonic() < deadline:
        try:
            with open(stderr_path, "rb") as stderr:
                if prompt in stderr.read():
                    break
        except FileNotFoundError:
            pass
        if child.poll() is not None:
            raise RuntimeError(f"command exited before prompting: {child.returncode}")
        time.sleep(0.02)
    else:
        child.kill()
        child.wait()
        raise RuntimeError("command did not reach its confirmation prompt")
    os.write(master, bytes.fromhex(answer_hex))
    returncode = child.wait(timeout=60)
    if returncode != 0:
        raise RuntimeError(f"command failed after prompting: {returncode}")
finally:
    if slave >= 0:
        os.close(slave)
    os.close(master)
"#;

        let stdout = ws.path.join("target/pty-stdout");
        let stderr = ws.path.join("target/pty-stderr");
        std::fs::create_dir_all(ws.path.join("target"))?;
        let answer_hex = answer.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let runner = Command::new("python3")
            .args(["-c", PTY_RUNNER])
            .arg(env!("CARGO_BIN_EXE_cargo-rail"))
            .arg(&ws.path)
            .arg(&stdout)
            .arg(&stderr)
            .arg(answer_hex)
            .output()?;
        anyhow::ensure!(
            runner.status.success(),
            "PTY runner failed: {}",
            String::from_utf8_lossy(&runner.stderr)
        );
        Ok((std::fs::read_to_string(stdout)?, std::fs::read_to_string(stderr)?))
    }

    let result: Result<()> = (|| {
        for (name, answer) in [
            ("lower-y", &b"y\r"[..]),
            ("upper-y", &b"Y\r"[..]),
            ("lower-yes", &b"yes\r"[..]),
            ("mixed-yes", &b"YeS\r"[..]),
        ] {
            let accepted = fixture(&format!("release-confirmation-pty-{name}"))?;
            let (stdout, stderr) = run_in_pty(&accepted, answer)?;
            assert!(
                stderr.contains("Proceed? [y/N] "),
                "prompt missing from stderr for {name}: {stderr}"
            );
            assert!(
                !stdout.contains("Proceed? [y/N] "),
                "prompt leaked to stdout for {name}: {stdout}"
            );
            assert!(
                std::fs::read_to_string(accepted.path.join("crates/lib-a/Cargo.toml"))?.contains("version = \"0.1.1\""),
                "accepted answer {name} did not authorize the release"
            );
        }

        for (name, answer) in [
            ("enter", &b"\r"[..]),
            ("n", &b"n\r"[..]),
            ("no", &b"no\r"[..]),
            ("leading-space", &b" y\r"[..]),
            ("trailing-space", &b"yes \r"[..]),
            ("other", &b"anything\r"[..]),
            ("eof", &b"\x04"[..]),
        ] {
            let rejected = fixture(&format!("release-confirmation-pty-{name}"))?;
            let head = git(&rejected.path, &["rev-parse", "HEAD"])?.stdout;
            let (stdout, stderr) = run_in_pty(&rejected, answer)?;
            assert!(
                stderr.contains("Proceed? [y/N] "),
                "prompt missing from stderr for {name}: {stderr}"
            );
            assert!(
                !stdout.contains("Proceed? [y/N] "),
                "prompt leaked to stdout for {name}: {stdout}"
            );
            assert_eq!(
                git(&rejected.path, &["rev-parse", "HEAD"])?.stdout,
                head,
                "rejected answer {name} changed HEAD"
            );
            assert!(
                std::fs::read_to_string(rejected.path.join("crates/lib-a/Cargo.toml"))?.contains("version = \"0.1.0\""),
                "rejected answer {name} mutated the manifest"
            );
        }

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that release fails from detached HEAD
#[test]
fn test_release_detached_head_fails() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

/// Test that confirmation does not authorize a non-default branch.
#[test]
fn test_release_non_default_branch_fails_without_branch_authority() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-branch")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;

        // Create and switch to a feature branch
        crate::helpers::git(&ws.path, &["checkout", "-b", "feature-branch"])?;

        // --yes skips only the prompt; it does not authorize this branch.
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

        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            !output.status.success(),
            "Release from non-default branch should fail without branch authority.\nstderr:\n{}",
            stderr
        );
        assert!(
            stderr.contains("feature-branch") && stderr.contains("--allow-non-default-branch"),
            "Error should name the branch authority flag.\nstderr:\n{}",
            stderr
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that prompt and branch authority are independently explicit.
#[test]
fn test_release_non_default_branch_requires_confirmation_and_branch_flags() {
    let result: Result<()> = (|| {
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
                "--allow-non-default-branch",
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
    })();
    super::helpers::finish_test(result);
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

#[test]
fn release_check_uses_the_same_local_plan_for_an_unpublishable_workspace() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-check-local-unpublishable")?;
        write_release_config(&ws, "remote_effects = \"none\"")?;
        add_unpublishable_crate(&ws, "internal", "0.1.0")?;
        ws.commit("feat: add internal release")?;

        let check = run_cargo_rail(&ws.path, &["rail", "release", "check", "--all", "--format", "json"])?;
        let run = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "--all", "--check", "--format", "json"],
        )?;
        assert_eq!(check.status.code(), Some(1));
        assert_eq!(run.status.code(), Some(1));

        let check: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        let run: serde_json::Value = serde_json::from_slice(&run.stdout)?;
        assert_eq!(check["release_plan"], run["release_plan"]);
        assert_eq!(check["mutation_plan"], run["mutation_plan"]);
        assert_eq!(check["release_plan"]["summary"]["crates_to_publish"], 0);
        assert_eq!(check["release_plan"]["summary"]["crates_to_tag"], 1);
        assert_eq!(check["readiness"]["scope"], "local");
        assert_eq!(check["readiness"]["effects_executed"], serde_json::json!([]));
        assert_eq!(check["readiness"]["planned_effects"]["git_tag"], true);
        assert_eq!(check["readiness"]["planned_effects"]["registry_publication"], false);
        assert_eq!(
            check["readiness"]["effects_excluded_from_check"],
            serde_json::json!([
                "workspace_mutation",
                "git_commit",
                "git_tag",
                "git_push",
                "forge_release",
                "registry_publication"
            ])
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_publication_check_names_its_publishable_only_scope() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-check-publication-unpublishable")?;
        write_release_config(&ws, "")?;
        add_unpublishable_crate(&ws, "internal", "0.1.0")?;
        ws.commit("feat: add internal release")?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--publication", "--all"])?;
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("no publishable crates found"));

        let json = run_cargo_rail(
            &ws.path,
            &["rail", "release", "check", "--publication", "--all", "--format", "json"],
        )?;
        assert_eq!(json.status.code(), Some(2));
        let json: serde_json::Value = serde_json::from_slice(&json.stdout)?;
        assert_eq!(json["result"], "failed");
        assert_eq!(json["exit_code"], 2);
        assert_eq!(json["readiness"]["scope"], "publication");
        assert_eq!(json["readiness"]["effects_executed"], serde_json::json!([]));
        assert_eq!(json["error"], "no publishable crates found");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that --all skips crates with publish = false in Cargo.toml
#[test]
fn test_release_check_all_skips_unpublishable_cargo_toml() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("check-skip-unpub")?;
        write_release_config(&ws, "")?;

        // Add a publishable crate
        ws.add_crate("lib-pub", "0.1.0", &[])?;

        // Add an unpublishable crate (publish = false in Cargo.toml)
        add_unpublishable_crate(&ws, "lib-internal", "0.1.0")?;

        ws.commit("Add crates")?;

        // Run release check --all
        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--publication", "--all"])?;
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
    })();
    super::helpers::finish_test(result);
}

/// Test that path-only deps are allowed for crates with publish = false
#[test]
fn test_release_check_path_deps_allowed_for_unpublishable() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("path-dep-unpub")?;
        write_release_config(&ws, "")?;

        // Add a publishable crate
        ws.add_crate("lib-core", "0.1.0", &[])?;

        // Add an unpublishable crate with a path-only dep
        add_crate_with_path_dep(&ws, "wasm-bindings", "0.1.0", "lib-core", false)?;

        ws.commit("Add crates")?;

        // Run release check --all
        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--publication", "--all"])?;
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
    })();
    super::helpers::finish_test(result);
}

/// Test that explicitly naming an unpublishable crate reports its status
#[test]
fn test_release_check_explicit_unpublishable_crate() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("explicit-unpub")?;
        write_release_config(&ws, "")?;

        // Add an unpublishable crate
        add_unpublishable_crate(&ws, "internal-tool", "0.1.0")?;
        ws.commit("Add crates")?;

        // Run release check on the specific crate
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "check", "--publication", "internal-tool"],
        )?;
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
    })();
    super::helpers::finish_test(result);
}

/// Test JSON output includes skipped crates
#[test]
fn test_release_check_json_includes_skipped() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("json-skipped")?;
        write_release_config(&ws, "")?;

        // Add publishable and unpublishable crates
        ws.add_crate("lib-pub", "0.1.0", &[])?;
        add_unpublishable_crate(&ws, "lib-internal", "0.1.0")?;
        ws.commit("Add crates")?;

        // Run release check --all --json
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "check", "--publication", "--all", "--json"],
        )?;
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
    })();
    super::helpers::finish_test(result);
}

/// Test that rail.toml publish = false is respected
#[test]
fn test_release_check_respects_rail_toml_publish_false() {
    let result: Result<()> = (|| {
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
        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--publication", "--all"])?;
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_run_rejects_partial_dependent_closure() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_run_include_dependents_expands_full_closure() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_subset_release_only_mutates_selected_closure_tags_and_changelogs() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_run_apply_supports_publish_false_from_rail_toml() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_subset_release_updates_shared_workspace_dependency_versions() {
    let result: Result<()> = (|| {
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
    })();
    super::helpers::finish_test(result);
}
