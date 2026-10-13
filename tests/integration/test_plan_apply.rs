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
        git(split_dir.path(), &["init", "--initial-branch=main"])?;
        let config = format!(
            r#"[crates.mylib.split]
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
                "--allow-dirty",
                "--check",
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
        git(split_dir.path(), &["init", "--initial-branch=main"])?;
        git(split_dir.path(), &["config", "user.name", "Test User"])?;
        git(split_dir.path(), &["config", "user.email", "test@example.com"])?;
        let config = format!(
            r#"[crates.mylib.split]
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
                "--allow-dirty",
                "--check",
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

/// The post-apply check must see files that appeared after the context captured the worktree.
#[test]
fn test_post_apply_check_rejects_untracked_files_created_after_capture() {
    use cargo_rail::mutation::{
        MUTATION_CONTRACT_VERSION, MutationPlan, MutationPreApplyChecks, MutationResolvedRefs, validate_changed_paths,
    };

    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("post-apply-untracked")?;
        ws.add_crate("demo", "0.1.0", &[])?;
        ws.commit("Add demo")?;
        let ctx = cargo_rail::workspace::WorkspaceContext::build_with_snapshot(&ws.path)?;
        let empty = MutationPlan {
            contract_version: MUTATION_CONTRACT_VERSION,
            operation: "unify".to_string(),
            operation_id: "unify-test".to_string(),
            inputs_fingerprint: String::new(),
            resolved_refs: MutationResolvedRefs {
                git_head: String::new(),
                git_branch: "main".to_string(),
            },
            actions: Vec::new(),
            declared_inputs: Vec::new(),
            risks: Vec::new(),
            trace: Vec::new(),
            pre_apply: MutationPreApplyChecks {
                git_head: String::new(),
                config_fingerprint: String::new(),
                toolchain_fingerprint: String::new(),
                lock_fingerprint: String::new(),
                metadata_fingerprint: String::new(),
                worktree_fingerprint: String::new(),
                changed_paths: Vec::new(),
            },
        };
        validate_changed_paths(&ctx, &empty)?;

        std::fs::write(ws.path.join("unplanned.txt"), "written during apply\n")?;
        let error = validate_changed_paths(&ctx, &empty).expect_err("a new untracked file is unplanned");
        assert!(error.to_string().contains("unplanned.txt"), "{error}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// A `--remote` override reaches the same Git commands as configured remotes; an option-like
/// value must stop before any Git process runs.
#[test]
fn test_split_rejects_an_option_like_remote_override() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-remote-option")?;
        ws.add_crate("mylib", "0.1.0", &[])?;
        ws.commit("Add mylib")?;
        let split_dir = TempDir::new()?;
        git(split_dir.path(), &["init", "--initial-branch=main"])?;
        let config = format!(
            "[crates.mylib.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;
        let marker = ws.path.join("upload-pack-ran");
        let remote = format!("--remote=--upload-pack=touch {};", marker.display());

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "mylib", "--allow-dirty", "--check", &remote],
        )?;
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("--remote"),
            "{output:?}"
        );
        assert!(!marker.exists(), "Git ran the option-like remote");
        Ok(())
    })();
    super::helpers::finish_test(result);
}
