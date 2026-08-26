//! Integration tests for plan/apply flows.

use crate::helpers::{TestWorkspace, git, run_cargo_rail};
use anyhow::Result;
use tempfile::TempDir;

#[test]
fn test_split_apply_from_plan_file() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-plan-apply")?;
        ws.add_crate("mylib", "0.1.0", &[])?;
        ws.commit("Add mylib")?;

        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.mylib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        let check = run_cargo_rail(
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
        assert_eq!(check.status.code(), Some(1), "split check should exit 1");
        let plan_path = ws.path.join("split-plan.json");
        std::fs::write(&plan_path, &check.stdout)?;

        let apply = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "split",
                "run",
                "mylib",
                "--allow-dirty",
                "--plan",
                plan_path.to_string_lossy().as_ref(),
                "--yes",
            ],
        )?;
        assert!(
            apply.status.success(),
            "split apply --plan should succeed.\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&apply.stdout),
            String::from_utf8_lossy(&apply.stderr)
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_sync_apply_from_plan_file() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("sync-plan-apply")?;
        ws.add_crate("mylib", "0.1.0", &[])?;
        ws.commit("Add mylib")?;

        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.mylib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        let split = run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;
        assert!(split.status.success(), "initial split should succeed");

        std::fs::write(ws.path.join("crates/mylib/src/lib.rs"), "pub fn changed() {}\n")?;
        ws.commit("Change mylib after split")?;

        let check = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "mylib",
                "--to-remote",
                "--check",
                "--allow-dirty",
                "-f",
                "json",
            ],
        )?;
        assert_eq!(
            check.status.code(),
            Some(1),
            "sync --check -f json should exit 1 when pending changes are detected"
        );
        let plan_path = ws.path.join("sync-plan.json");
        std::fs::write(&plan_path, &check.stdout)?;

        let apply = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "sync",
                "mylib",
                "--to-remote",
                "--allow-dirty",
                "--plan",
                plan_path.to_string_lossy().as_ref(),
                "--yes",
            ],
        )?;
        assert!(
            apply.status.success(),
            "sync apply --plan should succeed.\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&apply.stdout),
            String::from_utf8_lossy(&apply.stderr)
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_apply_from_plan_file() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("relplan", "0.1.0")?;
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "v{version}"
require_clean = false
require_release_notes = false
"#,
        )?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        std::fs::write(
            ws.path.join(".changes/release-plan.md"),
            "---\n\"relplan\" = \"patch\"\n---\n\nExercise release apply from a reviewed plan.\n",
        )?;
        ws.commit("Configure release plan test")?;

        let check = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--check",
                "--bump",
                "patch",
                "--skip-publish",
                "--skip-tag",
                "--json",
            ],
        )?;
        assert_eq!(check.status.code(), Some(1), "release check should report changes");
        let plan_path = ws.path.join("release-plan.json");
        std::fs::write(&plan_path, &check.stdout)?;

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
                plan_path.to_string_lossy().as_ref(),
            ],
        )?;
        assert!(
            apply.status.success(),
            "release apply --plan should succeed.\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&apply.stdout),
            String::from_utf8_lossy(&apply.stderr)
        );

        let apply_receipt = std::fs::read_dir(ws.path.join("target/cargo-rail/receipts"))?
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .filter_map(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
            .find(|receipt| receipt["operation"] == "release" && receipt["phase"] == "apply")
            .expect("release apply receipt");
        assert_eq!(apply_receipt["plan"]["contract_version"], 2);
        assert!(
            apply_receipt["verified_inputs"]["worktree_fingerprint"]
                .as_str()
                .is_some_and(|fingerprint| fingerprint.starts_with("git-object:"))
        );
        assert!(
            apply_receipt["applied_actions"]
                .as_array()
                .is_some_and(|actions| !actions.is_empty())
        );
        assert!(
            apply_receipt["resulting_objects"]
                .as_array()
                .is_some_and(|objects| objects.iter().any(|object| object["kind"] == "commit"))
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_plan_rejects_unreviewed_file_before_mutation() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("release-authority", "0.1.0")?;
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "v{version}"
require_clean = false
require_release_notes = false
"#,
        )?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        std::fs::write(
            ws.path.join(".changes/release-authority.md"),
            "---\n\"release-authority\" = \"patch\"\n---\n\nExercise release mutation authority.\n",
        )?;
        let config_head = ws.commit("Configure release")?;

        let check = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--check",
                "--bump",
                "patch",
                "--skip-publish",
                "--skip-tag",
                "--json",
            ],
        )?;
        assert_eq!(
            check.status.code(),
            Some(1),
            "release plan check failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
        let plan_dir = TempDir::new()?;
        let plan_path = plan_dir.path().join("release-plan.json");
        std::fs::write(&plan_path, &check.stdout)?;

        let unreviewed = ws.path.join("unreviewed.txt");
        std::fs::write(&unreviewed, "must not enter the release commit\n")?;
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
                plan_path.to_string_lossy().as_ref(),
            ],
        )?;

        assert!(
            !apply.status.success(),
            "release must reject post-approval worktree drift"
        );
        let stderr = String::from_utf8_lossy(&apply.stderr);
        assert!(
            stderr.contains("worktree changed") && stderr.contains("unreviewed.txt"),
            "error must identify the unreviewed path\nstderr:\n{}",
            stderr
        );
        let head = git(&ws.path, &["rev-parse", "HEAD"])?;
        assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), config_head);
        assert!(
            unreviewed.exists(),
            "rejected input should be left untouched for recovery"
        );
        assert!(
            !String::from_utf8_lossy(&git(&ws.path, &["ls-tree", "-r", "--name-only", "HEAD"])?.stdout)
                .contains("unreviewed.txt"),
            "unreviewed file must never be committed"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}
