//! Integration tests for command-specific output format contracts.

use crate::helpers::{TestWorkspace, cargo_rail_command, run_cargo_rail};
use anyhow::Result;
use std::process::Stdio;

#[test]
fn test_unsupported_output_formats_fail_during_cli_parsing() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("output-format-domains")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;

        let cases: &[(&str, &[&str], &str)] = &[
            (
                "sync",
                &["rail", "sync", "lib-a", "--format", "github"],
                "[possible values: text, json]",
            ),
            (
                "release",
                &["rail", "release", "check", "lib-a", "--format", "jsonl"],
                "[possible values: text, json]",
            ),
            (
                "clean",
                &["rail", "clean", "--format", "names-only"],
                "[possible values: text, json]",
            ),
            (
                "config",
                &["rail", "config", "locate", "--format", "cargo-args"],
                "[possible values: text, json]",
            ),
            (
                "split",
                &["rail", "split", "run", "lib-a", "--format", "github"],
                "[possible values: text, json, names-only, jsonl]",
            ),
            (
                "change",
                &["rail", "change", "status", "--format", "github"],
                "[possible values: text, json, names-only]",
            ),
            (
                "unify",
                &["rail", "unify", "--check", "--format", "names-only"],
                "[possible values: text, json]",
            ),
        ];

        for (name, args, expected_values) in cases {
            let output = run_cargo_rail(&ws.path, args)?;
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                output.status.code(),
                Some(2),
                "{name} must reject an unsupported format during parsing. Stderr:\n{stderr}"
            );
            assert!(
                stderr.contains("invalid value") && stderr.contains(expected_values),
                "{name} should report its exact format domain. Stderr:\n{stderr}"
            );
        }

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_global_json_rejects_commands_without_structured_contracts() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("global-json-domains")?;
        let cases: &[(&str, &[&str])] = &[
            ("split init", &["rail", "--json", "split", "init"]),
            ("release init", &["rail", "--json", "release", "init"]),
            ("release resume", &["rail", "--json", "release", "resume", "state.json"]),
            ("release abort", &["rail", "--json", "release", "abort", "state.json"]),
            ("completions", &["rail", "--json", "completions", "bash"]),
        ];

        for (command, args) in cases {
            let output = run_cargo_rail(&ws.path, args)?;
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                output.status.code(),
                Some(2),
                "{command} should reject --json: {stderr}"
            );
            assert!(
                stderr.contains(&format!("--json is not supported by 'cargo rail {command}'")),
                "{command} should explain why --json was rejected: {stderr}"
            );
            assert!(
                output.stdout.is_empty(),
                "{command} should not write stdout on parse failure"
            );
        }

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_global_json_rejects_a_distinct_stream_protocol() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("global-json-stream-conflict")?;
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "--json", "change", "status", "--format", "names-only"],
        )?;
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("distinct '--format names-only' stream protocol"),
            "{stderr}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_surface_github_failure_preserves_the_raw_stream_boundary() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("surface-github-failure")?;
        let output = run_cargo_rail(&ws.path, &["rail", "surface", "--format", "github"])?;

        assert_eq!(output.status.code(), Some(2));
        assert!(
            output.stdout.is_empty(),
            "GitHub failure must not change stdout to another protocol: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8(output.stderr)?;
        assert!(
            stderr.contains("surface is unavailable in this source-built cargo-rail installation"),
            "GitHub failure must retain the operational diagnostic on stderr: {stderr}"
        );
        assert!(
            stderr.contains("cargo install does not provide surface"),
            "GitHub failure must retain recovery guidance on stderr: {stderr}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_unify_undo_global_json_is_one_complete_value() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("unify-undo-json")?;
        let output = run_cargo_rail(&ws.path, &["rail", "--json", "unify", "undo", "--list"])?;
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(value["command"], "unify");
        assert_eq!(value["mode"], "undo_list");
        assert_eq!(value["backups"], serde_json::json!([]));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_verbose_and_quiet_are_distinct_global_detail_levels() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("output-detail-levels")?;
        let output = run_cargo_rail(&ws.path, &["rail", "--quiet", "--verbose", "config", "locate"])?;
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_closed_stdout_pipe_is_a_clean_process_boundary() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("closed-stdout-pipe")?;
        std::fs::write(ws.path.join(".config/rail.toml"), "")?;
        let mut child = cargo_rail_command(&ws.path)?
            .args(["rail", "config", "explain", "--all"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        drop(child.stdout.take());
        let output = child.wait_with_output()?;
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_explain_default_is_compact_and_selection_is_complete() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-explain-detail")?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            "[unify]\nconsumer_scope = \"workspace\"\n",
        )?;

        let compact = run_cargo_rail(&ws.path, &["rail", "config", "explain"])?;
        let compact = String::from_utf8(compact.stdout)?;
        assert!(compact.contains("unify.consumer_scope = workspace"), "{compact}");
        assert!(!compact.contains("configured:"), "{compact}");
        assert!(!compact.contains("unify.msrv_policy.mode"), "{compact}");

        let selected = run_cargo_rail(&ws.path, &["rail", "config", "explain", "unify.consumer_scope"])?;
        let selected = String::from_utf8(selected.stdout)?;
        for label in [
            "configured:",
            "effective:",
            "default:",
            "source:",
            "classification:",
            "why:",
        ] {
            assert!(selected.contains(label), "missing {label}: {selected}");
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_removed_top_level_run_is_rejected_during_cli_parsing() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("removed-top-level-run")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;

        let output = run_cargo_rail(&ws.path, &["rail", "run"])?;
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("unrecognized subcommand 'run'"), "{stderr}");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_clean_check_json_has_stable_stream_and_exit_contract() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("clean-json-contract")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        let artifact = ws.path.join("target/cargo-rail/compiler-artifacts-v1/result");
        std::fs::create_dir_all(artifact.parent().expect("artifact has parent"))?;
        std::fs::write(&artifact, "{}")?;

        let output = run_cargo_rail(&ws.path, &["rail", "clean", "--cache", "--check", "--json"])?;
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stderr.is_empty(), "JSON check output must keep stderr empty");

        let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["command"], "clean");
        assert_eq!(value["mode"], "check");
        assert_eq!(value["result"], "pending_changes");
        assert_eq!(value["exit_code"], 1);
        assert_eq!(value["has_changes"], true);
        assert!(artifact.exists(), "check mode must not remove artifacts");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_text_separates_normal_verbose_and_targeted_proof() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("plan-text-detail")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        std::fs::write(ws.path.join("README.md"), "changed documentation\n")?;

        let normal = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD"])?;
        let normal = String::from_utf8(normal.stdout)?;
        assert!(normal.starts_with("Required work:\n"), "{normal}");
        assert!(!normal.contains("skipped work"), "{normal}");
        assert!(!normal.contains("Decision proof:"), "{normal}");

        let verbose = run_cargo_rail(&ws.path, &["rail", "--verbose", "plan", "--since", "HEAD"])?;
        let verbose = String::from_utf8(verbose.stdout)?;
        assert!(verbose.contains("Changed inputs:"), "{verbose}");
        assert!(verbose.contains("skipped work:"), "{verbose}");

        let targeted = run_cargo_rail(
            &ws.path,
            &["rail", "plan", "--since", "HEAD", "--explain-work", "cargo.fmt"],
        )?;
        let targeted = String::from_utf8(targeted.stdout)?;
        assert!(targeted.contains("Decision proof:"), "{targeted}");
        assert!(targeted.contains("cargo.fmt (skipped)"), "{targeted}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_bare_unify_previews_and_explicit_apply_mutates() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("unify-explicit-apply")?;
        ws.add_crate("lib-a", "0.1.0", &[("tempfile", r#""3.0""#)])?;
        ws.add_crate("lib-b", "0.1.0", &[("tempfile", r#""3.0""#)])?;
        ws.commit("Add shared dependency")?;
        let manifest = ws.path.join("Cargo.toml");
        let before = std::fs::read(&manifest)?;

        let preview = run_cargo_rail(&ws.path, &["rail", "unify"])?;
        assert!(preview.status.success(), "bare preview must exit 0: {preview:?}");
        assert_eq!(std::fs::read(&manifest)?, before);
        let preview_text = String::from_utf8(preview.stdout)?;
        assert!(preview_text.contains("Pending:"), "{preview_text}");
        assert!(preview_text.contains("Next: cargo rail unify apply"), "{preview_text}");

        let check = run_cargo_rail(&ws.path, &["rail", "unify", "--check"])?;
        assert_eq!(check.status.code(), Some(1));
        assert_eq!(std::fs::read(&manifest)?, before);

        let applied = run_cargo_rail(&ws.path, &["rail", "unify", "apply"])?;
        assert!(applied.status.success(), "explicit apply failed: {applied:?}");
        assert_ne!(std::fs::read(&manifest)?, before);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_clean_text_uses_iec_units_and_verbose_owns_paths() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("clean-text-detail")?;
        let artifact_root = ws.path.join("target/cargo-rail/compiler-artifacts-v1");
        let artifact = artifact_root.join("result");
        std::fs::create_dir_all(artifact.parent().expect("artifact parent"))?;
        std::fs::write(&artifact, vec![b'x'; 2048])?;
        let artifact_root = cargo_rail::utils::canonicalize_existing(&artifact_root)?;

        let normal = run_cargo_rail(&ws.path, &["rail", "clean", "--cache", "--check"])?;
        assert_eq!(normal.status.code(), Some(1));
        let normal = String::from_utf8(normal.stdout)?;
        assert!(normal.contains("2.0 KiB"), "{normal}");
        assert!(!normal.contains(artifact_root.to_string_lossy().as_ref()), "{normal}");

        let verbose = run_cargo_rail(&ws.path, &["rail", "--verbose", "clean", "--cache", "--check"])?;
        assert_eq!(verbose.status.code(), Some(1));
        let verbose = String::from_utf8(verbose.stdout)?;
        assert!(
            verbose.contains(artifact_root.to_string_lossy().as_ref()),
            "expected {} in:\n{verbose}",
            artifact_root.display()
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_root_help_groups_tasks_and_global_options() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("root-help-groups")?;
        let output = run_cargo_rail(&ws.path, &["rail", "--help"])?;
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout)?;
        for heading in [
            "Common inspection:",
            "Workspace mutation:",
            "Advanced and external operations:",
            "Global Options:",
        ] {
            assert!(help.contains(heading), "missing {heading}: {help}");
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_and_sync_share_the_empty_selection_contract() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-sync-empty-selection")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        std::fs::write(ws.path.join(".config/rail.toml"), "")?;

        for (command, expected) in [
            (
                &["rail", "split", "run", "--all", "--check"][..],
                "No split operations are configured.",
            ),
            (
                &["rail", "sync", "--all", "--check"][..],
                "No sync operations are configured.",
            ),
        ] {
            let output = run_cargo_rail(&ws.path, command)?;
            assert!(output.status.success(), "{output:?}");
            assert_eq!(String::from_utf8(output.stdout)?.trim(), expected);
        }

        for command in [
            &["rail", "split", "run", "--check"][..],
            &["rail", "sync", "--check"][..],
        ] {
            let output = run_cargo_rail(&ws.path, command)?;
            assert_eq!(output.status.code(), Some(2));
            assert!(String::from_utf8_lossy(&output.stderr).contains("must specify a crate name or use --all"));
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_change_add_human_output_names_the_created_path_and_bump() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("change-add-human-output")?;
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
                "Added a reviewed output contract.",
            ],
        )?;
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout)?;
        assert!(
            stdout
                .lines()
                .next()
                .is_some_and(|line| line.starts_with("Created .changes/")),
            "{stdout}"
        );
        assert!(stdout.contains("\nBump: lib-a minor\n"), "{stdout}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}
