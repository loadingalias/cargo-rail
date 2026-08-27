//! Integration tests for `cargo rail config` commands (locate, print, validate, explain, migrate)

use crate::helpers::{TestWorkspace, cargo_rail_command, run_cargo_rail};
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
source = "both"
tag_prefix = "release-"
tag_format = "{crate}-{version}"
remote_effects = "push"
sign_tags = true
require_changelog_entries = true
require_release_notes = false
release_notes_dir = "notes"
change_dir = ".release-intent"
pre_1_breaking_bump = "major"
unconventional_commits = "deny"
semver_check = "deny"
require_change_files = ["test-crate"]
version_groups = { core = ["test-crate"] }

"#,
            ),
            (
                "deprecated",
                r#"[unify]
compiler_diag_cache = false
sort_dependencies = false
pin_transitives = true
transitive_host = "root"
msrv = true
msrv_source = "workspace"
enforce_msrv_inheritance = true

[release]
require_clean = false
publish_delay = 17
require_release_notes = false
"#,
            ),
            (
                "optional",
                r#"[unify]
transitive_pinning = { host = "root" }

[release.changelog]
commit_url = "https://example.invalid/commit/{sha}"
pr_url = "https://example.invalid/pull/{pr}"

[crates.test-crate.release]
publish = false

[crates.test-crate.changelog]
path = "HISTORY.md"
emoji = false
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
            for deprecated in [
                "compiler_diag_cache",
                "sort_dependencies",
                "require_clean",
                "publish_delay",
            ] {
                assert!(
                    !printed_text.contains(deprecated),
                    "{name}: emitted {deprecated}: {printed_text}"
                );
            }

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

            let migrated = run_cargo_rail(
                &ws.path,
                &["rail", "--config", &canonical_name, "config", "migrate", "--check"],
            )?;
            assert!(
                migrated.status.success(),
                "{name}: printed config has a pending migration: {migrated:?}"
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
fn test_config_validate_rejects_removed_change_detection_policy() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-rejects-change-detection")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        fs::write(
            ws.path.join(".config/rail.toml"),
            "[change-detection]\nconfidence_profile = \"strict\"\n",
        )?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--no-strict"])?;
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("change-detection"), "{stderr}");
        assert!(stderr.contains("cargo rail config migrate"), "{stderr}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_validate_warns_with_migration_guidance_for_deprecated_fields() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-validate-deprecated-toggles")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;

        fs::write(
            ws.path.join(".config/rail.toml"),
            r#"[unify]
compiler_diag_cache = false
sort_dependencies = false

[release]
require_clean = true
publish_delay = 5
"#,
        )?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--no-strict", "-f", "json"])?;
        assert!(
            output.status.success(),
            "deprecated compatibility input should warn locally"
        );

        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let warnings = json["warnings"].as_array().expect("warnings array");
        assert!(warnings.iter().any(|warning| {
            warning["message"].as_str().is_some_and(|message| {
                message.contains("unify.compiler_diag_cache") && message.contains("config migrate")
            })
        }));
        assert!(warnings.iter().any(|warning| {
            warning["message"].as_str().is_some_and(|message| {
                message.contains("unify.sort_dependencies") && message.contains("config migrate")
            })
        }));
        for path in ["release.require_clean", "release.publish_delay"] {
            assert!(
                warnings.iter().any(|warning| {
                    warning["message"]
                        .as_str()
                        .is_some_and(|message| message.contains(path) && message.contains("cargo rail config migrate"))
                }),
                "{path} warning must name the migration command"
            );
        }

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
fn test_config_validate_rejects_removed_repository_cache_authority() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-validate-removed-cache")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        fs::write(ws.path.join(".config/rail.toml"), "[cache]\nl2 = \"team\"\n")?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--no-strict"])?;
        assert_eq!(output.status.code(), Some(2));
        let stdout = String::from_utf8_lossy(&output.stderr);
        assert!(
            stdout.contains("[cache] repository configuration is no longer supported")
                && stdout.contains("CARGO_RAIL_CACHE_REMOTE"),
            "removed repository cache authority must name the machine-owned recovery rule:\n{stdout}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_validate_rejects_removed_run_configuration() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-validate-removed-run")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        fs::write(ws.path.join(".config/rail.toml"), "[run]\n")?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "-f", "json"])?;
        assert_eq!(output.status.code(), Some(2));
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let errors = json["errors"].as_array().expect("validation errors");
        let message = errors
            .iter()
            .find_map(|error| error["message"].as_str())
            .expect("removed run diagnostic");
        for expected in ["[run]", "Cargo", "cargo-nextest", "Just", "CI", "cargo rail plan"] {
            assert!(message.contains(expected), "diagnostic must name {expected}: {message}");
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

// Config Explain Tests

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
fn test_config_explain_marks_removed_input_as_ineffective() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-explain-removed")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        fs::write(
            ws.path.join(".config/rail.toml"),
            "[unify]\ncompiler_diag_cache = false\n",
        )?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "explain", "-f", "json"])?;
        assert!(output.status.success());
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let field = json["fields"]
            .as_array()
            .and_then(|fields| fields.iter().find(|field| field["path"] == "unify.compiler_diag_cache"))
            .expect("removed field explanation");
        assert_eq!(field["configured"], false);
        assert_eq!(field["effective"], serde_json::Value::Null);
        assert_eq!(field["default"], serde_json::Value::Null);
        assert_eq!(field["classification"], "implementation_detail");
        assert!(
            field["deprecation"]
                .as_str()
                .is_some_and(|message| message.contains("config migrate"))
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_explain_attributes_legacy_policy_to_config() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-explain-legacy-policy")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        fs::write(ws.path.join(".config/rail.toml"), "[release]\npush = true\n")?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "explain", "-f", "json"])?;
        assert!(output.status.success());
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let field = json["fields"]
            .as_array()
            .and_then(|fields| fields.iter().find(|field| field["path"] == "release.remote_effects"))
            .expect("effective release policy explanation");
        assert_eq!(field["effective"], "push");
        assert_eq!(field["source"], json["config_path"]);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// Config Migrate Tests

#[test]
fn test_config_migrate_check_is_read_only_and_exits_one() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-migrate-check")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;

        let config_path = ws.path.join(".config").join("rail.toml");
        let original_config = r#"[unify]
compiler_diag_cache = false
sort_dependencies = false

[release]
require_clean = true
publish_delay = 5
"#;
        fs::write(&config_path, original_config)?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "--check", "-f", "json"])?;
        assert_eq!(output.status.code(), Some(1), "pending migration must exit 1");

        let config_after = fs::read_to_string(&config_path)?;
        assert_eq!(original_config, config_after, "--check must not modify rail.toml");

        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(json["command"], "config");
        assert_eq!(json["action"], "migrate");
        assert_eq!(json["has_changes"], true);
        let changes = json["changes"].as_array().expect("changes array");
        assert_eq!(changes.len(), 4);
        assert!(changes.iter().all(|change| change["kind"] == "remove"));
        let paths = changes
            .iter()
            .filter_map(|change| change["path"].as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            paths,
            std::collections::BTreeSet::from([
                "release.publish_delay",
                "release.require_clean",
                "unify.compiler_diag_cache",
                "unify.sort_dependencies",
            ])
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_migrate_replaces_split_paths_with_cargo_member_names() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-migrate-split-members")?;
        ws.add_crate("member-a", "0.1.0", &[])?;
        ws.add_crate("member-b", "0.1.0", &[])?;
        ws.commit("Add split members")?;
        let config_path = ws.path.join(".config/rail.toml");
        fs::write(
            &config_path,
            r#"[crates.bundle.split]
remote = "../bundle"
branch = "main"
mode = "combined"
paths = [{ crate = "crates/member-b" }, { crate = "crates/member-a" }]
"#,
        )?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
        assert!(output.status.success());
        let migrated = fs::read_to_string(config_path)?;
        assert!(!migrated.contains("paths ="));
        assert!(
            migrated.contains("members = [\"member-a\", \"member-b\"]"),
            "{migrated}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_migrate_applies_explicit_renames_and_removals() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-migrate-apply")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;

        let config_path = ws.path.join(".config").join("rail.toml");
        fs::write(
            &config_path,
            r#"[cache]
l2 = "team"

[workspace]
root = "."

[toolchain]
channel = "stable"

[unify]
compiler_diag_cache = false
sort_dependencies = false
prune_dead_features = false
detect_unused = false
remove_unused = false
detect_undeclared_features = false
fix_undeclared_features = false
pin_transitives = true
transitive_host = "root"
msrv = true
msrv_source = "workspace"
enforce_msrv_inheritance = true

[release]
require_clean = true
publish_delay = 5
push = true
create_github_release = true
forge = "gitlab"

[change-detection]
conservative_unclassified_owner_fallback = true
bot_pr_confidence_profile = "strict"

[crates.demo.sync]
"#,
        )?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
        assert!(output.status.success(), "config migrate should apply known migrations");

        let migrated = fs::read_to_string(&config_path)?;
        assert!(!migrated.contains("[cache]"));
        assert!(!migrated.contains("l2 ="));
        assert!(!migrated.contains("compiler_diag_cache"));
        assert!(!migrated.contains("sort_dependencies"));
        assert!(!migrated.contains("prune_dead_features"));
        assert!(!migrated.contains("detect_unused"));
        assert!(!migrated.contains("remove_unused"));
        assert!(!migrated.contains("detect_undeclared_features"));
        assert!(!migrated.contains("fix_undeclared_features"));
        assert!(!migrated.contains("pin_transitives"));
        assert!(!migrated.contains("transitive_host"));
        assert!(migrated.contains("transitive_pinning = { host = \"root\" }"));
        assert!(!migrated.contains("msrv_source"));
        assert!(!migrated.contains("enforce_msrv_inheritance"));
        assert!(migrated.contains("msrv_policy = { mode = \"compute\", source = \"workspace\", inherit = true }"));
        assert!(!migrated.contains("create_github_release"));
        assert!(!migrated.contains("require_clean"));
        assert!(!migrated.contains("publish_delay"));
        assert!(!migrated.contains("push ="));
        assert!(!migrated.contains("forge ="));
        assert!(migrated.contains("remote_effects = \"gitlab\""));
        assert!(!migrated.contains("bot_pr_confidence_profile"));
        assert!(!migrated.contains("[workspace]"));
        assert!(!migrated.contains("[toolchain]"));
        assert!(!migrated.contains("[crates.demo.sync]"));
        assert!(!migrated.contains("conservative_unclassified_owner_fallback"));
        assert!(!migrated.contains("[change-detection]"));
        let second = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "--check"])?;
        assert!(second.status.success(), "applied migrations must be idempotent");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_migrate_removes_legacy_planning_policy() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-migrate-policy-values")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        let config_path = ws.path.join(".config/rail.toml");

        fs::write(&config_path, "[change-detection]\nunknown_file_policy = true\n")?;
        let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
        assert!(output.status.success());
        assert!(!fs::read_to_string(&config_path)?.contains("change-detection"));

        fs::write(
            &config_path,
            "[change-detection]\nunknown_file_policy = \"workspace_infra\"\nconservative_unclassified_owner_fallback = false\n",
        )?;
        let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
        assert!(output.status.success());
        let migrated = fs::read_to_string(&config_path)?;
        assert!(!migrated.contains("unknown_file_policy"));
        assert!(!migrated.contains("conservative_unclassified_owner_fallback"));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_migrate_reports_default_equivalents_as_omitted() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-migrate-default-equivalents")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        let config_path = ws.path.join(".config/rail.toml");
        let original = r#"[unify]
pin_transitives = false
msrv = true
msrv_source = "max"
enforce_msrv_inheritance = false

[release]
push = false
create_github_release = false
"#;
        fs::write(&config_path, original)?;

        let check = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "--check", "-f", "json"])?;
        assert_eq!(check.status.code(), Some(1));
        assert_eq!(fs::read_to_string(&config_path)?, original);
        let json: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        let replacements: Vec<_> = json["changes"]
            .as_array()
            .expect("migration changes")
            .iter()
            .filter_map(|change| change["replacement"].as_str())
            .collect();
        assert!(!replacements.is_empty());
        assert!(
            replacements
                .iter()
                .all(|replacement| replacement.starts_with("field omitted ("))
        );

        let apply = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
        assert!(apply.status.success());
        assert_eq!(fs::read_to_string(&config_path)?.trim(), "");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_migrate_refuses_invalid_legacy_release_effects() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-migrate-invalid-release-effects")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        let config_path = ws.path.join(".config/rail.toml");
        let original = "[release]\ncreate_github_release = true\npush = false\n";
        fs::write(&config_path, original)?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(fs::read_to_string(&config_path)?, original);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("choose one explicit policy"));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_config_migrate_rejects_removed_run_configuration_without_mutation() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-migrate-removed-run")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;
        let config_path = ws.path.join(".config/rail.toml");
        let original = "[run]\n";
        fs::write(&config_path, original)?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(fs::read_to_string(&config_path)?, original);
        let stderr = String::from_utf8_lossy(&output.stderr);
        for expected in ["[run]", "Cargo", "cargo-nextest", "Just", "CI", "cargo rail plan"] {
            assert!(stderr.contains(expected), "diagnostic must name {expected}: {stderr}");
        }

        Ok(())
    })();
    super::helpers::finish_test(result);
}
