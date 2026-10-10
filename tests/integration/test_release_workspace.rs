//! Package release selection and resulting workspace validity.

use crate::helpers::{TestWorkspace, cargo_command, finish_test, git, run_cargo_rail};
use anyhow::{Result, ensure};
use serde_json::Value;

fn workspace(requirement: Option<&str>, published: bool) -> Result<TestWorkspace> {
    let ws = TestWorkspace::new_named("independent-release")?;
    ws.add_crate("core", "0.2.0", &[])?;
    let dependency = requirement.map_or_else(
        || "{ path = \"../core\" }".to_owned(),
        |requirement| format!("{{ path = \"../core\", version = \"{requirement}\" }}"),
    );
    ws.add_crate("provider", "0.1.0", &[("core", &dependency)])?;
    if !published {
        let path = ws.path.join("crates/provider/Cargo.toml");
        let manifest = std::fs::read_to_string(&path)?;
        std::fs::write(path, manifest.replace("[package]", "[package]\npublish = false"))?;
    }
    ws.write_release_config(
        "tag_format = '{prefix}{version}'\nsemver_check = 'off'\nsign_tags = false\nremote_effects = 'none'\n",
    )?;
    ensure!(
        cargo_command(&ws.path)
            .args(["generate-lockfile", "--offline"])
            .status()?
            .success()
    );
    ws.commit("Initialize workspace")?;
    ws.tag("v0.2.0", "Release core")?;
    std::fs::create_dir_all(ws.path.join(".changes"))?;
    std::fs::write(
        ws.path.join(".changes/core.md"),
        "---\ncore = 'patch'\n---\n\nFix core.\n",
    )?;
    ws.commit("Review core release")?;
    Ok(ws)
}

#[test]
fn unpublished_dependents_receive_only_required_workspace_edits() {
    finish_test((|| -> Result<()> {
        for (requirement, bump, include) in [
            (Some("0.2.0"), "patch", false),
            (Some("0.2.0"), "minor", false),
            (None, "minor", false),
            (Some("=0.2.0"), "minor", true),
        ] {
            let ws = workspace(requirement, false)?;
            let provider = ws.path.join("crates/provider/Cargo.toml");
            let original_provider = std::fs::read_to_string(&provider)?;
            let mut args = vec![
                "rail",
                "release",
                "check",
                "core",
                "--bump",
                bump,
                "--skip-tag",
                "--format",
                "json",
            ];
            if include {
                args.push("--include-dependents");
            }
            let check = run_cargo_rail(&ws.path, &args)?;
            assert_eq!(check.status.code(), Some(1), "{check:?}");
            let check: Value = serde_json::from_slice(&check.stdout)?;
            assert_eq!(
                check["release_plan"]["canonical_crate_order"],
                serde_json::json!(["core"])
            );
            assert_eq!(check["release_plan"]["crates"][0]["previous_tag"], "v0.2.0");
            assert_eq!(std::fs::read_to_string(&provider)?, original_provider);
            let mut run = vec!["rail", "release", "run", "core", "--bump", bump, "--skip-tag", "--yes"];
            if include {
                run.push("--include-dependents");
            }
            let output = run_cargo_rail(&ws.path, &run)?;
            assert_eq!(output.status.code(), Some(0), "{output:?}");
            let metadata = cargo_command(&ws.path)
                .args(["metadata", "--offline", "--locked", "--format-version", "1"])
                .output()?;
            assert!(metadata.status.success(), "{metadata:?}");
            let metadata: Value = serde_json::from_slice(&metadata.stdout)?;
            let provider_metadata = metadata["packages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["name"] == "provider")
                .unwrap();
            assert_eq!(provider_metadata["version"], "0.1.0");
            assert!(!ws.path.join("crates/provider/CHANGELOG.md").exists());
            assert_eq!(
                String::from_utf8(git(&ws.path, &["tag", "--list"])?.stdout)?.trim(),
                "v0.2.0"
            );
            if requirement.is_none() || bump == "patch" {
                assert_eq!(std::fs::read_to_string(&provider)?, original_provider);
            }
            if requirement == Some("=0.2.0") {
                assert!(std::fs::read_to_string(&provider)?.contains("=0.3.0"));
            }
        }
        Ok(())
    })());
}

#[test]
fn published_dependents_keep_the_strict_release_closure() {
    finish_test((|| -> Result<()> {
        let ws = workspace(Some("0.2.0"), true)?;
        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail", "release", "check", "core", "--bump", "patch", "--format", "json",
            ],
        )?;
        assert_eq!(output.status.code(), Some(2));
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert!(
            value["message"]
                .as_str()
                .unwrap()
                .contains("partial release would leave dependent crate(s) out of sync: provider")
        );
        Ok(())
    })());
}

#[test]
fn per_crate_tag_formats_select_independent_previous_releases() {
    finish_test((|| -> Result<()> {
        let ws = workspace(Some("0.2.0"), false)?;
        let path = ws.path.join(".config/rail.toml");
        let mut config = std::fs::read_to_string(&path)?;
        config.push_str("\n[crates.provider.release]\ntag_format = '{crate}-{prefix}{version}'\n");
        std::fs::write(path, config)?;
        std::fs::write(
            ws.path.join(".changes/provider.md"),
            "---\nprovider = 'patch'\n---\n\nRelease provider.\n",
        )?;
        let check = run_cargo_rail(
            &ws.path,
            &["rail", "release", "check", "provider", "--skip-tag", "--format", "json"],
        )?;
        assert_eq!(check.status.code(), Some(1), "{check:?}");
        let check: Value = serde_json::from_slice(&check.stdout)?;
        assert_eq!(check["release_plan"]["crates"][0]["previous_tag"], Value::Null);
        assert_eq!(check["release_plan"]["crates"][0]["tag_name"], "provider-v0.1.1");
        ws.tag("provider-v0.1.0", "Provider release")?;
        let check = run_cargo_rail(
            &ws.path,
            &["rail", "release", "check", "provider", "--skip-tag", "--format", "json"],
        )?;
        assert_eq!(check.status.code(), Some(1), "{check:?}");
        let check: Value = serde_json::from_slice(&check.stdout)?;
        assert_eq!(check["release_plan"]["crates"][0]["previous_tag"], "provider-v0.1.0");

        // The public planner still accepts a caller-supplied global format.
        let ctx = cargo_rail::workspace::WorkspaceContext::build_with_snapshot(&ws.path)?;
        let mut release = ctx.config().expect("fixture configuration").release.clone();
        release.tag_format = "library-{prefix}{version}".into();
        let planner = cargo_rail::release::ReleasePlanner::new(&ctx, &release);
        let bump = "patch".parse()?;
        for (name, previous, tag) in [
            ("core", None, "library-v0.2.1"),
            ("provider", Some("provider-v0.1.0"), "provider-v0.1.1"),
        ] {
            let plan = planner.plan(
                Some(vec![name.into()]),
                &bump,
                cargo_rail::release::planner::DependentPolicy::RejectPartialClosure,
            )?;
            assert_eq!(plan.crates[0].previous_tag.as_deref(), previous);
            assert_eq!(plan.crates[0].tag_name, tag);
        }
        Ok(())
    })());
}

#[test]
fn release_check_json_contains_the_text_collision_warning() {
    finish_test((|| -> Result<()> {
        for (published, target, expected) in [(false, "provider", 1), (true, "core", 2)] {
            let ws = workspace(Some("0.2.0"), published)?;
            std::fs::write(
                ws.path.join(".changes/provider.md"),
                "---\nprovider = 'patch'\n---\n\nRelease provider.\n",
            )?;
            let text = run_cargo_rail(&ws.path, &["rail", "release", "check", target, "--skip-tag"])?;
            let json = run_cargo_rail(
                &ws.path,
                &["rail", "release", "check", target, "--skip-tag", "--format", "json"],
            )?;
            assert_eq!(text.status.code(), Some(expected), "{text:?}");
            assert_eq!(json.status.code(), Some(expected), "{json:?}");
            let value: Value = serde_json::from_slice(&json.stdout)?;
            let warning = "release.tag_format does not contain {crate} placeholder. In monorepos, this may cause tag collisions between crates.";
            assert!(String::from_utf8_lossy(&text.stderr).contains(warning));
            assert!(
                value["warnings"]
                    .as_array()
                    .unwrap()
                    .contains(&Value::String(warning.to_owned()))
            );
            assert!(json.stderr.is_empty());
        }
        Ok(())
    })());
}

#[test]
fn incompatible_compound_path_requirements_are_rejected_before_mutation() {
    finish_test((|| -> Result<()> {
        let ws = workspace(Some(">=0.2.0, <0.3.0"), false)?;
        let before = git(&ws.path, &["status", "--porcelain"])?.stdout;
        let provider = ws.path.join("crates/provider/Cargo.toml");
        let original = std::fs::read(&provider)?;
        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "core",
                "--bump",
                "minor",
                "--skip-tag",
                "--yes",
            ],
        )?;
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("requirement"),
            "{output:?}"
        );
        assert_eq!(std::fs::read(&provider)?, original);
        assert_eq!(git(&ws.path, &["status", "--porcelain"])?.stdout, before);
        assert!(!ws.path.join("crates/core/CHANGELOG.md").exists());
        Ok(())
    })());
}

#[test]
fn published_dependents_remain_in_closure_through_an_unpublished_member() {
    finish_test((|| -> Result<()> {
        let ws = workspace(Some("0.2.0"), false)?;
        ws.add_crate(
            "published",
            "0.1.0",
            &[("provider", "{ path = '../provider', version = '0.1.0' }")],
        )?;
        ensure!(
            cargo_command(&ws.path)
                .args(["generate-lockfile", "--offline"])
                .status()?
                .success()
        );
        ws.commit("Add a published transitive dependent")?;
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "check", "core", "--bump", "patch", "--skip-tag"],
        )?;
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("published"));
        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "check",
                "core",
                "--bump",
                "patch",
                "--include-dependents",
                "--skip-tag",
            ],
        )?;
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("missing change files for published"), "{stderr}");
        assert!(!stderr.contains("missing change files for provider"), "{stderr}");
        Ok(())
    })());
}

#[test]
fn archive_host_manifest_matches_its_schema_and_declared_assets() {
    finish_test((|| -> Result<()> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let config = cargo_rail::config::RailConfig::load(root)?;
        let directory = tempfile::tempdir()?;
        let files = &config.release.artifacts["cargo-rail"].files;
        for name in files.keys().filter(|name| name.ends_with(".zip")) {
            std::fs::write(directory.path().join(name), b"fixture archive")?;
        }
        let output = std::process::Command::new("python3")
            .arg("-B")
            .arg(root.join("scripts/package-archive-hosts.py"))
            .arg(directory.path())
            .output()?;
        assert!(output.status.success(), "{output:?}");
        let bytes = std::fs::read(directory.path().join("cargo-rail-archive-hosts-v1.json"))?;
        let value: Value = serde_json::from_slice(&bytes)?;
        let schema: Value = serde_json::from_str(include_str!("../../schemas/archive-hosts-v1.schema.json"))?;
        let validator = jsonschema::validator_for(&schema)
            .map_err(|error| anyhow::anyhow!("invalid archive-host schema: {error}"))?;
        assert!(validator.is_valid(&value), "{value}");
        assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(value["unsupported_host_installation"], "source");
        let archives = value["archives"].as_object().unwrap();
        assert!(!archives.contains_key("aarch64-pc-windows-msvc"));
        for (host, archive) in archives {
            assert_eq!(archive.as_str().unwrap(), format!("cargo-rail-{host}.zip"));
            assert!(files.contains_key(archive.as_str().unwrap()));
        }
        Ok(())
    })());
}

#[test]
fn release_projection_updates_renamed_inherited_and_target_path_requirements() {
    finish_test((|| -> Result<()> {
        let ws = workspace(Some("0.2.0"), false)?;
        let root_path = ws.path.join("Cargo.toml");
        let root = std::fs::read_to_string(&root_path)?.replace("[workspace.dependencies]", "[workspace.dependencies]\ncore_alias = { package = 'core', path = 'crates/core', version = '~0.2.0' } # keep inherited policy");
        std::fs::write(&root_path, root)?;
        let provider = ws.path.join("crates/provider/Cargo.toml");
        let declaration = "\n[target.'cfg(any(unix, windows))'.build-dependencies]\ncore_alias = { workspace = true } # keep target and alias\n";
        let mut provider_manifest: toml_edit::DocumentMut = std::fs::read_to_string(&provider)?.parse()?;
        provider_manifest["dependencies"].as_table_mut().unwrap().remove("core");
        std::fs::write(&provider, format!("{provider_manifest}{declaration}"))?;
        ensure!(
            cargo_command(&ws.path)
                .args(["generate-lockfile", "--offline"])
                .status()?
                .success()
        );
        let initial = cargo_command(&ws.path)
            .args(["metadata", "--locked", "--offline", "--format-version", "1"])
            .output()?;
        ensure!(initial.status.success(), "invalid Cargo fixture: {initial:?}");
        ws.commit("Use a renamed inherited target dependency")?;
        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "core",
                "--bump",
                "minor",
                "--skip-tag",
                "--yes",
            ],
        )?;
        assert!(output.status.success(), "{output:?}");
        let root = std::fs::read_to_string(&root_path)?;
        let manifest: toml_edit::DocumentMut = root.parse()?;
        assert_eq!(
            manifest["workspace"]["dependencies"]["core_alias"]["version"].as_str(),
            Some("~0.3.0")
        );
        assert!(root.contains("# keep inherited policy"), "{root}");
        assert!(std::fs::read_to_string(&provider)?.contains(declaration));
        let metadata = cargo_command(&ws.path)
            .args(["metadata", "--locked", "--offline", "--format-version", "1"])
            .output()?;
        assert!(metadata.status.success(), "{metadata:?}");
        let metadata: Value = serde_json::from_slice(&metadata.stdout)?;
        assert!(
            metadata["packages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|package| package["name"] == "provider" && package["version"] == "0.1.0")
        );
        Ok(())
    })());
}

#[test]
fn release_cargo_writes_use_portable_path_order_across_independent_workspaces() {
    finish_test((|| -> Result<()> {
        let ws = workspace(None, false)?;
        for name in ["aux", "aux-extra"] {
            let root = ws.path.join(name);
            std::fs::create_dir_all(root.join("src"))?;
            std::fs::write(
                root.join("Cargo.toml"),
                format!(
                    "[workspace]\n[package]\nname = '{name}'\nversion = '0.1.0'\npublish = false\nedition = '2021'\n[dependencies]\ncore = {{ path = '../crates/core' }}\n"
                ),
            )?;
            std::fs::write(root.join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n")?;
            let lock = cargo_command(&root).args(["generate-lockfile", "--offline"]).output()?;
            ensure!(lock.status.success(), "{lock:?}");
        }
        ws.write_release_config("tag_format = '{prefix}{version}'\nsemver_check = 'off'\nsign_tags = false\nremote_effects = 'none'\nauxiliary_cargo_manifests = ['aux/Cargo.toml', 'aux-extra/Cargo.toml']\n")?;
        ws.commit("Declare independent workspaces with prefix-related names")?;
        let check = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "check",
                "core",
                "--bump",
                "patch",
                "--skip-tag",
                "--format",
                "json",
            ],
        )?;
        assert_eq!(check.status.code(), Some(1), "{check:?}");
        let report: Value = serde_json::from_slice(&check.stdout)?;
        let paths = report["release_plan"]["cargo_writes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|write| write["path"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            [
                "Cargo.lock",
                "aux-extra/Cargo.lock",
                "aux/Cargo.lock",
                "crates/core/Cargo.toml"
            ]
        );
        let prepare = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "core",
                "--bump",
                "patch",
                "--skip-tag",
                "--prepare",
                "--yes",
            ],
        )?;
        assert!(prepare.status.success(), "{prepare:?}");
        for root in [&ws.path, &ws.path.join("aux"), &ws.path.join("aux-extra")] {
            let metadata = cargo_command(root)
                .args(["metadata", "--locked", "--offline", "--format-version", "1"])
                .output()?;
            assert!(metadata.status.success(), "{metadata:?}");
        }
        Ok(())
    })());
}
