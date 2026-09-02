//! Integration tests for `cargo rail config` commands (locate, print, validate, explain, migrate)

use crate::helpers::{TestWorkspace, cargo_rail_command, run_cargo_rail, run_cargo_rail_with_env};
use anyhow::Result;
use std::fs;
use std::io::Write as _;
use std::process::Stdio;

// Config Locate Tests

#[test]
fn test_config_locate_finds_config() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-locate-finds")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;

        // Run config locate
        let output = run_cargo_rail(&ws.path, &["rail", "config", "locate"])?;

        // Verify success
        assert!(output.status.success(), "config locate should succeed");

        // Verify output contains path
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("rail.toml"), "output should contain config path");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_locate_no_config() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-locate-no-config")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        ws.remove_config()?;

        // Run config locate
        let output = run_cargo_rail(&ws.path, &["rail", "config", "locate"])?;

        // Absence is a successful query result, not an operational failure.
        assert!(
            output.status.success(),
            "config locate should report absence successfully"
        );

        // Verify helpful message
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("no config file found"),
            "output should say no config found"
        );
        assert!(stdout.contains("cargo rail init"), "output should suggest running init");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_locate_json_output() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-locate-json")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;

        // Run config locate with JSON format
        let output = run_cargo_rail(&ws.path, &["rail", "config", "locate", "-f", "json"])?;

        // Verify success
        assert!(output.status.success(), "config locate -f json should succeed");

        // Verify JSON output
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&stdout)?;

        assert_eq!(json["command"], "config");
        assert_eq!(json["action"], "locate");
        assert_eq!(json["found"], true);
        let path = json["path"].as_str().expect("path should be a string");
        assert!(path.ends_with("rail.toml"), "path should point to rail.toml");
        let search_paths = json["search_paths"]
            .as_array()
            .expect("search_paths should be an array");
        assert!(
            !search_paths.is_empty(),
            "search_paths should include checked config locations"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_locate_with_config_flag() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-locate-with-flag")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;

        // Create a custom config file
        let custom_config = ws.path.join("custom-rail.toml");
        fs::write(&custom_config, "targets = []\n")?;

        // Run config locate with --config flag
        let output = run_cargo_rail(&ws.path, &["rail", "--config", "custom-rail.toml", "config", "locate"])?;

        // Verify success
        assert!(output.status.success(), "config locate with --config should succeed");

        // Verify output contains the custom path
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("custom-rail.toml"),
            "output should contain custom config path"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// Config Print Tests

fn config_print_body(output: &str) -> &str {
    output
        .split_once("\n\n")
        .map(|(_, body)| body)
        .expect("text config output must separate its provenance header from canonical TOML")
}

#[test]
fn test_config_print_shows_defaults() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-print-defaults")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;

        // Create minimal config
        let config_path = ws.path.join(".config").join("rail.toml");
        fs::write(&config_path, "targets = []\n")?;

        // Run config print
        let output = run_cargo_rail(&ws.path, &["rail", "config", "print"])?;

        // Verify success
        assert!(output.status.success(), "config print should succeed");

        // Verify output shows defaults
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("# Effective configuration"),
            "output should have header comment"
        );
        assert!(stdout.contains("[unify]"), "output should contain [unify] section");
        assert!(stdout.contains("msrv"), "output should contain default msrv setting");
        assert!(stdout.contains("[release]"), "output should contain [release] section");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_print_json_output() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-print-json")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;

        // Run config print with JSON format
        let output = run_cargo_rail(&ws.path, &["rail", "config", "print", "-f", "json"])?;

        // Verify success
        assert!(output.status.success(), "config print -f json should succeed");

        // Verify JSON output
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&stdout)?;

        assert_eq!(json["command"], "config");
        assert_eq!(json["action"], "print");
        let config_path = json["config_path"].as_str().expect("config_path should be a string");
        assert!(
            config_path.ends_with("rail.toml"),
            "config_path should point to rail.toml"
        );
        assert!(json["config"].is_object());
        assert!(json["config"]["unify"].is_object());

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_print_emits_canonical_strictly_valid_configuration() {
    let result: Result<()> = (|| {
        let fixtures = [
            ("empty", ""),
            ("minimal", "targets = [\"x86_64-unknown-linux-gnu\"]\n"),
            (
                "customized",
                r#"targets = ["x86_64-unknown-linux-gnu"]

[unify]
include_paths = false
include_renamed = true
transitive_pinning = { host = "root" }
exclude = ["platform-only"]
include = ["serde"]
max_backups = 7
compiler_artifact_soft_limit_bytes = 1024
compiler_artifact_hard_limit_bytes = 2048
msrv_policy = { mode = "compute", source = "workspace", inherit = true }
consumer_scope = "workspace"
preserve_features = ["unstable-*"]
strict_version_compat = false
exact_pin_handling = "preserve"
major_version_conflict = "bump"
skip_undeclared_patterns = ["default"]

[surface]
enabled = true
consumer_scope = "workspace"
targets = ["host", "x86_64-unknown-linux-gnu"]
crate_visibility = "allow"
preserve_uniform_fields = true
doctest_coverage = "disabled"

[[surface.lint]]
selector = "warnings"
level = "warn"

[release]
tag_prefix = "release-"
tag_format = "{crate}-{version}"
remote_effects = "push"
sign_tags = true
change_dir = ".release-intent"
pre_1_breaking_bump = "major"
semver_check = "deny"
version_groups = { core = ["test-crate"] }

"#,
            ),
            (
                "optional",
                r#"[unify]
transitive_pinning = { host = "root" }

[crates.test-crate.release]
publish = false

[crates.test-crate.changelog]
path = "HISTORY.md"
"#,
            ),
        ];

        for (name, input) in fixtures {
            let ws = TestWorkspace::new_named(&format!("config-print-canonical-{name}"))?;
            ws.add_crate("test-crate", "0.1.0", &[])?;
            ws.commit("Add test crate")?;
            fs::write(ws.path.join(".config/rail.toml"), input)?;

            let printed = run_cargo_rail(&ws.path, &["rail", "config", "print"])?;
            assert!(printed.status.success(), "{name}: config print failed: {printed:?}");
            let printed_text = String::from_utf8(printed.stdout)?;
            let original_json = run_cargo_rail(&ws.path, &["rail", "config", "print", "-f", "json"])?;
            assert!(original_json.status.success(), "{name}: JSON config print failed");
            let original_json: serde_json::Value = serde_json::from_slice(&original_json.stdout)?;

            let canonical_name = format!("canonical-{name}.toml");
            fs::write(ws.path.join(&canonical_name), &printed_text)?;
            let validated = run_cargo_rail(
                &ws.path,
                &["rail", "--config", &canonical_name, "config", "validate", "--strict"],
            )?;
            assert!(
                validated.status.success(),
                "{name}: printed config failed strict validation: {validated:?}"
            );

            let canonical_json = run_cargo_rail(
                &ws.path,
                &["rail", "--config", &canonical_name, "config", "print", "-f", "json"],
            )?;
            assert!(canonical_json.status.success(), "{name}: canonical JSON print failed");
            let canonical_json: serde_json::Value = serde_json::from_slice(&canonical_json.stdout)?;
            assert_eq!(
                original_json["config"], canonical_json["config"],
                "{name}: TOML and JSON projections changed effective public policy"
            );

            let repeated = run_cargo_rail(&ws.path, &["rail", "--config", &canonical_name, "config", "print"])?;
            assert!(repeated.status.success(), "{name}: repeated config print failed");
            let repeated_text = String::from_utf8(repeated.stdout)?;
            assert_eq!(
                config_print_body(&printed_text),
                config_print_body(&repeated_text),
                "{name}: repeated print changed canonical policy"
            );
        }

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_print_no_config() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-print-no-config")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        ws.remove_config()?;

        // Run config print
        let output = run_cargo_rail(&ws.path, &["rail", "config", "print"])?;

        // Verify failure
        assert!(!output.status.success(), "config print should fail without config");

        // Should suggest running init
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("cargo rail init"),
            "should suggest running 'cargo rail init'"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// Config Validate Tests

#[test]
fn test_config_validate_valid_config() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-validate-valid")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;

        // Run config validate
        let output = run_cargo_rail(&ws.path, &["rail", "config", "validate"])?;

        // Verify success
        assert!(
            output.status.success(),
            "config validate should succeed with valid config"
        );

        // Verify output contains success message
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("configuration is valid"),
            "output should confirm valid config"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_validate_accepts_empty_config() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-validate-empty")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;

        let config_path = ws.path.join(".config").join("rail.toml");
        fs::write(&config_path, "")?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--strict", "-f", "json"])?;
        assert!(output.status.success(), "an empty rail.toml must be valid");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&stdout)?;
        assert_eq!(json["valid"], true);
        assert_eq!(json["errors"], serde_json::Value::Array(vec![]));
        assert_eq!(json["warnings"], serde_json::Value::Array(vec![]));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_validate_no_config() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-validate-no-config")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        ws.remove_config()?;

        // Run config validate
        let output = run_cargo_rail(&ws.path, &["rail", "config", "validate"])?;

        // Verify failure
        assert!(!output.status.success(), "config validate should fail without config");

        // Verify helpful message
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stdout.contains("no configuration file found") || stderr.contains("no configuration file found"),
            "output should say no config found. stdout: {}, stderr: {}",
            stdout,
            stderr
        );
        assert!(
            stdout.contains("cargo rail init") || stderr.contains("cargo rail init"),
            "output should suggest running init"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_validate_json_output() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-validate-json")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;

        // Run config validate with JSON format
        let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "-f", "json"])?;

        // Verify success
        assert!(output.status.success(), "config validate -f json should succeed");

        // Verify JSON output
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&stdout)?;

        assert_eq!(json["command"], "config");
        assert_eq!(json["action"], "validate");
        assert_eq!(json["valid"], true);
        let config_path = json["config_path"].as_str().expect("config_path should be a string");
        assert!(
            config_path.ends_with("rail.toml"),
            "config_path should point to rail.toml"
        );
        let errors = json["errors"].as_array().expect("errors should be an array");
        let warnings = json["warnings"].as_array().expect("warnings should be an array");
        assert!(errors.is_empty(), "valid config should have no errors");
        assert!(warnings.is_empty(), "valid config should have no warnings");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_validate_rejects_invalid_unify_glob() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-validate-invalid-unify-glob")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;

        fs::write(
            ws.path.join(".config/rail.toml"),
            "[unify]\npreserve_features = [\"[\"]\n",
        )?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "-f", "json"])?;
        assert!(!output.status.success(), "invalid unify glob should fail validation");

        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(json["valid"], false);
        assert!(
            json["errors"]
                .as_array()
                .is_some_and(|errors| errors.iter().any(|error| {
                    error["message"].as_str().is_some_and(|message| {
                        message.contains("invalid glob pattern") && message.contains("unify.preserve_features")
                    })
                }))
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_validate_rejects_empty_split_branch() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-validate-empty-split-branch")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;

        fs::write(
            ws.path.join(".config/rail.toml"),
            r#"[crates.test-crate.split]
remote = "https://example.invalid/test-crate.git"
branch = ""
mode = "single"
members = ["test-crate"]
"#,
        )?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "-f", "json"])?;
        assert!(!output.status.success(), "empty split branch should fail validation");

        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(json["valid"], false);
        assert!(
            json["errors"]
                .as_array()
                .is_some_and(|errors| errors.iter().any(|error| {
                    error["message"]
                        .as_str()
                        .is_some_and(|message| message == "branch must not be empty")
                }))
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_validate_no_config_json() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-validate-no-config-json")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        ws.remove_config()?;

        // Run config validate with JSON format
        let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "-f", "json"])?;

        // Verify failure (exit code 2)
        assert!(!output.status.success());

        // Verify JSON output shows error
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&stdout)?;

        assert_eq!(json["command"], "config");
        assert_eq!(json["valid"], false);
        assert!(json["config_path"].is_null());
        let errors = json["errors"].as_array().unwrap();
        assert!(!errors.is_empty());
        assert!(
            errors[0]["message"]
                .as_str()
                .unwrap()
                .contains("no configuration file found")
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_validate_global_json_flag() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-validate-global-json")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;

        // Run config validate with global --json flag
        let output = run_cargo_rail(&ws.path, &["rail", "--json", "config", "validate"])?;

        // Verify success
        assert!(output.status.success(), "config validate with --json should succeed");

        // Verify JSON output
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&stdout)?;

        assert_eq!(json["command"], "config");
        assert_eq!(json["valid"], true);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_validate_with_config_flag() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-validate-with-flag")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        ws.remove_config()?;

        let custom_config = ws.path.join("custom-rail.toml");
        fs::write(&custom_config, "targets = []\n")?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "--config", "custom-rail.toml", "config", "validate"],
        )?;
        assert!(output.status.success(), "config validate with --config should succeed");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("custom-rail.toml"),
            "should validate override config path"
        );
        assert!(
            stdout.contains("configuration is valid"),
            "output should confirm valid config"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_structural_dynamic_keys_round_trip_and_validate_strictly() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-structural-dynamic-keys")?;
        ws.add_crate("cli-tools", "0.1.0", &[])?;
        ws.commit("Add dynamic-key fixture")?;
        fs::write(
            ws.path.join(".config/rail.toml"),
            r#"[plan.work."docs.generated"]
scope = "repository"
paths = ["docs/**"]

[crates."cli-tools".release]
publish = false
"#,
        )?;

        let validated = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--strict", "-f", "json"])?;
        assert!(
            validated.status.success(),
            "dynamic keys failed strict validation: {validated:?}"
        );
        let explained = run_cargo_rail(&ws.path, &["rail", "config", "explain", "-f", "json"])?;
        assert!(
            explained.status.success(),
            "dynamic keys failed explanation: {explained:?}"
        );
        let explained: serde_json::Value = serde_json::from_slice(&explained.stdout)?;
        assert!(explained["fields"].as_array().is_some_and(|fields| {
            fields
                .iter()
                .any(|field| field["path"] == "plan.work.\"docs.generated\".paths")
        }));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_canonical_config_print_validates_from_stdin() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-validate-stdin")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add stdin validation fixture")?;
        let printed = run_cargo_rail(&ws.path, &["rail", "config", "print"])?;
        assert!(printed.status.success(), "canonical print failed: {printed:?}");

        let mut child = cargo_rail_command(&ws.path)?
            .args(["rail", "--config", "-", "config", "validate", "--strict", "-f", "json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child.stdin.take().expect("piped stdin").write_all(&printed.stdout)?;
        let validated = child.wait_with_output()?;
        assert!(validated.status.success(), "stdin validation failed: {validated:?}");
        let value: serde_json::Value = serde_json::from_slice(&validated.stdout)?;
        assert_eq!(value["valid"], true);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_unknown_keys_fail_normal_loading_even_without_strict_validation() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-unknown-normal-load")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add unknown-key fixture")?;
        fs::write(ws.path.join(".config/rail.toml"), "targtes = []\n")?;

        for args in [
            &["rail", "config", "validate", "--no-strict"][..],
            &["rail", "config", "print"][..],
            &["rail", "plan", "--since", "HEAD"][..],
        ] {
            let output = run_cargo_rail(&ws.path, args)?;
            assert_eq!(output.status.code(), Some(2), "unknown key accepted by {args:?}");
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(combined.contains("unknown configuration key 'targtes'"), "{combined}");
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_semantic_config_failures_match_validation_and_plan_consumers() {
    let result: Result<()> = (|| {
        let fixtures = [
            (
                "planner-id",
                "[plan.work.Invalid]\nscope = \"repository\"\npaths = [\"docs/**\"]\n",
                "must match [a-z][a-z0-9.-]*",
            ),
            (
                "release",
                "[release]\ntag_format = \"\"\n",
                "tag_format cannot be empty",
            ),
            (
                "split",
                "[crates.test-crate.split]\nremote = \"https://example.invalid/repo.git\"\nbranch = \"\"\nmode = \"single\"\nmembers = [\"test-crate\"]\n",
                "branch must not be empty",
            ),
            (
                "target",
                "targets = [\"definitely-not-a-rust-target\"]\n",
                "invalid target triple",
            ),
        ];
        for (name, config, needle) in fixtures {
            let ws = TestWorkspace::new_named(&format!("config-shared-validator-{name}"))?;
            ws.add_crate("test-crate", "0.1.0", &[])?;
            ws.commit("Add shared-validator fixture")?;
            fs::write(ws.path.join(".config/rail.toml"), config)?;

            for args in [
                &["rail", "config", "validate", "--no-strict"][..],
                &["rail", "plan", "--since", "HEAD"][..],
            ] {
                let output = run_cargo_rail(&ws.path, args)?;
                assert_eq!(output.status.code(), Some(2), "{name} was accepted by {args:?}");
                let combined = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(combined.contains(needle), "{name} via {args:?}: {combined}");
            }
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_validate_with_missing_config_flag_fails() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-validate-with-missing-flag")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        ws.remove_config()?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "--config", "missing.toml", "config", "validate", "-f", "json"],
        )?;
        assert!(!output.status.success(), "validate with missing --config should fail");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&stdout)?;
        assert_eq!(json["valid"], false);
        let errors = json["errors"].as_array().unwrap();
        assert!(
            errors
                .iter()
                .filter_map(|e| e["message"].as_str())
                .any(|msg| msg.contains("specified config file not found")),
            "expected missing override error. Output:\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_explain_json_reports_effective_default_and_source() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-explain-json")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;

        let config_path = ws.path.join(".config").join("rail.toml");
        fs::write(&config_path, "[unify]\nmsrv_policy = { mode = \"disabled\" }\n")?;

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "config",
                "explain",
                "unify.msrv_policy.mode",
                "unify.consumer_scope",
                "--json",
            ],
        )?;
        assert!(output.status.success(), "config explain should succeed");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(json["command"], "config");
        assert_eq!(json["action"], "explain");

        let fields = json["fields"].as_array().expect("fields array");
        let msrv = fields
            .iter()
            .find(|field| field["path"] == "unify.msrv_policy.mode")
            .expect("unify.msrv_policy.mode explanation");
        assert_eq!(msrv["effective"], "disabled");
        assert_eq!(msrv["default"], "compute");
        assert_eq!(msrv["source"], json["config_path"]);
        assert_eq!(msrv["classification"], "project_policy");
        assert!(msrv["why"].as_str().is_some_and(|why| !why.is_empty()));

        let consumer_scope = fields
            .iter()
            .find(|field| field["path"] == "unify.consumer_scope")
            .expect("unify.consumer_scope explanation");
        assert_eq!(consumer_scope["effective"], "open");
        assert_eq!(consumer_scope["default"], "open");
        assert_eq!(consumer_scope["source"], "default");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_workspace_surface_targets_are_materialized_with_provenance() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-surface-workspace-targets")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        fs::write(
            ws.path.join(".config/rail.toml"),
            r#"targets = ["aarch64-unknown-linux-gnu", "wasm32-wasip1"]

[surface]
targets = "workspace"
"#,
        )?;

        let printed = run_cargo_rail(&ws.path, &["rail", "config", "print", "-f", "json"])?;
        assert!(printed.status.success(), "config print failed: {printed:?}");
        let printed: serde_json::Value = serde_json::from_slice(&printed.stdout)?;
        assert_eq!(
            printed["config"]["surface"]["targets"],
            serde_json::json!(["host", "aarch64-unknown-linux-gnu", "wasm32-wasip1"])
        );

        let explained = run_cargo_rail(&ws.path, &["rail", "config", "explain", "-f", "json"])?;
        assert!(explained.status.success(), "config explain failed: {explained:?}");
        let explained: serde_json::Value = serde_json::from_slice(&explained.stdout)?;
        let targets = explained["fields"]
            .as_array()
            .and_then(|fields| fields.iter().find(|field| field["path"] == "surface.targets"))
            .expect("surface.targets explanation");
        assert_eq!(targets["configured"], "workspace");
        assert_eq!(
            targets["effective"],
            serde_json::json!(["host", "aarch64-unknown-linux-gnu", "wasm32-wasip1"])
        );
        assert!(
            targets["source"]
                .as_str()
                .is_some_and(|source| source.ends_with("rail.toml (inherited from targets)"))
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_explicit_surface_target_subset_does_not_inherit_new_targets() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-surface-explicit-targets")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        fs::write(
            ws.path.join(".config/rail.toml"),
            r#"targets = ["aarch64-unknown-linux-gnu", "wasm32-wasip1"]

[surface]
targets = ["host", "wasm32-wasip1"]
"#,
        )?;

        let printed = run_cargo_rail(&ws.path, &["rail", "config", "print", "-f", "json"])?;
        assert!(printed.status.success(), "config print failed: {printed:?}");
        let printed: serde_json::Value = serde_json::from_slice(&printed.stdout)?;
        assert_eq!(
            printed["config"]["surface"]["targets"],
            serde_json::json!(["host", "wasm32-wasip1"])
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_validate_rejects_surface_target_outside_workspace_policy() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-surface-unknown-target")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        fs::write(
            ws.path.join(".config/rail.toml"),
            r#"targets = ["aarch64-unknown-linux-gnu"]

[surface]
targets = ["wasm32-wasip1"]
"#,
        )?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--strict", "-f", "json"])?;
        assert_eq!(output.status.code(), Some(2));
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(json["valid"], false);
        assert!(
            json["errors"]
                .as_array()
                .is_some_and(|errors| errors.iter().any(|error| {
                    error["section"] == "surface"
                        && error["message"]
                            .as_str()
                            .is_some_and(|message| message.contains("not declared in top-level targets"))
                }))
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_validate_rejects_unknown_and_duplicate_unify_compiler_targets() {
    let result: Result<()> =
        (|| {
            let ws = TestWorkspace::new_named("config-unify-compiler-targets")?;
            ws.add_crate("test-crate", "0.1.0", &[])?;
            ws.commit("Add test crate")?;

            for (configured, expected) in [
                ("[\"wasm32-wasip1\"]", "not declared in top-level targets"),
                (
                    "[\"aarch64-unknown-linux-gnu\", \"aarch64-unknown-linux-gnu\"]",
                    "contains duplicate target",
                ),
            ] {
                fs::write(
                    ws.path.join(".config/rail.toml"),
                    format!("targets = [\"aarch64-unknown-linux-gnu\"]\n\n[unify]\ncompiler_targets = {configured}\n"),
                )?;
                let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--strict", "-f", "json"])?;
                assert_eq!(output.status.code(), Some(2), "invalid compiler target was accepted");
                let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
                assert_eq!(report["valid"], false);
                assert!(
                    report["errors"]
                        .as_array()
                        .is_some_and(
                            |errors| errors.iter().any(|error| error["message"].as_str().is_some_and(
                                |message| message.contains("unify.compiler_targets") && message.contains(expected)
                            ))
                        ),
                    "missing exact compiler-target validation error: {report}"
                );
            }

            Ok(())
        })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_explain_text_uses_same_field_values_as_json() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-explain-text")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        fs::write(
            ws.path.join(".config/rail.toml"),
            "[unify]\nmsrv_policy = { mode = \"disabled\" }\n",
        )?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "explain", "unify.msrv_policy.mode"])?;
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("unify.msrv_policy.mode"));
        assert!(stdout.contains("effective: disabled"));
        assert!(stdout.contains("default: compute"));
        assert!(stdout.contains("source:"));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_migrate_checks_and_applies_exact_v0_25_configuration_losslessly() {
    let result: Result<()> = (|| {
        const TAGGED_CONFIG: &[u8] = include_bytes!("../fixtures/config/v0.25.0/rail.toml");

        let ws = TestWorkspace::new_named("config-migrate-v0-25")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        let config_path = ws.path.join(".config/rail.toml");
        fs::write(&config_path, TAGGED_CONFIG)?;

        let checked = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "--check", "-f", "json"])?;
        assert_eq!(checked.status.code(), Some(1));
        assert_eq!(fs::read(&config_path)?, TAGGED_CONFIG, "check mode modified rail.toml");
        let checked: serde_json::Value = serde_json::from_slice(&checked.stdout)?;
        assert_eq!(checked["command"], "config");
        assert_eq!(checked["action"], "migrate");
        assert_eq!(checked["result"], "pending_changes");
        assert_eq!(checked["has_changes"], true);

        let applied = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "-f", "json"])?;
        assert!(applied.status.success(), "migration failed: {applied:?}");
        let applied_json: serde_json::Value = serde_json::from_slice(&applied.stdout)?;
        #[cfg(unix)]
        {
            let previous_config = applied_json["previous_config"]
                .as_str()
                .expect("Unix migration output must name the preserved previous configuration");
            assert_eq!(fs::read(previous_config)?, TAGGED_CONFIG);
        }
        #[cfg(windows)]
        assert!(applied_json.get("previous_config").is_none());
        let migrated = fs::read_to_string(&config_path)?;
        assert!(migrated.starts_with("# Repository policy. Omitted fields use cargo-rail's coded defaults.\n"));
        assert!(migrated.contains("[plan.work.compatibility]"));
        assert!(migrated.contains("cargo = [\"cargo.build\", \"cargo.test\"]"));
        assert!(!migrated.contains("require_changelog_entries"));
        assert!(!migrated.contains("require_change_files"));
        assert!(!migrated.contains("unconventional_commits"));
        assert!(!migrated.contains("[release.changelog.filters]"));

        let validated = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--strict", "-f", "json"])?;
        assert!(
            validated.status.success(),
            "migrated tagged configuration did not validate: {}",
            String::from_utf8_lossy(&validated.stdout)
        );
        let rechecked = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "--check"])?;
        assert!(
            rechecked.status.success(),
            "migration was not idempotent: {rechecked:?}"
        );
        assert_eq!(fs::read_to_string(&config_path)?, migrated);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_migrate_rejects_unknown_v0_25_shaped_input_without_writing() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-migrate-v0-25-unknown")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        let config_path = ws.path.join(".config/rail.toml");
        let original = b"[release]\nrequire_change_files = true\nrequire_change_fiels = true\n";
        fs::write(&config_path, original)?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("release.require_change_fiels"),
            "unexpected diagnostic: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(&config_path)?, original);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_migrate_removes_inline_and_reserved_v0_25_inputs() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-migrate-v0-25-inline")?;
        ws.add_crate("demo", "0.1.0", &[])?;
        let config_path = ws.path.join(".config/rail.toml");
        let original = r#"workspace = { root = "." }
toolchain = { channel = "stable" }
release = { tag_format = "{prefix}{version}", changelog = { path = "CHANGELOG.md", emoji = false } }
crates = { demo = { changelog = { path = "CHANGES.md", emoji = false }, sync = { arbitrary = "ignored by v0.25.0" } } }
"#;
        fs::write(&config_path, original)?;

        let checked = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "--check"])?;
        assert_eq!(checked.status.code(), Some(1), "{checked:?}");
        assert_eq!(fs::read_to_string(&config_path)?, original);

        let applied = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
        assert!(
            applied.status.success(),
            "inline migration failed: {}",
            String::from_utf8_lossy(&applied.stderr)
        );
        let migrated = fs::read_to_string(&config_path)?;
        assert!(!migrated.contains("workspace"));
        assert!(!migrated.contains("toolchain"));
        assert!(!migrated.contains("emoji"));
        assert!(!migrated.contains("sync"));
        assert!(migrated.contains("tag_format = \"{prefix}{version}\""));
        assert!(migrated.contains("path = \"CHANGES.md\""));

        let validated = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--strict"])?;
        assert!(validated.status.success(), "{validated:?}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_migrate_rejects_invalid_or_conflicting_v0_25_values_without_writing() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-migrate-v0-25-types")?;
        ws.add_crate("demo", "0.1.0", &[])?;
        let config_path = ws.path.join(".config/rail.toml");
        for original in [
            "[release]\nrequire_clean = \"yes\"\n",
            "[release]\nremote_effects = \"none\"\npush = false\n",
            "[release]\npush = false\nforge = \"bogus\"\n",
            "[release]\nchangelog = { emoji = \"yes\" }\n",
            "[unify]\ntransitive_pinning = { host = \"root\" }\npin_transitives = false\n",
        ] {
            fs::write(&config_path, original)?;
            let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
            assert_eq!(
                output.status.code(),
                Some(2),
                "invalid input was accepted: {original}\n{output:?}"
            );
            assert_eq!(fs::read_to_string(&config_path)?, original);
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_migrate_replaces_split_paths_with_cargo_member_names() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-migrate-split-members")?;
        ws.add_crate("member-a", "0.1.0", &[])?;
        let config_path = ws.path.join(".config/rail.toml");
        let original = r#"# preserve this comment
[crates.bundle.split]
remote = "../bundle"
branch = "main"
mode = "single"
paths = [{ crate = "crates/member-a" }]
"#;
        fs::write(&config_path, original)?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
        assert!(
            output.status.success(),
            "split migration failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let migrated = fs::read_to_string(config_path)?;
        assert!(migrated.starts_with("# preserve this comment\n"));
        assert!(migrated.contains("members = [\"member-a\"]"));
        assert!(!migrated.contains("paths ="));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_migrate_accepts_curdir_split_member_paths() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_named("config-migrate-split-curdir")?;
        workspace.add_crate("member-a", "0.1.0", &[])?;
        let config_path = workspace.path.join(".config/rail.toml");
        fs::write(
            &config_path,
            "[crates.bundle.split]\nremote = \"../bundle\"\nbranch = \"main\"\nmode = \"single\"\npaths = [{ crate = \"./crates/member-a\" }]\n",
        )?;
        let applied = run_cargo_rail(&workspace.path, &["rail", "config", "migrate"])?;
        assert!(applied.status.success(), "{applied:?}");
        assert!(fs::read_to_string(&config_path)?.contains("members = [\"member-a\"]"));

        let root_package = TestWorkspace::new_single_crate("root-package", "0.1.0")?;
        let root_config = root_package.path.join(".config/rail.toml");
        fs::write(
            &root_config,
            "[crates.root-package.split]\nremote = \"../root-package-split\"\nbranch = \"main\"\nmode = \"single\"\npaths = [{ crate = \".\" }]\n",
        )?;
        let applied = run_cargo_rail(&root_package.path, &["rail", "config", "migrate"])?;
        assert!(applied.status.success(), "{applied:?}");
        assert!(fs::read_to_string(root_config)?.contains("members = [\"root-package\"]"));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_migrate_accepts_array_of_tables_split_member_paths() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_named("config-migrate-split-array-of-tables")?;
        workspace.add_crate("member-a", "0.1.0", &[])?;
        let config_path = workspace.path.join(".config/rail.toml");
        let original = r#"[crates.bundle.split]
remote = "../bundle"
branch = "main"
mode = "single"

[[crates.bundle.split.paths]]
crate = "crates/member-a"
"#;
        fs::write(&config_path, original)?;

        let checked = run_cargo_rail(&workspace.path, &["rail", "config", "migrate", "--check"])?;
        assert_eq!(checked.status.code(), Some(1), "{checked:?}");
        assert_eq!(fs::read_to_string(&config_path)?, original);

        let applied = run_cargo_rail(&workspace.path, &["rail", "config", "migrate"])?;
        assert!(applied.status.success(), "{applied:?}");
        let migrated = fs::read_to_string(&config_path)?;
        assert!(migrated.contains("members = [\"member-a\"]"));
        assert!(!migrated.contains("[[crates.bundle.split.paths]]"));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_migrate_accepts_empty_root_split_member_path() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("root-package", "0.1.0")?;
        let config_path = workspace.path.join(".config/rail.toml");
        let original = "[crates.root-package.split]\nremote = \"../root-package-split\"\nbranch = \"main\"\nmode = \"single\"\npaths = [{ crate = \"\" }]\n";
        fs::write(&config_path, original)?;

        let checked = run_cargo_rail(&workspace.path, &["rail", "config", "migrate", "--check"])?;
        assert_eq!(checked.status.code(), Some(1), "{checked:?}");
        assert_eq!(fs::read_to_string(&config_path)?, original);

        let applied = run_cargo_rail(&workspace.path, &["rail", "config", "migrate"])?;
        assert!(applied.status.success(), "{applied:?}");
        let migrated = fs::read_to_string(&config_path)?;
        assert!(migrated.contains("members = [\"root-package\"]"));
        assert!(!migrated.contains("paths ="));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_migrate_release_binary_ignores_legacy_fault_environment() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-migrate-inert-fault-environment")?;
        ws.add_crate("member-a", "0.1.0", &[])?;
        let config_path = ws.path.join(".config/rail.toml");
        let original = b"[release]\nrequire_clean = true\n";
        fs::write(&config_path, original)?;
        let member_manifest = ws.path.join("crates/member-a/Cargo.toml");
        let original_manifest = fs::read(&member_manifest)?;

        let output = run_cargo_rail_with_env(
            &ws.path,
            &["rail", "config", "migrate"],
            &[
                ("CARGO_RAIL_TEST_CONFIG_MIGRATE_EDIT_AFTER_SPLIT_REVALIDATION", "1"),
                (
                    "CARGO_RAIL_TEST_CONFIG_MIGRATE_EDIT_SPLIT_MANIFEST_AFTER_TEMP_WRITE",
                    "1",
                ),
                ("CARGO_RAIL_TEST_CONFIG_MIGRATE_SUBSTITUTE_PARENT_AFTER_TEMP_WRITE", "1"),
                (
                    "CARGO_RAIL_TEST_CONFIG_MIGRATE_REPLACE_DESTINATION_AFTER_FINAL_REVALIDATION",
                    "1",
                ),
                (
                    "CARGO_RAIL_TEST_CONFIG_MIGRATE_SUBSTITUTE_PARENT_AFTER_FINAL_REVALIDATION",
                    "1",
                ),
                (
                    "CARGO_RAIL_TEST_CONFIG_MIGRATE_SWAP_SPLIT_ANCESTOR_AFTER_FINAL_REVALIDATION",
                    "1",
                ),
                (
                    "CARGO_RAIL_TEST_CONFIG_MIGRATE_EDIT_METADATA_AFTER_FINAL_REVALIDATION",
                    "1",
                ),
                ("CARGO_RAIL_TEST_CONFIG_MIGRATE_EDIT_ACL_AFTER_FINAL_REVALIDATION", "1"),
                (
                    "CARGO_RAIL_TEST_CONFIG_MIGRATE_EDIT_MEMBERSHIP_AFTER_FINAL_REVALIDATION",
                    "1",
                ),
                ("CARGO_RAIL_TEST_CONFIG_MIGRATE_REPLACE_AFTER_PUBLICATION", "1"),
                ("CARGO_RAIL_TEST_CONFIG_MIGRATE_EDIT_BYTES_AFTER_PUBLICATION", "1"),
                ("CARGO_RAIL_TEST_CONFIG_MIGRATE_EDIT_METADATA_AFTER_PUBLICATION", "1"),
            ],
        )?;
        assert!(output.status.success(), "{output:?}");
        assert!(!fs::read_to_string(&config_path)?.contains("require_clean"));
        assert_eq!(fs::read(&member_manifest)?, original_manifest);
        assert!(ws.path.join(".config").is_dir());
        assert!(!ws.path.join(".config.cargo-rail-test-original").exists());
        assert!(!ws.path.join(".config.cargo-rail-test-after-final").exists());
        let migration_artifacts = fs::read_dir(config_path.parent().expect("configuration has a parent"))?
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".cargo-rail-config-migrate-")
            })
            .count();
        #[cfg(unix)]
        assert_eq!(migration_artifacts, 1, "Unix keeps exactly one previous configuration");
        #[cfg(windows)]
        assert_eq!(migration_artifacts, 0, "Windows removes its exact private backup");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_config_migrate_preserves_destination_mode() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_named("config-migrate-preserves-mode")?;
        workspace.add_crate("test-crate", "0.1.0", &[])?;
        let config_path = workspace.path.join(".config/rail.toml");
        fs::write(&config_path, "[release]\nrequire_clean = true\n")?;
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o640))?;

        let output = run_cargo_rail(&workspace.path, &["rail", "config", "migrate"])?;
        assert!(output.status.success(), "{output:?}");
        assert_eq!(fs::metadata(config_path)?.mode() & 0o7777, 0o640);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_config_migrate_rejects_linked_split_manifest_without_writing() {
    use std::os::unix::fs::symlink;

    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-migrate-linked-split-manifest")?;
        ws.add_crate("member-a", "0.1.0", &[])?;
        let outside = tempfile::TempDir::new()?;
        let outside_manifest = outside.path().join("Cargo.toml");
        fs::write(
            &outside_manifest,
            "[package]\nname = \"outside\"\nversion = \"0.1.0\"\n",
        )?;
        let member_manifest = ws.path.join("crates/member-a/Cargo.toml");
        fs::remove_file(&member_manifest)?;
        symlink(&outside_manifest, &member_manifest)?;

        let config_path = ws.path.join(".config/rail.toml");
        let original = b"[crates.bundle.split]\nremote = \"../bundle\"\nbranch = \"main\"\nmode = \"single\"\npaths = [{ crate = \"crates/member-a\" }]\n";
        fs::write(&config_path, original)?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("regular, non-symlink file"),
            "unexpected diagnostic: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(config_path)?, original);
        assert_eq!(
            fs::read_to_string(outside_manifest)?.lines().nth(1),
            Some("name = \"outside\"")
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_migrate_validates_split_cardinality_and_workspace_membership_before_writing() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_named("config-migrate-context-validation")?;
        workspace.add_crate("member-a", "0.1.0", &[])?;
        workspace.add_crate("member-b", "0.1.0", &[])?;
        let config_path = workspace.path.join(".config/rail.toml");
        let invalid_single = b"[crates.bundle.split]\nremote = \"../bundle\"\nbranch = \"main\"\nmode = \"single\"\npaths = [{ crate = \"crates/member-a\" }, { crate = \"crates/member-b\" }]\n";
        fs::write(&config_path, invalid_single)?;
        let output = run_cargo_rail(&workspace.path, &["rail", "config", "migrate"])?;
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("must have exactly one member"),
            "unexpected diagnostic: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(&config_path)?, invalid_single);

        let detached = workspace.path.join("detached");
        fs::create_dir(&detached)?;
        fs::write(
            detached.join("Cargo.toml"),
            "[package]\nname = \"detached-member\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )?;
        fs::create_dir(detached.join("src"))?;
        fs::write(detached.join("src/lib.rs"), "")?;
        let nonmember = b"[crates.detached.split]\nremote = \"../detached-split\"\nbranch = \"main\"\nmode = \"single\"\npaths = [{ crate = \"detached\" }]\n";
        fs::write(&config_path, nonmember)?;
        let output = run_cargo_rail(&workspace.path, &["rail", "config", "migrate"])?;
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("detached-member"),
            "unexpected diagnostic: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(&config_path)?, nonmember);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_migrate_preserves_decorated_structural_parent_tables() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_named("config-migrate-decorated-parent")?;
        workspace.add_crate("test-crate", "0.1.0", &[])?;
        let config_path = workspace.path.join(".config/rail.toml");
        fs::write(
            &config_path,
            "# release policy remains documented\n[release] # preserve table decoration\n# retired leaf explanation remains historical context\nrequire_clean = true\n",
        )?;

        let output = run_cargo_rail(&workspace.path, &["rail", "config", "migrate"])?;
        assert!(output.status.success(), "{output:?}");
        let migrated = fs::read_to_string(config_path)?;
        assert!(migrated.contains("# release policy remains documented"));
        assert!(migrated.contains("[release] # preserve table decoration"));
        assert!(!migrated.contains("require_clean"));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_config_migrate_preserves_unix_security_metadata() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_named("config-migrate-security-metadata")?;
        workspace.add_crate("test-crate", "0.1.0", &[])?;
        let config_path = workspace.path.join(".config/rail.toml");
        fs::write(&config_path, "[release]\nrequire_clean = true\n")?;
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o640))?;
        #[cfg(target_os = "macos")]
        let xattr_name = "com.cargo-rail.migration-test";
        #[cfg(not(target_os = "macos"))]
        let xattr_name = "user.cargo_rail_migration_test";
        rustix::fs::setxattr(
            &config_path,
            xattr_name,
            b"preserve-xattr",
            rustix::fs::XattrFlags::empty(),
        )?;
        #[cfg(target_os = "macos")]
        let expected_acl = {
            let uid = rustix::process::getuid().as_raw();
            let acl_principal = if uid == 0 { "1".to_string() } else { "0".to_string() };
            let mut acl = exacl::getfacl(&config_path, None)?;
            acl.push(exacl::AclEntry::allow_user(&acl_principal, exacl::Perm::READ, None));
            exacl::setfacl(&[&config_path], &acl, None)?;
            exacl::getfacl(&config_path, None)?
        };
        let expected_metadata = fs::metadata(&config_path)?;

        let output = run_cargo_rail(&workspace.path, &["rail", "config", "migrate"])?;
        assert!(output.status.success(), "{output:?}");
        let actual_metadata = fs::metadata(&config_path)?;
        assert_eq!(actual_metadata.uid(), expected_metadata.uid());
        assert_eq!(actual_metadata.gid(), expected_metadata.gid());
        assert_eq!(actual_metadata.mode() & 0o7777, expected_metadata.mode() & 0o7777);
        #[cfg(target_os = "macos")]
        assert_eq!(exacl::getfacl(&config_path, None)?, expected_acl);
        let mut xattr = [0_u8; 64];
        let xattr_len = rustix::fs::getxattr(&config_path, xattr_name, &mut xattr)?;
        assert_eq!(&xattr[..xattr_len], b"preserve-xattr");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(windows)]
#[test]
fn test_config_migrate_preserves_windows_named_stream() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_named("config-migrate-windows-stream")?;
        workspace.add_crate("test-crate", "0.1.0", &[])?;
        let config_path = workspace.path.join(".config/rail.toml");
        fs::write(&config_path, "[release]\nrequire_clean = true\n")?;
        let stream_path = std::path::PathBuf::from(format!("{}:cargo-rail-test", config_path.display()));
        fs::write(&stream_path, b"preserve-stream")?;
        let output = run_cargo_rail(&workspace.path, &["rail", "config", "migrate"])?;
        assert!(output.status.success(), "{output:?}");
        assert_eq!(fs::read(stream_path)?, b"preserve-stream");
        Ok(())
    })();
    super::helpers::finish_test(result);
}
