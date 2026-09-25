//! Integration tests for `cargo rail config` commands (locate, print, validate, explain)

use crate::helpers::{TestWorkspace, cargo_rail_command, run_cargo_rail, run_cargo_rail_with_env};
use anyhow::{Context as _, Result};
use std::fs;
use std::io::Write as _;
use std::process::Stdio;

fn stdin_validation(workspace: &std::path::Path, input: &[u8]) -> Result<std::process::Output> {
    let mut child = cargo_rail_command(workspace)?
        .args(["rail", "--config", "-", "config", "validate", "--strict", "-f", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().expect("piped stdin").write_all(input)?;
    Ok(child.wait_with_output()?)
}

#[test]
fn current_configuration_loads_without_writes() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("current-configuration")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("fixture")?;
        let path = ws.path.join(".config/rail.toml");
        let original = b"# retain comments and spelling\n[unify]\nmsrv_policy = { mode = 'disabled' }\n[release]\nsource = 'commits'\nremote_effects = 'none'\n";
        fs::write(&path, original)?;
        let writable = fs::metadata(&path)?.permissions();
        let mut readonly = writable.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&path, readonly)?;
        let commands: &[&[&str]] = &[
            &["rail", "plan", "--since", "HEAD", "--json"],
            &["rail", "--quiet", "plan", "--since", "HEAD", "--json"],
            &["rail", "config", "validate", "--strict", "-f", "json"],
            &["rail", "config", "print", "-f", "json"],
            &["rail", "config", "explain", "-f", "json"],
            &["rail", "config", "--json"],
        ];
        for environment in [&[][..], &[("CI", "true")][..]] {
            for args in commands {
                let output = run_cargo_rail_with_env(&ws.path, args, environment)?;
                assert!(output.status.success(), "{args:?}: {output:?}");
                assert!(
                    output.stderr.is_empty(),
                    "{args:?}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
                if value["action"] == "print" {
                    assert_eq!(value["config"]["unify"]["msrv_policy"]["mode"], "disabled");
                    assert_eq!(value["config"]["release"]["source"], "commits");
                    assert_eq!(value["config"]["release"]["remote_effects"], "none");
                }
                if value["action"] == "explain" {
                    let schema: serde_json::Value =
                        serde_json::from_str(include_str!("../../schemas/config-explain-v2.schema.json"))?;
                    jsonschema::validator_for(&schema)?
                        .validate(&value)
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                    assert!(value.get("compatibility").is_none());
                }
                assert_eq!(fs::read(&path)?, original);
            }
        }
        assert!(fs::metadata(&path)?.permissions().readonly());
        fs::set_permissions(&path, writable)?;
        assert_eq!(
            fs::read_dir(path.parent().unwrap())?.count(),
            2,
            "inspection created a config artifact"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn unsupported_configuration_leaves_input_unchanged_across_consumers() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("demo", "0.1.0")?;
        fs::create_dir_all(ws.path.join(".config"))?;
        let path = ws.path.join(".config/rail.toml");
        for input in [
            "[unify]\nmsrv = 'false'\n",
            "[unify]\nmsrv = false\nmsrv_policy = { mode = 'disabled' }\n",
            "[release]\npush = false\nsource = 'typo'\n",
            "[release]\nrequire_clean = false\nunknown = true\n",
            "[unify]\nmsrv = false\npreserve_features = ['[']\n",
        ] {
            fs::write(&path, input)?;
            for args in [
                &["rail", "config", "explain"][..],
                &["rail", "config", "print"][..],
                &["rail", "config", "validate", "--no-strict"][..],
                &["rail", "plan", "--since", "HEAD"][..],
            ] {
                let output = run_cargo_rail(&ws.path, args)?;
                assert_eq!(output.status.code(), Some(2), "{input}: {args:?}: {output:?}");
                assert_eq!(fs::read_to_string(&path)?, input);
            }
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn unsupported_split_paths_fail_without_cargo_discovery_or_writes() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("demo", "0.1.0")?;
        fs::create_dir_all(ws.path.join(".config"))?;
        let path = ws.path.join(".config/rail.toml");
        let input = "[crates.demo.split]\nremote = '../demo'\npaths = [{ crate = '.' }]\n";
        fs::write(&path, input)?;
        fs::write(ws.path.join("Cargo.toml"), "[broken manifest")?;
        for error in [
            cargo_rail::config::RailConfig::load(&ws.path).unwrap_err(),
            cargo_rail::commands::clean::CleanContext::capture(&ws.path, None).unwrap_err(),
        ] {
            assert!(error.to_string().contains("crates.demo.split.paths"), "{error}");
        }
        assert_eq!(fs::read_to_string(path)?, input);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn inspection_never_treats_failed_workspace_discovery_as_valid_policy() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("demo", "0.1.0")?;
        fs::create_dir_all(ws.path.join(".config"))?;
        fs::write(ws.path.join(".config/rail.toml"), "")?;
        fs::write(ws.path.join("Cargo.toml"), "[broken manifest")?;
        for action in ["print", "explain", "validate"] {
            let output = run_cargo_rail(&ws.path, &["rail", "config", action])?;
            assert_eq!(output.status.code(), Some(2), "{action}: {output:?}");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("Cargo cannot load manifest `Cargo.toml:1:"),
                "{output:?}"
            );
        }
        let outside = tempfile::tempdir()?;
        let invalid = stdin_validation(outside.path(), b"[release]\ntag_format = ''\n")?;
        assert_eq!(invalid.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&invalid.stdout).contains("tag_format cannot be empty"));
        for input in [
            b"[release]\nversion_groups = { group = ['demo'] }\n".as_slice(),
            b"[unify]\ntransitive_pinning = { host = 'crates/host' }\n".as_slice(),
        ] {
            let missing = stdin_validation(outside.path(), input)?;
            assert_eq!(missing.status.code(), Some(2));
            assert!(String::from_utf8_lossy(&missing.stdout).contains("requires Cargo workspace context"));
        }
        for input in [
            b"[unify]\nmsrv_policy = { mode = 'disabled' }\n".as_slice(),
            b"[crates.demo]\n".as_slice(),
        ] {
            let independent = stdin_validation(&ws.path, input)?;
            assert!(
                independent.status.success(),
                "stdin used an unrelated broken manifest: {independent:?}"
            );
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn unreadable_discovered_configuration_never_falls_back_to_defaults() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("demo", "0.1.0")?;
        let path = ws.path.join("rail.toml");
        fs::create_dir(&path)?;
        for args in [
            &["rail", "config"][..],
            &["rail", "config", "print"][..],
            &["rail", "config", "validate", "-f", "json"][..],
            &["rail", "plan", "--since", "HEAD"][..],
        ] {
            let output = run_cargo_rail(&ws.path, args)?;
            assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
            let message = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(message.contains("rail.toml"), "{message}");
            assert!(path.is_dir());
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn canonical_export_preserves_target_inheritance_after_policy_changes() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("demo", "0.1.0")?;
        fs::create_dir_all(ws.path.join(".config"))?;
        let path = ws.path.join(".config/rail.toml");
        fs::write(&path, "[surface]\ntargets = 'workspace'\n")?;
        let printed = run_cargo_rail(&ws.path, &["rail", "config", "print"])?;
        assert!(printed.status.success(), "{printed:?}");
        let mut document: toml_edit::DocumentMut = String::from_utf8(printed.stdout)?.parse()?;
        let mut targets = toml_edit::Array::new();
        targets.push("wasm32-wasip1");
        document["targets"] = toml_edit::value(targets);
        fs::write(&path, document.to_string())?;
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "config", "explain", "surface.targets", "-f", "json"],
        )?;
        assert!(output.status.success(), "{output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(value["fields"][0]["configured"], "workspace");
        assert_eq!(
            value["fields"][0]["effective"],
            serde_json::json!(["host", "wasm32-wasip1"])
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn config_explain_expands_parent_nodes_and_suggests_valid_children() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("demo", "0.1.0")?;
        let node = run_cargo_rail(&ws.path, &["rail", "config", "explain", "surface", "-f", "json"])?;
        assert!(node.status.success(), "parent explanation failed: {node:?}");
        let node: serde_json::Value = serde_json::from_slice(&node.stdout)?;
        let fields = node["fields"].as_array().expect("explained fields");
        assert!(fields.len() > 5);
        assert!(
            fields
                .iter()
                .all(|field| { field["path"].as_str().is_some_and(|path| path.starts_with("surface.")) })
        );

        let unknown = run_cargo_rail(&ws.path, &["rail", "config", "explain", "surface.nope"])?;
        assert_eq!(unknown.status.code(), Some(2));
        let diagnostic = String::from_utf8_lossy(&unknown.stderr);
        assert!(
            diagnostic.contains("valid child paths include: surface."),
            "{diagnostic}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

// Config Locate Tests

#[test]
fn test_config_locate_finds_config() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-locate-finds")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add test crate")?;

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

        let output = run_cargo_rail(&ws.path, &["rail", "config", "print"])?;

        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("coded defaults"));

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

        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("configuration is valid"));

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

        assert!(output.status.success(), "{output:?}");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(json["valid"], true);
        assert!(json["config_path"].is_null());
        assert_eq!(json["errors"], serde_json::json!([]));

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
fn member_invocation_keeps_workspace_configuration_authoritative() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-member-invocation")?;
        let member = ws.add_crate("test-crate", "0.1.0", &[])?;
        fs::create_dir_all(member.join(".config"))?;
        fs::write(member.join(".config/rail.toml"), "targtes = []\n")?;
        ws.commit("Add member-local non-workspace configuration")?;

        let output = run_cargo_rail(&member, &["rail", "plan", "--since", "HEAD", "--json"])?;
        assert!(
            output.status.success(),
            "member-local configuration replaced workspace policy: {output:?}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Run cargo-rail where every Git, Cargo, and rustc spawn fails.
fn run_cargo_rail_without_tools(cwd: &std::path::Path, args: &[&str]) -> Result<std::process::Output> {
    let tools = tempfile::tempdir()?;
    let absent = tools.path().join("absent");
    Ok(cargo_rail_command(cwd)?
        .args(args)
        .env("PATH", tools.path())
        .env("CARGO", &absent)
        .env("RUSTC", &absent)
        .output()?)
}

#[test]
fn discovered_policy_is_rejected_before_git_or_cargo() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-early-discovered")?;
        let member = ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add early-validation fixture")?;
        let package = TestWorkspace::new_single_crate("demo", "0.1.0")?;
        let plan = &["rail", "plan", "--since", "HEAD", "--json"][..];
        // macOS scans a newly linked executable before its first launch; keep that out of the budget.
        run_cargo_rail_without_tools(&ws.path, &["--version"])?;
        let invocations = [
            (&ws.path, &ws.path, plan),
            (&ws.path, &ws.path, &["rail", "unify", "--check"][..]),
            (&ws.path, &member, plan),
            (&package.path, &package.path, plan),
        ];
        for (input, needle) in [
            ("targtes = []\n", "unknown configuration key 'targtes'"),
            ("[release]\ntag_format = \"\"\n", "tag_format cannot be empty"),
        ] {
            for (root, cwd, args) in invocations {
                fs::write(root.join(".config/rail.toml"), input)?;
                let started = std::time::Instant::now();
                let output = run_cargo_rail_without_tools(cwd, args)?;
                // Budget: B1 measured 11-16 ms with no Git or Cargo reachable; the bound
                // leaves room for a loaded host while catching any subprocess or capture.
                assert!(
                    started.elapsed() < std::time::Duration::from_secs(1),
                    "{args:?} took {:?} to reject invalid policy",
                    started.elapsed()
                );
                let combined = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(
                    output.status.code(),
                    Some(2),
                    "{args:?} in {}: {combined}",
                    cwd.display()
                );
                assert!(combined.contains(needle), "{args:?} in {}: {combined}", cwd.display());
                for generated in [root.join("target"), cwd.join("target")] {
                    assert!(!generated.exists(), "{args:?} created {}", generated.display());
                }
            }
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn excluded_package_policy_is_selected_by_cargo_not_the_enclosing_workspace() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-excluded-package")?;
        let manifest = ws.path.join("Cargo.toml");
        let workspace = fs::read_to_string(&manifest)?.replace(
            "members = [\"crates/*\"]\n",
            "members = [\"crates/*\"]\nexclude = [\"standalone\"]\n",
        );
        fs::write(&manifest, workspace)?;
        let standalone = ws.path.join("standalone");
        fs::create_dir_all(standalone.join("src"))?;
        fs::create_dir_all(standalone.join(".config"))?;
        fs::write(
            standalone.join("Cargo.toml"),
            "[package]\nname = \"standalone\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )?;
        fs::write(standalone.join("src/lib.rs"), "")?;
        fs::write(standalone.join(".config/rail.toml"), "")?;
        fs::write(ws.path.join(".config/rail.toml"), "targtes = []\n")?;
        ws.commit("Add excluded package with its own policy")?;

        let output = run_cargo_rail(&standalone, &["rail", "plan", "--since", "HEAD", "--json"])?;
        assert!(
            output.status.success(),
            "enclosing workspace policy replaced the excluded package policy: {output:?}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn inspection_from_a_member_uses_the_workspace_policy_like_consuming_commands() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-member-inspection")?;
        let member = ws.add_crate("test-crate", "0.1.0", &[])?;
        fs::write(
            ws.path.join(".config/rail.toml"),
            "[unify]\nmsrv_policy = { mode = 'disabled' }\n",
        )?;
        fs::create_dir_all(member.join(".config"))?;
        fs::write(member.join(".config/rail.toml"), "targtes = []\n")?;
        ws.commit("Add member-local non-workspace configuration")?;
        let workspace_policy = fs::canonicalize(ws.path.join(".config/rail.toml"))?
            .display()
            .to_string();

        for args in [
            &["rail", "config", "validate", "--strict", "-f", "json"][..],
            &["rail", "config", "print", "-f", "json"][..],
            &["rail", "config", "explain", "-f", "json"][..],
            &["rail", "plan", "--since", "HEAD", "--json"][..],
        ] {
            let output = run_cargo_rail(&member, args)?;
            assert!(output.status.success(), "{args:?} used member-local policy: {output:?}");
        }
        let located = run_cargo_rail(&member, &["rail", "config", "locate", "-f", "json"])?;
        let located: serde_json::Value = serde_json::from_slice(&located.stdout)?;
        assert_eq!(located["path"], workspace_policy.as_str(), "{located}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn independent_configuration_errors_are_reported_together_everywhere() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-independent-errors")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add independent-errors fixture")?;
        fs::write(
            ws.path.join(".config/rail.toml"),
            "[release]\ntag_format = \"\"\n\n[plan.work.Invalid]\nscope = \"repository\"\npaths = [\"docs/**\"]\n",
        )?;
        let needles = ["tag_format cannot be empty", "must match [a-z][a-z0-9.-]*"];

        let validated = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--strict", "-f", "json"])?;
        assert_eq!(validated.status.code(), Some(2), "{validated:?}");
        let validated: serde_json::Value = serde_json::from_slice(&validated.stdout)?;
        let messages = validated["errors"]
            .as_array()
            .expect("validation errors")
            .iter()
            .filter_map(|issue| issue["message"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            messages.len(),
            2,
            "each independent error is its own issue: {validated}"
        );
        for needle in needles {
            assert!(
                messages.iter().any(|message| message.contains(needle)),
                "{needle}: {validated}"
            );
        }

        for args in [
            &["rail", "config", "print"][..],
            &["rail", "plan", "--since", "HEAD"][..],
        ] {
            let output = run_cargo_rail(&ws.path, args)?;
            assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(combined.contains("2 configuration errors"), "{args:?}: {combined}");
            for needle in needles {
                assert!(combined.contains(needle), "{args:?} omitted {needle}: {combined}");
            }
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn removed_release_changelog_entry_gate_is_rejected() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-removed-release-gate")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("Add removed release gate fixture")?;
        fs::write(
            ws.path.join(".config/rail.toml"),
            "[release]\nrequire_changelog_entries = true\n",
        )?;

        let output = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--no-strict"])?;
        assert_eq!(output.status.code(), Some(2), "removed field was accepted: {output:?}");
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            combined.contains(
                "configuration key 'release.require_changelog_entries' was removed in Cargo-Rail 0.29.0; \
                 `release.require_release_notes` remains the release-prose gate; \
                 `cargo rail config migrate` previews its removal"
            ),
            "{combined}"
        );
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
                &["rail", "config", "explain"][..],
                &["rail", "config", "print"][..],
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
fn test_config_workspace_surface_targets_preserve_policy_and_explain_resolved_values() {
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
        assert_eq!(printed["config"]["surface"]["targets"], serde_json::json!("workspace"));

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
fn broken_member_manifest_is_named_and_never_validated() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("broken-member-manifest")?;
        ws.add_crate("member", "0.1.0", &[("absent", "{ workspace = true }")])?;
        let root_manifest = fs::read(ws.path.join("Cargo.toml"))?;
        let member_manifest = fs::read(ws.path.join("crates/member/Cargo.toml"))?;

        let json = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--strict", "-f", "json"])?;
        assert_eq!(json.status.code(), Some(2), "{json:?}");
        let value: serde_json::Value = serde_json::from_slice(&json.stdout)?;
        assert_eq!(value["valid"], false);
        assert_eq!(value["evidence"], "workspace");
        let issue = &value["errors"][0];
        assert_eq!(issue["section"], "cargo", "{value:#}");
        let message = issue["message"].as_str().unwrap_or_default();
        assert!(
            message.starts_with("Cargo cannot load manifest `crates/member/Cargo.toml`"),
            "{message}"
        );
        assert!(message.contains("absent"), "Cargo's cause must be retained: {message}");
        assert!(
            issue["help"].as_str().is_some_and(|help| help.contains("reproduce: ")),
            "{issue:#}"
        );

        let text = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--strict"])?;
        assert_eq!(text.status.code(), Some(2), "{text:?}");
        let stderr = String::from_utf8_lossy(&text.stderr);
        assert!(
            stderr.contains("[cargo] Cargo cannot load manifest `crates/member/Cargo.toml`"),
            "{stderr}"
        );
        assert!(stderr.contains("    help: "), "{stderr}");
        assert!(!String::from_utf8_lossy(&text.stdout).contains("configuration is valid"));

        assert_eq!(fs::read(ws.path.join("Cargo.toml"))?, root_manifest);
        assert_eq!(fs::read(ws.path.join("crates/member/Cargo.toml"))?, member_manifest);
        assert!(
            !ws.path.join("Cargo.lock").exists(),
            "validation must not create a lockfile"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn validation_states_whether_it_checked_the_cargo_workspace() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("demo", "0.1.0")?;
        let workspace = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--strict", "-f", "json"])?;
        assert!(workspace.status.success(), "{workspace:?}");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&workspace.stdout)?["evidence"],
            "workspace"
        );
        let text = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--strict"])?;
        assert!(
            String::from_utf8_lossy(&text.stdout).contains("configuration is valid for the Cargo workspace"),
            "{text:?}"
        );

        let outside = tempfile::tempdir()?;
        let schema = stdin_validation(outside.path(), b"")?;
        assert!(schema.status.success(), "{schema:?}");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&schema.stdout)?["evidence"],
            "schema"
        );
        let no_manifest = run_cargo_rail(outside.path(), &["rail", "config", "validate", "--strict"])?;
        assert!(
            String::from_utf8_lossy(&no_manifest.stdout)
                .contains("configuration is valid (schema only; no Cargo workspace was checked)"),
            "{no_manifest:?}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

fn effective_config(ws: &TestWorkspace) -> Result<serde_json::Value> {
    let output = run_cargo_rail(&ws.path, &["rail", "config", "print", "-f", "json"])?;
    anyhow::ensure!(output.status.success(), "config print failed: {output:?}");
    Ok(serde_json::from_slice::<serde_json::Value>(&output.stdout)?["config"].clone())
}

/// `config print` writes every default. Migration reduces such a file to intentional policy,
/// keeps comments, never changes effective policy, and applies exactly what it previewed.
#[test]
fn config_migrate_reduces_printed_defaults_to_intentional_policy() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-migrate")?;
        ws.add_crate("member", "0.1.0", &[])?;
        ws.commit("fixture")?;
        let printed = run_cargo_rail(&ws.path, &["rail", "config", "print"])?;
        anyhow::ensure!(printed.status.success(), "config print failed: {printed:?}");
        let printed = String::from_utf8(printed.stdout)?;
        let intentional = printed.replacen(
            "include_renamed = false",
            "# Renamed dependencies are unified on purpose.\ninclude_renamed = true",
            1,
        );
        anyhow::ensure!(intentional != printed, "printed configuration lacks include_renamed");
        let path = ws.path.join(".config/rail.toml");
        fs::write(&path, format!("# Repository policy.\n\n{intentional}"))?;
        ws.commit("Adopt printed configuration")?;
        let before = effective_config(&ws)?;

        let check = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "--check", "-f", "json"])?;
        assert_eq!(check.status.code(), Some(1), "{check:?}");
        let preview: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        assert_eq!(preview["migration"], "rewrite", "{preview:#}");
        let expected =
            "# Repository policy.\n\n[unify]\n# Renamed dependencies are unified on purpose.\ninclude_renamed = true\n";
        assert_eq!(preview["content"], expected, "{preview:#}");
        assert!(
            preview["removed"].as_array().is_some_and(|removed| removed.len() > 20),
            "{preview:#}"
        );
        assert_eq!(
            fs::read_to_string(&path)?,
            format!("# Repository policy.\n\n{intentional}"),
            "preview must not write"
        );

        let apply = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "apply", "-f", "json"])?;
        assert!(apply.status.success(), "{apply:?}");
        assert_eq!(fs::read_to_string(&path)?, expected);
        assert_eq!(effective_config(&ws)?, before, "migration must keep effective policy");
        let validate = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--strict"])?;
        assert!(validate.status.success(), "{validate:?}");
        let clean = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "--check"])?;
        assert_eq!(clean.status.code(), Some(0), "{clean:?}");
        Ok(())
    })();
    crate::helpers::finish_test(result);
}

#[test]
fn config_migrate_deletes_a_default_only_file_and_rejects_drift() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-migrate-delete")?;
        ws.add_crate("member", "0.1.0", &[])?;
        let path = ws.path.join(".config/rail.toml");
        fs::write(&path, "[unify]\ncompiler_targets = []\n\n[surface]\nenabled = false\n")?;
        ws.commit("Default-only configuration")?;

        let preview = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "-f", "json"])?;
        assert!(preview.status.success(), "{preview:?}");
        let value: serde_json::Value = serde_json::from_slice(&preview.stdout)?;
        assert_eq!(value["migration"], "delete", "{value:#}");
        let plan_path = ws.path.join("target/config-migrate-plan.json");
        fs::create_dir_all(plan_path.parent().ok_or_else(|| anyhow::anyhow!("plan directory"))?)?;
        // The saved preview is the plan file; apply reads its `mutation_plan`.
        fs::write(&plan_path, &preview.stdout)?;

        // The approved plan binds the previewed file; an edit after preview is drift.
        fs::write(
            &path,
            "[unify]\ncompiler_targets = []\n\n[surface]\nenabled = false\n# edited\n",
        )?;
        let plan = plan_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 plan path"))?;
        let drifted = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "apply", "--plan", plan])?;
        assert!(!drifted.status.success(), "{drifted:?}");
        assert!(path.exists(), "drift must leave the file in place");

        fs::write(&path, "[unify]\ncompiler_targets = []\n\n[surface]\nenabled = false\n")?;
        let applied = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "apply", "--plan", plan])?;
        assert!(applied.status.success(), "{applied:?}");
        assert!(!path.exists(), "a default-only file is deleted");
        Ok(())
    })();
    crate::helpers::finish_test(result);
}

/// A previous-minor policy file with keys that later releases removed.
const PREVIOUS_MINOR_POLICY: &str = "# Project policy\n[run]\nprofile = \"ci\"\n\n[release]\n\
     require_changelog_entries = true\n\n[unify]\ninclude_paths = false\n";

#[test]
fn removed_configuration_keys_name_their_release_everywhere_and_migrate_away() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("config-retired-keys")?;
        ws.add_crate("member", "0.1.0", &[])?;
        let path = ws.path.join(".config/rail.toml");
        fs::write(&path, PREVIOUS_MINOR_POLICY)?;
        let base = ws.commit("Previous-minor policy")?;

        let expected = [
            "configuration key 'release.require_changelog_entries' was removed in Cargo-Rail 0.29.0; \
             `release.require_release_notes` remains the release-prose gate; \
             `cargo rail config migrate` previews its removal",
            "configuration key 'run' was removed in Cargo-Rail 0.22.0",
        ];
        for arguments in [
            &["rail", "unify", "--check"][..],
            &["rail", "config", "validate", "--strict"][..],
        ] {
            let output = run_cargo_rail(&ws.path, arguments)?;
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(output.status.code(), Some(2), "{arguments:?}: {text}");
            for message in expected {
                assert!(text.contains(message), "{arguments:?} omits `{message}`:\n{text}");
            }
            assert!(
                text.contains(".config/rail.toml"),
                "{arguments:?} names the file:\n{text}"
            );
        }

        let preview = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "-f", "json"])?;
        assert!(preview.status.success(), "{preview:?}");
        let value: serde_json::Value = serde_json::from_slice(&preview.stdout)?;
        let retired = value["removed"]
            .as_array()
            .context("removed settings")?
            .iter()
            .filter_map(|setting| Some((setting["path"].as_str()?, setting["removed_in"].as_str()?)))
            .collect::<Vec<_>>();
        assert_eq!(
            retired,
            [("release.require_changelog_entries", "0.29.0"), ("run", "0.22.0")],
            "{value:#}"
        );
        let applied = run_cargo_rail(&ws.path, &["rail", "config", "migrate", "apply"])?;
        assert!(applied.status.success(), "{applied:?}");
        assert_eq!(
            fs::read_to_string(&path)?,
            "# Project policy\n\n[unify]\ninclude_paths = false\n"
        );
        let validated = run_cargo_rail(&ws.path, &["rail", "config", "validate", "--strict"])?;
        assert!(validated.status.success(), "{validated:?}");

        // Planning the migration compares against a base whose policy still has retired keys.
        ws.commit("Migrate policy")?;
        let planned = run_cargo_rail(&ws.path, &["rail", "plan", "--since", &base, "--json"])?;
        assert!(planned.status.success(), "{}", String::from_utf8_lossy(&planned.stderr));
        let plan: serde_json::Value = serde_json::from_slice(&planned.stdout)?;
        assert_eq!(
            plan["changes"]["config"],
            serde_json::json!([]),
            "retired keys had no policy: {plan:#}"
        );

        // A key that needs a manual edit is never removed automatically.
        fs::write(
            &path,
            "[crates.member.split]\nremote = \"../member\"\nbranch = \"main\"\nmode = \"single\"\n\
             paths = [{ crate = \"crates/member\" }]\n",
        )?;
        let manual = run_cargo_rail(&ws.path, &["rail", "config", "migrate"])?;
        let stderr = String::from_utf8_lossy(&manual.stderr);
        assert_eq!(manual.status.code(), Some(2), "{stderr}");
        assert!(
            stderr.contains(
                "configuration key 'crates.member.split.paths' was removed in Cargo-Rail 0.26.0; \
                             list the split's Cargo package names in `members`; change it by hand"
            ),
            "{stderr}"
        );
        Ok(())
    })();
    crate::helpers::finish_test(result);
}
