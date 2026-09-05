//! Integration tests for `cargo rail config` commands (locate, print, validate, explain)

use crate::helpers::{TestWorkspace, cargo_rail_command, run_cargo_rail, run_cargo_rail_with_env};
use anyhow::Result;
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
fn supported_configuration_loads_directly_without_writes_or_compatibility_warnings() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("automatic-configuration")?;
        ws.add_crate("test-crate", "0.1.0", &[])?;
        ws.commit("fixture")?;
        let path = ws.path.join(".config/rail.toml");
        let original = b"# retain comments and spelling\n[unify]\nmsrv = false\npin_transitives = false\ndetect_unused = false\n[release]\nsource = 'commits'\npush = false\nrequire_clean = false\npublish_delay = 17\n";
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
                        serde_json::from_str(include_str!("../../schemas/config-explain-v1.schema.json"))?;
                    jsonschema::validator_for(&schema)?
                        .validate(&value)
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                    assert!(value["compatibility"].as_array().is_some_and(|facts| !facts.is_empty()));
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
fn compatibility_failures_leave_input_unchanged_across_consumers() {
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
fn predecessor_split_paths_resolve_captured_members_and_reject_conflicts() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("automatic-split-members")?;
        ws.add_crate("member-a", "0.1.0", &[])?;
        ws.add_crate("member-b", "0.1.0", &[])?;
        ws.commit("members")?;
        for selection in [
            "paths = [{ crate = './crates/member-a' }, { crate = 'crates/member-b' }]",
            "members = ['member-b', 'member-a']\npaths = [{ crate = 'crates/member-a' }, { crate = 'crates/member-b' }]",
            "[[crates.bundle.split.paths]]\ncrate = 'crates/member-a'\n[[crates.bundle.split.paths]]\ncrate = 'crates/member-b'",
        ] {
            let input = format!(
                "[crates.bundle.split]\nremote = '../bundle'\nbranch = 'main'\nmode = 'combined'\n{selection}\n"
            );
            fs::write(ws.path.join(".config/rail.toml"), &input)?;
            let output = run_cargo_rail(&ws.path, &["rail", "config", "print", "-f", "json"])?;
            assert!(output.status.success(), "{output:?}");
            let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            let mut members = value["config"]["crates"]["bundle"]["split"]["members"]
                .as_array()
                .unwrap()
                .iter()
                .map(|member| member.as_str().unwrap())
                .collect::<Vec<_>>();
            members.sort_unstable();
            assert_eq!(members, ["member-a", "member-b"]);
            assert_eq!(fs::read_to_string(ws.path.join(".config/rail.toml"))?, input);
        }
        for selection in [
            "members = ['member-a']\npaths = [{ crate = 'crates/member-b' }]",
            "members = 3\npaths = [{ crate = 'crates/member-a' }]",
            "paths = [{ crate = '../escape' }]",
            "paths = [{ crate = 'not-a-member' }]",
            "paths = 'crates/member-a'",
        ] {
            fs::write(
                ws.path.join(".config/rail.toml"),
                format!("[crates.bundle.split]\nremote = '../bundle'\nbranch = 'main'\nmode = 'single'\n{selection}\n"),
            )?;
            let output = run_cargo_rail(&ws.path, &["rail", "config", "explain"])?;
            assert_eq!(output.status.code(), Some(2), "{selection}: {output:?}");
        }
        let root = TestWorkspace::new_single_crate("root-package", "0.1.0")?;
        fs::create_dir_all(root.path.join(".config"))?;
        for relative in ["", ".", "./"] {
            fs::write(
                root.path.join(".config/rail.toml"),
                format!(
                    "[crates.root-package.split]\nremote = '../root-package'\nbranch = 'main'\nmode = 'single'\npaths = [{{ crate = '{relative}' }}]\n"
                ),
            )?;
            let output = run_cargo_rail(&root.path, &["rail", "config", "print", "-f", "json"])?;
            assert!(output.status.success(), "{relative}: {output:?}");
            let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            assert_eq!(
                value["config"]["crates"]["root-package"]["split"]["members"],
                serde_json::json!(["root-package"])
            );
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn predecessor_paths_load_without_cargo_discovery_for_cleanup_and_library_callers() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("demo", "0.1.0")?;
        fs::create_dir_all(ws.path.join(".config"))?;
        let path = ws.path.join(".config/rail.toml");
        let input =
            "[crates.demo.split]\nremote = '../demo'\nbranch = 'main'\nmode = 'single'\npaths = [{ crate = '.' }]\n";
        fs::write(&path, input)?;
        // Cargo discovery cannot succeed, but the captured package name is sufficient for these loaders.
        fs::write(ws.path.join("Cargo.toml"), "[package]\nname = 'demo'\n")?;
        let config = cargo_rail::config::RailConfig::load(&ws.path)?;
        assert_eq!(config.build_split_configs()[0].members, ["demo"]);
        cargo_rail::commands::clean::CleanContext::capture(&ws.path, None)?;
        assert_eq!(fs::read_to_string(path)?, input);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn predecessor_split_paths_reject_linked_manifests_without_writing() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("linked-split-manifest")?;
        ws.add_crate("demo", "0.1.0", &[])?;
        ws.commit("member")?;
        let path = ws.path.join(".config/rail.toml");
        let input = "[crates.demo.split]\nremote = '../demo'\nbranch = 'main'\nmode = 'single'\npaths = [{ crate = 'crates/demo' }]\n";
        fs::write(&path, input)?;
        let original = ws.path.join("crates/demo/Cargo.toml");
        let saved = ws.path.join("saved-manifest.toml");
        fs::rename(&original, &saved)?;
        std::os::unix::fs::symlink(&saved, &original)?;
        for args in [
            &["rail", "config", "explain"][..],
            &["rail", "config", "print"][..],
            &["rail", "config", "validate"][..],
            &["rail", "plan", "--since", "HEAD"][..],
        ] {
            let output = run_cargo_rail(&ws.path, args)?;
            assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("symbolic link"),
                "{output:?}"
            );
            assert_eq!(fs::read_to_string(&path)?, input);
        }
        assert!(
            cargo_rail::config::RailConfig::load(&ws.path)
                .unwrap_err()
                .to_string()
                .contains("symbolic links")
        );
        assert!(
            cargo_rail::commands::clean::CleanContext::capture(&ws.path, None)
                .unwrap_err()
                .to_string()
                .contains("symbolic links")
        );
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
                String::from_utf8_lossy(&output.stderr).contains("cannot validate Cargo workspace configuration"),
                "{output:?}"
            );
        }
        let outside = tempfile::tempdir()?;
        let invalid = stdin_validation(outside.path(), b"[release]\ntag_format = ''\n")?;
        assert_eq!(invalid.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&invalid.stdout).contains("tag_format cannot be empty"));
        for input in [
            b"[release]\nversion_groups = { group = ['demo'] }\n".as_slice(),
            b"[unify]\npin_transitives = true\ntransitive_host = 'crates/host'\n".as_slice(),
        ] {
            let missing = stdin_validation(outside.path(), input)?;
            assert_eq!(missing.status.code(), Some(2));
            assert!(String::from_utf8_lossy(&missing.stdout).contains("requires Cargo workspace context"));
        }
        for input in [
            b"[unify]\nmsrv = false\n".as_slice(),
            b"[crates.demo.sync]\nformer = 'reserved'\n".as_slice(),
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
