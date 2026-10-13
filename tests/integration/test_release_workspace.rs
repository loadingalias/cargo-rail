//! Package release selection and resulting workspace validity.

use crate::helpers::{TestWorkspace, cargo_command, finish_test, git, run_cargo_rail};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::process::Output;

const RELEASE_CONFIG: &str = "tag_format = 'v{version}'\nsemver_check = 'off'\nsign_tags = false\npublish = 'none'\n";

/// `core` released at 0.2.0 with reviewed patch intent, and `provider` depending on it, on origin.
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
    ws.write_release_config(RELEASE_CONFIG)?;
    generate_lockfile(&ws.path)?;
    ws.commit("Initialize workspace")?;
    ws.tag("v0.2.0", "Release core")?;
    std::fs::create_dir_all(ws.path.join(".changes"))?;
    std::fs::write(
        ws.path.join(".changes/core.md"),
        "---\ncore = 'patch'\n---\n\nFix core.\n",
    )?;
    ws.commit("Review core release")?;
    ws.with_origin()?;
    Ok(ws)
}

fn generate_lockfile(root: &std::path::Path) -> Result<()> {
    let output = cargo_command(root).args(["generate-lockfile", "--offline"]).output()?;
    ensure!(output.status.success(), "{output:?}");
    Ok(())
}

fn release(ws: &TestWorkspace, args: &[&str]) -> Result<Output> {
    let mut arguments = vec!["rail", "release"];
    arguments.extend_from_slice(args);
    run_cargo_rail(&ws.path, &arguments)
}

fn crate_names(report: &Value) -> Vec<&str> {
    report["crates"]
        .as_array()
        .map(|crates| crates.iter().filter_map(|entry| entry["name"].as_str()).collect())
        .unwrap_or_default()
}

fn tags(ws: &TestWorkspace) -> Result<Vec<String>> {
    Ok(String::from_utf8(git(&ws.path, &["tag", "--list"])?.stdout)?
        .lines()
        .map(str::to_owned)
        .collect())
}

#[test]
fn unpublished_dependents_receive_only_required_workspace_edits() {
    finish_test((|| -> Result<()> {
        for (requirement, bump, tag) in [
            (Some("0.2.0"), "patch", "v0.2.1"),
            (Some("0.2.0"), "minor", "v0.3.0"),
            (None, "minor", "v0.3.0"),
            (Some("=0.2.0"), "minor", "v0.3.0"),
        ] {
            let ws = workspace(requirement, false)?;
            let provider = ws.path.join("crates/provider/Cargo.toml");
            let original_provider = std::fs::read_to_string(&provider)?;
            let check = release(&ws, &["core", "--bump", bump, "--dry-run", "--format", "json"])?;
            assert_eq!(check.status.code(), Some(0), "{check:?}");
            let check: Value = serde_json::from_slice(&check.stdout)?;
            assert_eq!(
                crate_names(&check),
                ["core"],
                "an unpublished dependent is not released"
            );
            assert_eq!(std::fs::read_to_string(&provider)?, original_provider);
            // Automatic selection names no history range, so the previous release tag was found.
            let auto = release(&ws, &["core", "--dry-run", "--format", "json"])?;
            let auto: Value = serde_json::from_slice(&auto.stdout)?;
            assert_eq!(auto["crates"][0]["reason"], "auto: reviewed change files -> patch");

            let output = release(&ws, &["core", "--bump", bump, "--yes"])?;
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
            assert_eq!(tags(&ws)?, ["v0.2.0", tag]);
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
fn published_dependents_join_the_release_closure() {
    finish_test((|| -> Result<()> {
        let ws = workspace(Some("0.2.0"), true)?;
        let output = release(&ws, &["core", "--bump", "patch", "--dry-run", "--format", "json"])?;
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(
            crate_names(&value),
            ["core", "provider"],
            "naming core releases its published dependent: {value}"
        );
        assert_eq!(value["crates"][1]["version"], "0.1.1");
        Ok(())
    })());
}

#[test]
fn per_crate_tag_formats_select_independent_previous_releases() {
    finish_test((|| -> Result<()> {
        let ws = workspace(Some("0.2.0"), false)?;
        let path = ws.path.join(".config/rail.toml");
        let mut config = std::fs::read_to_string(&path)?;
        config.push_str("\n[crates.provider.release]\ntag_format = '{crate}-v{version}'\n");
        std::fs::write(path, config)?;
        std::fs::write(
            ws.path.join(".changes/provider.md"),
            "---\nprovider = 'patch'\n---\n\nRelease provider.\n",
        )?;
        ws.commit("Give provider its own tag line")?;
        ws.push()?;
        let check = release(&ws, &["provider", "--dry-run", "--format", "json"])?;
        assert_eq!(check.status.code(), Some(0), "{check:?}");
        let check: Value = serde_json::from_slice(&check.stdout)?;
        assert_eq!(crate_names(&check), ["provider"]);
        assert_eq!(
            check["crates"][0]["reason"],
            "auto: reviewed change files -> patch (no previous tag: full history)"
        );
        assert_eq!(check["crates"][0]["tag"], "provider-v0.1.1");
        ws.tag("provider-v0.1.0", "Provider release")?;
        ws.push()?;
        let check = release(&ws, &["provider", "--dry-run", "--format", "json"])?;
        assert_eq!(check.status.code(), Some(0), "{check:?}");
        let check: Value = serde_json::from_slice(&check.stdout)?;
        assert_eq!(check["crates"][0]["reason"], "auto: reviewed change files -> patch");

        // The public planner still accepts a caller-supplied global format.
        let ctx = cargo_rail::workspace::WorkspaceContext::build_with_snapshot(&ws.path)?;
        let mut release = ctx.config().expect("fixture configuration").release.clone();
        release.tag_format = "library-v{version}".into();
        let planner = cargo_rail::release::ReleasePlanner::new(&ctx, &release);
        let bump = "patch".parse()?;
        for (name, previous, tag) in [
            ("core", None, "library-v0.2.1"),
            ("provider", Some("provider-v0.1.0"), "provider-v0.1.1"),
        ] {
            let plan = planner.plan(Some(vec![name.into()]), &bump)?;
            assert_eq!(plan.crates[0].previous_tag.as_deref(), previous);
            assert_eq!(plan.crates[0].tag_name, tag);
            assert!(
                plan.cargo_writes.is_empty(),
                "planning leaves Cargo writes to the projection"
            );
        }
        Ok(())
    })());
}

#[test]
fn release_json_contains_the_text_collision_warning() {
    finish_test((|| -> Result<()> {
        for (published, target) in [(false, "provider"), (true, "core")] {
            let ws = workspace(Some("0.2.0"), published)?;
            std::fs::write(
                ws.path.join(".changes/provider.md"),
                "---\nprovider = 'patch'\n---\n\nRelease provider.\n",
            )?;
            ws.commit("Review provider release")?;
            ws.push()?;
            let text = release(&ws, &[target, "--dry-run"])?;
            let json = release(&ws, &[target, "--dry-run", "--format", "json"])?;
            assert_eq!(text.status.code(), Some(0), "{text:?}");
            assert_eq!(json.status.code(), Some(0), "{json:?}");
            let value: Value = serde_json::from_slice(&json.stdout)?;
            let warning = "release.tag_format does not contain {crate}; in a workspace that releases several crates, \
                           two crates at one version would need the same tag";
            assert!(String::from_utf8_lossy(&text.stderr).contains(warning), "{text:?}");
            assert!(
                value["warnings"]
                    .as_array()
                    .unwrap()
                    .contains(&Value::String(warning.to_owned())),
                "{value}"
            );
            assert!(json.stderr.is_empty(), "{json:?}");
        }
        Ok(())
    })());
}

#[test]
fn incompatible_compound_path_requirements_are_rejected_before_mutation() {
    finish_test((|| -> Result<()> {
        let ws = workspace(Some(">=0.2.0, <0.3.0"), false)?;
        let before = git(&ws.path, &["status", "--porcelain"])?.stdout;
        let head = git(&ws.path, &["rev-parse", "HEAD"])?.stdout;
        let provider = ws.path.join("crates/provider/Cargo.toml");
        let original = std::fs::read(&provider)?;
        let output = release(&ws, &["core", "--bump", "minor", "--yes"])?;
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("requirement"),
            "{output:?}"
        );
        assert_eq!(std::fs::read(&provider)?, original);
        assert_eq!(git(&ws.path, &["status", "--porcelain"])?.stdout, before);
        assert_eq!(git(&ws.path, &["rev-parse", "HEAD"])?.stdout, head);
        assert!(!ws.path.join("crates/core/CHANGELOG.md").exists());
        assert_eq!(tags(&ws)?, ["v0.2.0"]);
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
        generate_lockfile(&ws.path)?;
        ws.commit("Add a published transitive dependent")?;
        ws.push()?;
        let output = release(&ws, &["core", "--bump", "patch", "--dry-run"])?;
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
        let files = &config.release.assets["cargo-rail"];
        for name in files.iter().filter(|name| name.ends_with(".zip")) {
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
            let archive = archive.as_str().unwrap();
            assert_eq!(archive, format!("cargo-rail-{host}.zip"));
            assert!(
                files.iter().any(|file| file == archive),
                "{archive} is not a declared asset"
            );
        }
        assert!(files.iter().any(|file| file == "cargo-rail-archive-hosts-v1.json"));
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
        generate_lockfile(&ws.path)?;
        let initial = cargo_command(&ws.path)
            .args(["metadata", "--locked", "--offline", "--format-version", "1"])
            .output()?;
        ensure!(initial.status.success(), "invalid Cargo fixture: {initial:?}");
        ws.commit("Use a renamed inherited target dependency")?;
        ws.push()?;
        let output = release(&ws, &["core", "--bump", "minor", "--yes"])?;
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
fn release_rejects_undeclared_path_dependent_lockfiles_before_mutation() {
    finish_test((|| -> Result<()> {
        let ws = workspace(Some("0.2.0"), false)?;
        let auxiliary = ws.path.join("auxiliary");
        std::fs::create_dir_all(auxiliary.join("src"))?;
        std::fs::write(
            auxiliary.join("Cargo.toml"),
            "[workspace]\n[package]\nname = 'auxiliary'\nversion = '0.1.0'\npublish = false\nedition = '2021'\n[dependencies]\ncore = { path = '../crates/core', version = '0.2.0' }\n",
        )?;
        std::fs::write(auxiliary.join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n")?;
        generate_lockfile(&auxiliary)?;
        ws.commit("Track an undeclared independent workspace")?;
        ws.push()?;
        let before = git(&ws.path, &["rev-parse", "HEAD"])?.stdout;
        let lock_before = std::fs::read(auxiliary.join("Cargo.lock"))?;
        for args in [
            &["core", "--bump", "patch", "--dry-run", "--format", "json"][..],
            &["core", "--bump", "patch", "--yes"][..],
        ] {
            let output = release(&ws, args)?;
            assert_eq!(output.status.code(), Some(2), "{output:?}");
            let diagnostic = if args.contains(&"json") {
                let error: Value = serde_json::from_slice(&output.stdout)?;
                format!(
                    "{}\n{}",
                    error["message"].as_str().expect("JSON error message"),
                    error["help"].as_str().expect("JSON error help")
                )
            } else {
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
            };
            assert!(diagnostic.contains("auxiliary/Cargo.lock"), "{diagnostic}");
            let manifest = std::path::Path::new("auxiliary").join("Cargo.toml");
            assert!(diagnostic.contains(&manifest.display().to_string()), "{diagnostic}");
            assert!(diagnostic.contains("release.auxiliary_cargo_manifests"), "{diagnostic}");
            assert_eq!(git(&ws.path, &["rev-parse", "HEAD"])?.stdout, before);
            assert!(git(&ws.path, &["status", "--porcelain"])?.stdout.is_empty());
            assert_eq!(std::fs::read(auxiliary.join("Cargo.lock"))?, lock_before);
            assert_eq!(tags(&ws)?, ["v0.2.0"]);
        }
        let metadata = cargo_command(&auxiliary)
            .args(["metadata", "--locked", "--offline", "--format-version", "1"])
            .output()?;
        assert!(metadata.status.success(), "{metadata:?}");
        Ok(())
    })());
}

#[test]
fn release_covers_inactive_transitive_path_dependencies_in_independent_lockfiles() {
    finish_test((|| -> Result<()> {
        let ws = workspace(None, false)?;
        for (name, dependency) in [
            (
                "auxiliary",
                "bridge = { path = '../bridge', optional = true, features = ['leaf'] }",
            ),
            (
                "bridge",
                "leaf = { path = '../leaf', optional = true, features = ['core'] }",
            ),
            (
                "leaf",
                "core = { path = '../crates/core', version = '^0.2.0', optional = true }",
            ),
        ] {
            let root = ws.path.join(name);
            std::fs::create_dir_all(root.join("src"))?;
            std::fs::write(
                root.join("Cargo.toml"),
                format!(
                    "[workspace]\n[package]\nname = '{name}'\nversion = '0.1.0'\npublish = false\nedition = '2021'\n[dependencies]\n{dependency}\n"
                ),
            )?;
            std::fs::write(root.join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n")?;
        }
        let auxiliary = ws.path.join("auxiliary");
        generate_lockfile(&auxiliary)?;
        ws.commit("Track an inactive transitive path dependency")?;
        ws.push()?;
        let lock_before = std::fs::read(auxiliary.join("Cargo.lock"))?;
        assert!(String::from_utf8_lossy(&lock_before).contains("name = \"core\""));
        let check = release(&ws, &["core", "--bump", "minor", "--dry-run", "--format", "json"])?;
        assert_eq!(check.status.code(), Some(2), "{check:?}");
        assert!(
            String::from_utf8_lossy(&check.stdout).contains("auxiliary/Cargo.lock"),
            "{check:?}"
        );
        assert_eq!(std::fs::read(auxiliary.join("Cargo.lock"))?, lock_before);
        ws.write_release_config(&format!(
            "{RELEASE_CONFIG}auxiliary_cargo_manifests = ['auxiliary/Cargo.toml']\n"
        ))?;
        ws.commit("Declare the independent workspace")?;
        ws.push()?;
        let release = release(&ws, &["core", "--bump", "minor", "--yes"])?;
        assert!(release.status.success(), "{release:?}");
        let leaf: toml_edit::DocumentMut = std::fs::read_to_string(ws.path.join("leaf/Cargo.toml"))?.parse()?;
        assert_eq!(leaf["package"]["version"].as_str(), Some("0.1.0"));
        assert_eq!(leaf["dependencies"]["core"]["version"].as_str(), Some("^0.3.0"));
        let metadata = cargo_command(&auxiliary)
            .args(["metadata", "--locked", "--offline", "--format-version", "1"])
            .output()?;
        assert!(metadata.status.success(), "{metadata:?}");
        let lock: toml_edit::DocumentMut = std::fs::read_to_string(auxiliary.join("Cargo.lock"))?.parse()?;
        assert!(lock["package"].as_array_of_tables().unwrap().iter().any(|package| package["name"].as_str() == Some("core") && package["version"].as_str() == Some("0.3.0")));
        Ok(())
    })());
}

#[test]
fn release_rejects_unattributed_local_lockfile_identities_until_the_workspace_is_declared() {
    finish_test((|| -> Result<()> {
        let ws = workspace(None, false)?;
        let auxiliary = ws.path.join("auxiliary");
        std::fs::create_dir_all(auxiliary.join("src"))?;
        std::fs::write(
            auxiliary.join("Cargo.toml"),
            "[workspace]\n[package]\nname = 'auxiliary'\nversion = '0.1.0'\npublish = false\nedition = '2021'\n[dependencies]\ncore = { version = '0.2.0', optional = true }\n[patch.crates-io]\ncore = { path = '../crates/core' }\n",
        )?;
        std::fs::write(auxiliary.join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n")?;
        generate_lockfile(&auxiliary)?;
        ws.commit("Track an inactive path patch absent from resolved metadata")?;
        ws.push()?;
        assert!(std::fs::read_to_string(auxiliary.join("Cargo.lock"))?.contains("name = \"core\""));
        let check = release(&ws, &["core", "--bump", "patch", "--dry-run", "--format", "json"])?;
        assert_eq!(check.status.code(), Some(2), "{check:?}");
        let diagnostic = String::from_utf8(check.stdout)?;
        assert!(diagnostic.contains("could not attribute"), "{diagnostic}");
        assert!(diagnostic.contains("auxiliary/Cargo.lock"), "{diagnostic}");
        assert!(diagnostic.contains("release.auxiliary_cargo_manifests"), "{diagnostic}");
        ws.write_release_config(&format!(
            "{RELEASE_CONFIG}auxiliary_cargo_manifests = ['auxiliary/Cargo.toml']\n"
        ))?;
        ws.commit("Declare the independent path-patched workspace")?;
        ws.push()?;
        let release = release(&ws, &["core", "--bump", "patch", "--yes"])?;
        assert!(release.status.success(), "{release:?}");
        let metadata = cargo_command(&auxiliary)
            .args([
                "metadata",
                "--all-features",
                "--locked",
                "--offline",
                "--format-version",
                "1",
            ])
            .output()?;
        assert!(metadata.status.success(), "{metadata:?}");
        let metadata: Value = serde_json::from_slice(&metadata.stdout)?;
        assert!(
            metadata["packages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|package| package["name"] == "core" && package["version"] == "0.2.1")
        );
        Ok(())
    })());
}

#[test]
fn unrelated_independent_lockfile_with_the_same_package_identity_does_not_block_release() {
    finish_test((|| -> Result<()> {
        let ws = workspace(None, false)?;
        let auxiliary = ws.path.join("auxiliary");
        std::fs::create_dir_all(auxiliary.join("src"))?;
        std::fs::write(
            auxiliary.join("Cargo.toml"),
            "[workspace]\n[package]\nname = 'core'\nversion = '0.2.0'\npublish = false\nedition = '2021'\n",
        )?;
        std::fs::write(auxiliary.join("src/lib.rs"), "pub fn independent() {}\n")?;
        generate_lockfile(&auxiliary)?;
        ws.commit("Track an unrelated package with the same name and version")?;
        ws.push()?;
        let lock_before = std::fs::read(auxiliary.join("Cargo.lock"))?;
        let release = release(&ws, &["core", "--bump", "patch", "--yes"])?;
        assert!(release.status.success(), "{release:?}");
        assert_eq!(std::fs::read(auxiliary.join("Cargo.lock"))?, lock_before);
        let metadata = cargo_command(&auxiliary)
            .args(["metadata", "--locked", "--offline", "--format-version", "1"])
            .output()?;
        assert!(metadata.status.success(), "{metadata:?}");
        Ok(())
    })());
}

#[test]
fn release_cargo_writes_cover_independent_workspaces_with_prefix_related_names() {
    finish_test((|| -> Result<()> {
        let ws = workspace(None, false)?;
        for name in ["auxiliary", "auxiliary-extra"] {
            let root = ws.path.join(name);
            std::fs::create_dir_all(root.join("src"))?;
            std::fs::write(
                root.join("Cargo.toml"),
                format!(
                    "[workspace]\n[package]\nname = '{name}'\nversion = '0.1.0'\npublish = false\nedition = '2021'\n[dependencies]\ncore = {{ path = '../crates/core' }}\n"
                ),
            )?;
            std::fs::write(root.join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n")?;
            generate_lockfile(&root)?;
        }
        ws.write_release_config(&format!(
            "{RELEASE_CONFIG}auxiliary_cargo_manifests = ['auxiliary/Cargo.toml', 'auxiliary-extra/Cargo.toml']\n"
        ))?;
        ws.commit("Declare independent workspaces with prefix-related names")?;
        ws.push()?;
        let check = release(&ws, &["core", "--bump", "patch", "--dry-run"])?;
        assert_eq!(check.status.code(), Some(0), "{check:?}");
        let released = release(&ws, &["core", "--bump", "patch", "--yes"])?;
        assert!(released.status.success(), "{released:?}");
        let changed = String::from_utf8(git(&ws.path, &["diff", "--name-only", "HEAD^", "HEAD"])?.stdout)?;
        let changed = changed.lines().collect::<Vec<_>>();
        for path in [
            "Cargo.lock",
            "auxiliary-extra/Cargo.lock",
            "auxiliary/Cargo.lock",
            "crates/core/Cargo.toml",
        ] {
            assert!(changed.contains(&path), "the release commit omits {path}: {changed:?}");
        }
        for root in [&ws.path, &ws.path.join("auxiliary"), &ws.path.join("auxiliary-extra")] {
            let metadata = cargo_command(root)
                .args(["metadata", "--locked", "--offline", "--format-version", "1"])
                .output()?;
            assert!(metadata.status.success(), "{metadata:?}");
        }
        Ok(())
    })());
}
