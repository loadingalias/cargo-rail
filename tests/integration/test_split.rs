//! Integration tests for split operations

use crate::helpers::{TestWorkspace, git, run_cargo_rail};
use anyhow::Result;
use cargo_rail::git::mappings::{HistorySide, MappingStore, repository_identity};
use tempfile::TempDir;

#[test]
fn test_split_single_crate_basic() {
    let result: Result<()> = (|| {
        // Create monorepo with a single crate
        let ws = TestWorkspace::new()?;
        ws.add_crate("mylib", "0.1.0", &[])?;
        ws.commit("Add mylib")?;

        // Create split repo target
        let split_dir = TempDir::new()?;
        let split_path = split_dir.path();

        // Create rail.toml config
        let config = format!(
            r#"[workspace]
root = "."

[crates.mylib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_path.display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        // Perform split
        run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;

        // Verify split structure
        assert!(split_path.join("Cargo.toml").exists(), "Cargo.toml should exist");
        assert!(split_path.join("src/lib.rs").exists(), "src/lib.rs should exist");
        assert!(split_path.join("README.md").exists(), "README.md should exist");

        // Verify Cargo.toml was transformed (no workspace inheritance)
        let cargo_toml = std::fs::read_to_string(split_path.join("Cargo.toml"))?;
        assert!(
            !cargo_toml.contains("workspace = true"),
            "Should not contain workspace inheritance"
        );
        assert!(
            cargo_toml.contains("edition = \"2021\""),
            "Should have flattened edition"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_initializes_the_configured_branch() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-configured-branch")?;
        ws.add_crate("mylib", "0.1.0", &[])?;
        ws.commit("Add mylib")?;
        let split_dir = TempDir::new()?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                r#"[crates.mylib.split]
remote = "{}"
branch = "stable"
mode = "single"
"#,
                split_dir.path().display().to_string().replace('\\', "\\\\")
            ),
        )?;

        let output = run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;
        assert!(output.status.success());
        let branch = git(split_dir.path(), &["branch", "--show-current"])?;
        assert_eq!(String::from_utf8_lossy(&branch.stdout).trim(), "stable");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_preserves_git_history() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("mylib", "0.1.0", &[])?;
        ws.commit("Initial mylib")?;

        // Make several commits
        ws.modify_file("mylib", "src/lib.rs", "// Version 1")?;
        ws.commit("Update mylib v1")?;

        ws.modify_file("mylib", "src/lib.rs", "// Version 2")?;
        ws.commit("Update mylib v2")?;

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

        run_cargo_rail(&ws.path, &["rail", "split", "run", "mylib", "--yes", "--allow-dirty"])?;

        // Check git history in split
        let log_output = git(split_dir.path(), &["log", "--oneline"])?;
        let log = String::from_utf8_lossy(&log_output.stdout);

        assert!(log.contains("Initial mylib"), "Should contain initial commit");
        assert!(log.contains("Update mylib v1"), "Should contain v1 commit");
        assert!(log.contains("Update mylib v2"), "Should contain v2 commit");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_migrates_legacy_notes_losslessly_into_history() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("legacy-lib", "0.1.0", &[])?;
        ws.commit("Add legacy-lib")?;
        let source_commit = String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();
        let source_identity = repository_identity(&ws.path)?;

        let target = TempDir::new()?;
        git(target.path(), &["init", "-b", "main"])?;
        git(target.path(), &["config", "user.name", "Test User"])?;
        git(target.path(), &["config", "user.email", "test@example.com"])?;
        git(target.path(), &["commit", "--allow-empty", "-m", "Legacy split head"])?;
        let legacy_target = String::from_utf8(git(target.path(), &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_string();
        git(target.path(), &["fetch", "--quiet", ws.path.to_str().unwrap(), "HEAD"])?;
        git(
            target.path(),
            &[
                "notes",
                "--ref",
                "refs/notes/rail/legacy-lib",
                "add",
                "-m",
                &legacy_target,
                &source_commit,
            ],
        )?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[crates.legacy-lib.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
                target.path().display().to_string().replace('\\', "\\\\")
            ),
        )?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "legacy-lib", "--yes", "--allow-dirty"],
        )?;
        assert!(output.status.success());
        let message = git(target.path(), &["log", "-1", "--format=%B"])?;
        let message = String::from_utf8_lossy(&message.stdout);
        assert!(message.contains("Rail-Origin: v1"));
        assert!(message.contains(&format!("target={legacy_target}")));

        git(target.path(), &["update-ref", "-d", "refs/notes/rail/legacy-lib"])?;
        let clone_parent = TempDir::new()?;
        let clone = clone_parent.path().join("clone");
        git(
            clone_parent.path(),
            &["clone", target.path().to_str().unwrap(), "clone"],
        )?;
        let mut mappings = MappingStore::new("legacy-lib".to_string());
        mappings.load_history(&clone, HistorySide::Target, &source_identity)?;
        assert_eq!(mappings.get_mapping(&source_commit), Some(legacy_target));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_filters_unrelated_commits() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.add_crate("lib-b", "0.1.0", &[])?;
        ws.commit("Add both libs")?;

        // Modify only lib-a
        ws.modify_file("lib-a", "src/lib.rs", "// Changed A")?;
        ws.commit("Update lib-a")?;

        // Modify only lib-b
        ws.modify_file("lib-b", "src/lib.rs", "// Changed B")?;
        ws.commit("Update lib-b")?;

        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.lib-a.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        run_cargo_rail(&ws.path, &["rail", "split", "run", "lib-a", "--yes", "--allow-dirty"])?;

        // Check that only lib-a commits are in split
        let log_output = git(split_dir.path(), &["log", "--oneline"])?;
        let log = String::from_utf8_lossy(&log_output.stdout);

        assert!(log.contains("Add both libs"), "Should contain initial commit");
        assert!(log.contains("Update lib-a"), "Should contain lib-a update");
        assert!(!log.contains("Update lib-b"), "Should NOT contain lib-b update");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_transforms_path_dependencies() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("lib-util", "0.1.0", &[])?;
        ws.add_crate(
            "lib-core",
            "0.2.0",
            &[("lib-util", r#"{ version = "0.1", path = "../lib-util" }"#)],
        )?;
        ws.commit("Add libs with dependency")?;

        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.lib-core.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "lib-core", "--yes", "--allow-dirty"],
        )?;

        // Check that path dependency was transformed to version dependency
        let cargo_toml = std::fs::read_to_string(split_dir.path().join("Cargo.toml"))?;

        assert!(!cargo_toml.contains("path ="), "Should not contain path dependencies");
        assert!(
            cargo_toml.contains("lib-util") && cargo_toml.contains("0.1"),
            "Should have version dependency on lib-util"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_combined_mode_multiple_crates() {
    let result: Result<()> = (|| {
        // Test combined mode: multiple crates split to one repo, preserving structure
        let ws = TestWorkspace::new()?;
        ws.add_crate("lib-core", "0.1.0", &[])?;
        ws.add_crate("service-api", "0.2.0", &[])?;
        ws.commit("Add lib-core and service-api")?;

        // Make changes to both crates
        ws.modify_file("lib-core", "src/lib.rs", "// Core functionality")?;
        ws.commit("Update lib-core")?;

        ws.modify_file("service-api", "src/lib.rs", "// API service")?;
        ws.commit("Update service-api")?;

        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.combined.split]
remote = "{}"
branch = "main"
mode = "combined"
members = ["lib-core", "service-api"]
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "combined", "--yes", "--allow-dirty"],
        )?;

        // Verify both crates exist with preserved structure
        let split_path = split_dir.path();
        assert!(
            split_path.join("crates/lib-core/Cargo.toml").exists(),
            "lib-core Cargo.toml should exist at crates/lib-core/Cargo.toml"
        );
        assert!(
            split_path.join("crates/lib-core/src/lib.rs").exists(),
            "lib-core lib.rs should exist at crates/lib-core/src/lib.rs"
        );
        assert!(
            split_path.join("crates/service-api/Cargo.toml").exists(),
            "service-api Cargo.toml should exist at crates/service-api/Cargo.toml"
        );
        assert!(
            split_path.join("crates/service-api/src/lib.rs").exists(),
            "service-api lib.rs should exist at crates/service-api/src/lib.rs"
        );

        // Verify content was copied correctly
        let core_content = std::fs::read_to_string(split_path.join("crates/lib-core/src/lib.rs"))?;
        assert!(
            core_content.contains("// Core functionality"),
            "lib-core should have correct content"
        );

        let api_content = std::fs::read_to_string(split_path.join("crates/service-api/src/lib.rs"))?;
        assert!(
            api_content.contains("// API service"),
            "service-api should have correct content"
        );

        // Verify git history includes commits for both crates
        let log_output = git(split_path, &["log", "--oneline"])?;
        let log = String::from_utf8_lossy(&log_output.stdout);
        assert!(
            log.contains("Add lib-core and service-api"),
            "Should contain initial commit"
        );
        assert!(log.contains("Update lib-core"), "Should contain lib-core update");
        assert!(log.contains("Update service-api"), "Should contain service-api update");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_release_flow_creates_tag_and_changelog() {
    let result: Result<()> = (|| {
        // Split a crate, then run release in the split repo to ensure tagging/changelog works.
        let ws = TestWorkspace::new()?;
        ws.add_crate("lib-release", "0.1.0", &[])?;
        ws.commit("Add lib-release")?;

        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.lib-release.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        // Perform split
        run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "lib-release", "--yes", "--allow-dirty"],
        )?;

        // Prepare release config inside split repo
        let split_root = split_dir.path();
        // Configure git user to allow tagging/commits in split repo
        git(split_root, &["config", "user.name", "Test Split"])?;
        git(split_root, &["config", "user.email", "split@example.com"])?;

        std::fs::create_dir_all(split_root.join(".config"))?;
        std::fs::write(
            split_root.join(".config/rail.toml"),
            r#"[workspace]
root = "."

[release]
tag_prefix = "v"
tag_format = "v{version}"
require_clean = false

[release.changelog]
path = "CHANGELOG.md"
"#,
        )?;

        // Tag current version
        git(split_root, &["tag", "-a", "v0.1.0", "-m", "Initial split tag"])?;

        // Make a change to release
        std::fs::write(split_root.join("src/lib.rs"), "// bumped")?;
        std::fs::create_dir_all(split_root.join(".changes"))?;
        std::fs::write(
            split_root.join(".changes/release.md"),
            "---\n\"lib-release\" = \"patch\"\n---\n\nPrepare the split crate release.\n",
        )?;
        git(split_root, &["add", "."])?;
        git(split_root, &["commit", "-m", "feat: prepare release"])?;

        // Run release publish in split repo (skip crates.io)
        let output = run_cargo_rail(
            split_root,
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
        assert!(output.status.success(), "Split release should succeed");

        // Verify tag and changelog
        let tags = git(split_root, &["tag", "--list"])?;
        let tag_list = String::from_utf8_lossy(&tags.stdout);
        assert!(
            tag_list.contains("v0.1.1"),
            "Release should create new tag v0.1.1. Tags:\n{}",
            tag_list
        );

        let changelog = std::fs::read_to_string(split_root.join("CHANGELOG.md"))?;
        assert!(
            changelog.contains("## [0.1.1]"),
            "Changelog should include new version header"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test split --remote override flag
#[test]
fn test_split_remote_override() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-remote-override")?;
        ws.add_crate("override-lib", "0.1.0", &[])?;

        // Create a custom target directory
        let custom_target = tempfile::TempDir::new()?;
        git(custom_target.path(), &["init", "--initial-branch=main"])?;
        git(custom_target.path(), &["config", "user.name", "Test"])?;
        git(custom_target.path(), &["config", "user.email", "test@test.com"])?;
        std::fs::write(custom_target.path().join("README.md"), "# Custom")?;
        git(custom_target.path(), &["add", "."])?;
        git(custom_target.path(), &["commit", "-m", "Initial"])?;

        // Configure split with default remote using new [crates.<name>.split] format
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            r#"[workspace]
root = "."

[crates.override-lib.split]
remote = "/tmp/default-remote"
branch = "main"
mode = "single"
"#,
        )?;

        ws.commit("Add override-lib with config")?;

        // Refuse an unrelated existing repository with no cargo-rail origin evidence.
        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "split",
                "run",
                "override-lib",
                "--check",
                "--remote",
                custom_target.path().to_str().unwrap(),
            ],
        )?;
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("existing split target has no cargo-rail origin evidence"),
            "unexpected diagnostic: {stderr}"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test split --json output
#[test]
fn test_split_json_output() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-json")?;
        ws.add_crate("json-lib", "0.1.0", &[])?;
        ws.commit("Add json-lib")?;

        // Configure split using new [crates.<name>.split] format
        let target_dir = tempfile::TempDir::new()?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            format!(
                r#"[workspace]
root = "."

[crates.json-lib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
                target_dir.path().display().to_string().replace('\\', "\\\\")
            ),
        )?;

        // Run split with --check and --json
        let output = run_cargo_rail(&ws.path, &["rail", "split", "run", "json-lib", "--check", "--json"])?;
        assert_eq!(
            output.status.code(),
            Some(1),
            "split run --check --format json should exit 1 when changes are pending"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|_| panic!("split --json should output valid JSON. stdout: {}", stdout));
        assert_eq!(json["schema_version"], serde_json::json!(1));
        assert_eq!(json["command"], serde_json::json!("split"));
        assert_eq!(json["mode"], serde_json::json!("check"));
        assert_eq!(json["result"], serde_json::json!("pending_changes"));
        assert_eq!(json["exit_code"], serde_json::json!(1));

        let applied = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "json-lib", "--yes", "--allow-dirty"],
        )?;
        assert!(applied.status.success());
        let clean = run_cargo_rail(&ws.path, &["rail", "split", "run", "json-lib", "--check", "--json"])?;
        assert_eq!(clean.status.code(), Some(0));
        let clean_json: serde_json::Value = serde_json::from_slice(&clean.stdout)?;
        assert_eq!(clean_json["result"], "clean");
        assert_eq!(clean_json["exit_code"], 0);
        assert_eq!(clean_json["crates"][0]["pending"], false);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_ownership_uses_its_captured_snapshot_and_cargo_graph() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-shared-snapshot")?;
        ws.add_crate("owned-dependency", "0.1.0", &[])?;
        ws.add_crate(
            "owned-root",
            "0.1.0",
            &[("owned-dependency", "{ path = \"../owned-dependency\" }")],
        )?;
        ws.commit("Add owned Cargo graph")?;
        let target = TempDir::new()?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                r#"[workspace]
root = "."

[release.version_groups]
owned = ["owned-root", "owned-dependency"]

[crates.owned-root.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
                target.path().display().to_string().replace('\\', "\\\\")
            ),
        )?;

        let split = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "split",
                "run",
                "owned-root",
                "--check",
                "--json",
                "--allow-dirty",
            ],
        )?;
        assert_eq!(split.status.code(), Some(1));
        let split: serde_json::Value = serde_json::from_slice(&split.stdout)?;
        let ownership = &split["planning"]["targets"][0]["ownership"];
        assert!(
            ownership["snapshot_id"]
                .as_str()
                .is_some_and(|identity| identity.starts_with("v1-sha256-"))
        );
        assert_eq!(
            split["mutation_plan"]["pre_apply"]["metadata_fingerprint"],
            format!("snapshot:{}", ownership["snapshot_id"].as_str().unwrap())
        );
        assert_eq!(ownership["members"], serde_json::json!(["owned-root"]));
        assert_eq!(ownership["dependency_closure"], serde_json::json!(["owned-dependency"]));
        assert_eq!(ownership["release_boundaries"][0]["name"], "owned");
        assert_eq!(
            ownership["release_boundaries"][0]["members"],
            serde_json::json!(["owned-dependency", "owned-root"])
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test split init command
#[test]
fn test_split_init_command() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-init-cmd")?;
        ws.add_crate("init-lib", "0.1.0", &[])?;
        ws.commit("Add init-lib")?;

        // Remove existing config to test init
        ws.remove_config()?;

        // Create minimal config without splits
        std::fs::create_dir_all(ws.path.join(".config"))?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            r#"[workspace]
root = "."
"#,
        )?;

        // Run split init with --dry-run.
        let output = run_cargo_rail(&ws.path, &["rail", "split", "init", "--dry-run"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            output.status.success(),
            "split init --dry-run should succeed. stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("init-lib") || stdout.contains("[crates."),
            "split init should show detected crates. Output:\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that split uses parallel prefetch for performance on repos with many commits.
/// This test creates more than 5 commits to trigger the parallel prefetch path.
#[test]
fn test_split_parallel_prefetch_many_commits() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-parallel-prefetch")?;

        // Create initial crate
        ws.add_crate("prefetch-lib", "0.1.0", &[])?;
        ws.commit("Add prefetch-lib")?;

        // Create more than 5 commits to trigger parallel prefetch (threshold is > 5)
        for i in 1..=8 {
            ws.modify_file("prefetch-lib", "src/lib.rs", &format!("// Version {}", i))?;
            ws.commit(&format!("Update prefetch-lib v{}", i))?;
        }

        // Configure split
        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.prefetch-lib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        // Run split - should use parallel prefetch
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "prefetch-lib", "--yes", "--allow-dirty"],
        )?;

        // Verify split succeeded
        assert!(
            output.status.success(),
            "Split with parallel prefetch should succeed. stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Check stderr mentions parallel prefetch (progress output goes to stderr)
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Prefetching exact trees and transform inputs in parallel"),
            "Should use parallel prefetch for 9 commits. stderr: {}",
            stderr
        );

        // Verify the split repo has all commits
        let log_output = git(split_dir.path(), &["log", "--oneline"])?;
        let log = String::from_utf8_lossy(&log_output.stdout);

        assert!(log.contains("Add prefetch-lib"), "Should have initial commit");
        assert!(log.contains("Update prefetch-lib v8"), "Should have last commit");

        // Verify final content
        let lib_content = std::fs::read_to_string(split_dir.path().join("src/lib.rs"))?;
        assert!(
            lib_content.contains("Version 8"),
            "Final content should be Version 8. Got: {}",
            lib_content
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that split handles "dirty history" gracefully - commits where the crate
/// was temporarily deleted or didn't exist at certain points in history.
///
/// This is a common scenario when:
/// - A crate is temporarily removed and later restored
/// - Files are moved/renamed in a way that deleted the old path temporarily
/// - The crate didn't exist at the start of the filtered history
#[test]
fn test_split_handles_dirty_history() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-dirty-history")?;

        // Create crate and commit
        ws.add_crate("dirty-lib", "0.1.0", &[])?;
        ws.commit("Add dirty-lib")?;

        // Make a change
        ws.modify_file("dirty-lib", "src/lib.rs", "// Version 1")?;
        ws.commit("Update dirty-lib v1")?;

        // DELETE the crate entirely (simulating dirty history)
        std::fs::remove_dir_all(ws.path.join("crates/dirty-lib"))?;
        ws.commit("Remove dirty-lib temporarily")?;

        // Recreate the crate (restoration)
        ws.add_crate("dirty-lib", "0.2.0", &[])?;
        ws.modify_file("dirty-lib", "src/lib.rs", "// Version 2 - restored")?;
        ws.commit("Restore dirty-lib")?;

        // Make another change after restoration
        ws.modify_file("dirty-lib", "src/lib.rs", "// Version 3 - final")?;
        ws.commit("Update dirty-lib v3")?;

        // Configure split
        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.dirty-lib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        // Run split - should succeed despite dirty history
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "dirty-lib", "--yes", "--allow-dirty"],
        )?;

        // Verify split succeeded
        assert!(
            output.status.success(),
            "Split should succeed with dirty history. stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Verify the split repo exists and has files
        assert!(
            split_dir.path().join("Cargo.toml").exists(),
            "Cargo.toml should exist in split repo"
        );
        assert!(
            split_dir.path().join("src/lib.rs").exists(),
            "src/lib.rs should exist in split repo"
        );

        // Verify the final content is correct
        let lib_content = std::fs::read_to_string(split_dir.path().join("src/lib.rs"))?;
        assert!(
            lib_content.contains("Version 3 - final"),
            "Final content should be Version 3. Got: {}",
            lib_content
        );

        // Verify git history in split repo has the commits that DID have files
        let log_output = git(split_dir.path(), &["log", "--oneline"])?;
        let log = String::from_utf8_lossy(&log_output.stdout);

        assert!(log.contains("Add dirty-lib"), "Should contain initial add commit");
        assert!(log.contains("Update dirty-lib v1"), "Should contain v1 update");
        assert!(
            log.contains("Remove dirty-lib"),
            "deletion must be preserved. Log:\n{}",
            log
        );
        assert!(log.contains("Restore dirty-lib"), "Should contain restore commit");
        assert!(log.contains("Update dirty-lib v3"), "Should contain v3 update");

        let deletion = git(
            split_dir.path(),
            &[
                "log",
                "--format=%H",
                "--fixed-strings",
                "--grep=Remove dirty-lib temporarily",
            ],
        )?;
        let deletion_sha = String::from_utf8_lossy(&deletion.stdout).trim().to_string();
        let deletion_tree = git(split_dir.path(), &["ls-tree", "-r", "--name-only", &deletion_sha])?;
        assert!(
            deletion_tree.stdout.is_empty(),
            "the historical deletion snapshot must contain no stale crate files"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_split_history_preserves_rename_delete_mode_symlink_and_exact_final_tree() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let ws = TestWorkspace::new_named("split-exact-tree-history")?;
        ws.add_crate("exact-tree", "0.1.0", &[])?;
        let crate_root = ws.path.join("crates/exact-tree");
        std::fs::write(crate_root.join("src/old.rs"), "pub const VALUE: u8 = 1;\n")?;
        std::fs::write(crate_root.join("tool.sh"), "#!/bin/sh\nexit 0\n")?;
        ws.commit("Add historical files")?;

        let tool = crate_root.join("tool.sh");
        let mut permissions = std::fs::metadata(&tool)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&tool, permissions)?;
        std::fs::write(crate_root.join("src/old.rs"), "pub const VALUE: u8 = 2;\n")?;
        ws.commit("Modify file and make tool executable")?;

        std::fs::rename(crate_root.join("src/old.rs"), crate_root.join("src/new.rs"))?;
        symlink("new.rs", crate_root.join("src/link.rs"))?;
        ws.commit("Rename file and add symlink")?;

        std::fs::remove_file(&tool)?;
        ws.commit("Delete executable tool")?;

        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.exact-tree.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "exact-tree", "--yes", "--allow-dirty"],
        )?;
        assert!(
            output.status.success(),
            "split stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let rename_commit = git(
            split_dir.path(),
            &[
                "log",
                "--format=%H",
                "--fixed-strings",
                "--grep=Rename file and add symlink",
            ],
        )?;
        let rename_commit = String::from_utf8_lossy(&rename_commit.stdout).trim().to_string();
        let rename_tree = git(split_dir.path(), &["ls-tree", "-r", &rename_commit])?;
        let rename_tree = String::from_utf8_lossy(&rename_tree.stdout);
        assert!(rename_tree.contains("120000 blob") && rename_tree.contains("src/link.rs"));
        assert!(rename_tree.contains("100755 blob") && rename_tree.contains("tool.sh"));
        assert!(rename_tree.contains("src/new.rs"));
        assert!(!rename_tree.contains("src/old.rs"));

        assert!(!split_dir.path().join("tool.sh").exists());
        assert!(!split_dir.path().join("src/old.rs").exists());
        assert_eq!(
            std::fs::read_link(split_dir.path().join("src/link.rs"))?,
            std::path::PathBuf::from("new.rs")
        );

        let source_tree = git(
            &ws.path,
            &["ls-tree", "-r", "--name-only", "HEAD", "--", "crates/exact-tree"],
        )?;
        let source_owned = String::from_utf8_lossy(&source_tree.stdout)
            .lines()
            .map(|path| path.trim_start_matches("crates/exact-tree/").to_string())
            .collect::<std::collections::BTreeSet<_>>();
        let target_tree = git(split_dir.path(), &["ls-tree", "-r", "--name-only", "HEAD"])?;
        let target_owned = String::from_utf8_lossy(&target_tree.stdout)
            .lines()
            .filter(|path| matches!(*path, "Cargo.toml" | "README.md") || path.starts_with("src/"))
            .map(str::to_string)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            target_owned, source_owned,
            "final owned tree must equal the source subtree"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_preserves_merge_parents_and_commit_identities() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-merge-history")?;
        ws.add_crate("merge-tree", "0.1.0", &[])?;
        ws.commit("Add merge-tree")?;
        git(&ws.path, &["checkout", "-b", "feature"])?;
        ws.modify_file("merge-tree", "src/feature.rs", "pub const FEATURE: bool = true;\n")?;
        ws.commit("Add feature side")?;
        git(&ws.path, &["checkout", "main"])?;
        ws.modify_file("merge-tree", "src/main_side.rs", "pub const MAIN: bool = true;\n")?;
        ws.commit("Add main side")?;
        git(&ws.path, &["merge", "--no-ff", "feature", "-m", "Merge split feature"])?;

        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.merge-tree.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;
        run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "merge-tree", "--yes", "--allow-dirty"],
        )?;

        let target_merge = git(
            split_dir.path(),
            &["log", "--format=%H", "--fixed-strings", "--grep=Merge split feature"],
        )?;
        let target_merge = String::from_utf8_lossy(&target_merge.stdout).trim().to_string();
        let parents = git(split_dir.path(), &["rev-list", "--parents", "-n", "1", &target_merge])?;
        assert_eq!(String::from_utf8_lossy(&parents.stdout).split_whitespace().count(), 3);

        let metadata_format = "%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI";
        let source_metadata = git(
            &ws.path,
            &["show", "-s", &format!("--format={metadata_format}"), "HEAD"],
        )?;
        let target_metadata = git(
            split_dir.path(),
            &["show", "-s", &format!("--format={metadata_format}"), &target_merge],
        )?;
        assert_eq!(source_metadata.stdout, target_metadata.stdout);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

// Safety Rails Tests

/// Test that split apply requires explicit confirmation in non-interactive mode
#[test]
fn test_split_requires_explicit_confirmation_non_interactive() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("confirm-lib", "0.1.0", &[])?;
        ws.commit("Add confirm-lib")?;

        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.confirm-lib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        let output = run_cargo_rail(&ws.path, &["rail", "split", "run", "confirm-lib", "--allow-dirty"])?;
        assert!(
            !output.status.success(),
            "split should fail without --yes/--plan in non-interactive mode"
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

/// Test that split fails on dirty worktree without --allow-dirty
#[test]
fn test_split_dirty_worktree_error() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("safety-lib", "0.1.0", &[])?;
        ws.commit("Add safety-lib")?;

        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.safety-lib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        // Make worktree dirty by adding an uncommitted file
        std::fs::write(ws.path.join("dirty.txt"), "uncommitted content")?;

        // Run split WITHOUT --allow-dirty - should fail
        let output = run_cargo_rail(&ws.path, &["rail", "split", "run", "safety-lib", "--yes"])?;

        assert!(
            !output.status.success(),
            "Split should fail on dirty worktree without --allow-dirty"
        );

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("uncommitted changes") || stderr.contains("dirty"),
            "Error should mention uncommitted changes. stderr: {}",
            stderr
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_dirty_check_excludes_exact_generated_roots_without_hiding_named_source_paths() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-generated-state-boundary")?;
        ws.add_crate("boundary-lib", "0.1.0", &[])?;

        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.boundary-lib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;
        std::fs::write(ws.path.join(".gitignore"), "ignored-state/\n")?;
        let status = std::process::Command::new(env!("CARGO"))
            .args(["generate-lockfile", "--offline"])
            .current_dir(&ws.path)
            .status()?;
        assert!(status.success(), "fixture lockfile generation failed");
        ws.commit("Configure exact generated-state boundary")?;

        let generated = ws.path.join("target/debug/generated.rlib");
        std::fs::create_dir_all(generated.parent().expect("generated fixture must have a parent"))?;
        std::fs::write(&generated, "Cargo output\n")?;
        let output = run_cargo_rail(&ws.path, &["rail", "split", "run", "boundary-lib", "--yes"])?;
        assert!(
            output.status.success(),
            "resolved Cargo output must not dirty split: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let source = ws.path.join("docs/target/intentional.txt");
        std::fs::create_dir_all(source.parent().expect("source fixture must have a parent"))?;
        std::fs::write(&source, "intentional source\n")?;
        let output = run_cargo_rail(&ws.path, &["rail", "split", "run", "boundary-lib", "--yes"])?;
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("docs/target/intentional.txt"),
            "named source path must remain inside the dirty boundary: {stderr}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that --allow-dirty bypasses the dirty worktree check
#[test]
fn test_split_allow_dirty_bypasses_check() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("allow-dirty-lib", "0.1.0", &[])?;
        ws.commit("Add allow-dirty-lib")?;

        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.allow-dirty-lib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        // Make worktree dirty
        std::fs::write(ws.path.join("dirty.txt"), "uncommitted content")?;

        // Run split WITH --allow-dirty - should succeed
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "allow-dirty-lib", "--yes", "--allow-dirty"],
        )?;

        assert!(
            output.status.success(),
            "Split should succeed with --allow-dirty. stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Verify split was created
        assert!(
            split_dir.path().join("Cargo.toml").exists(),
            "Split repo should be created"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_rejects_target_inside_source_repository() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-target-boundary")?;
        ws.add_crate("boundary-lib", "0.1.0", &[])?;
        let source_head = ws.commit("Add boundary-lib")?;
        let target = ws.path.join("split-target");
        let config = format!(
            r#"[workspace]
root = "."

[crates.boundary-lib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            target.display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "boundary-lib", "--yes", "--allow-dirty"],
        )?;
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("split target") && stderr.contains("overlaps") && stderr.contains("source worktree"),
            "boundary error should identify both roots\nstderr:\n{}",
            stderr
        );
        assert!(!target.exists(), "rejected target must not be created");
        let head = git(&ws.path, &["rev-parse", "HEAD"])?;
        assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), source_head);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_split_rejects_unknown_member_before_target_mutation() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("split-crate-boundary")?;
        ws.add_crate("boundary-lib", "0.1.0", &[])?;
        ws.commit("Add boundary-lib")?;
        let target_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.boundary-lib.split]
remote = "{}"
branch = "main"
mode = "single"
members = ["outside"]
"#,
            target_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "boundary-lib", "--yes", "--allow-dirty"],
        )?;
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Crate 'outside' not found") && !target_dir.path().join(".git").exists(),
            "member resolution should fail before target mutation\nstderr:\n{}",
            stderr
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that split is idempotent - running twice produces the same result (no-op on second run)
#[test]
fn test_split_idempotent_run_twice() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("idempotent-lib", "0.1.0", &[])?;
        ws.commit("Initial commit")?;

        // Make a few commits to have history
        ws.modify_file("idempotent-lib", "src/lib.rs", "// Version 1")?;
        ws.commit("Update v1")?;

        ws.modify_file("idempotent-lib", "src/lib.rs", "// Version 2")?;
        ws.commit("Update v2")?;

        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.idempotent-lib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        // First split
        let output1 = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "idempotent-lib", "--yes", "--allow-dirty"],
        )?;
        assert!(
            output1.status.success(),
            "First split should succeed. stderr: {}",
            String::from_utf8_lossy(&output1.stderr)
        );

        // Get commit count after first split
        let log1 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
        let commit_count1: usize = String::from_utf8_lossy(&log1.stdout).trim().parse()?;

        // Get HEAD SHA after first split
        let head1 = git(split_dir.path(), &["rev-parse", "HEAD"])?;
        let head_sha1 = String::from_utf8_lossy(&head1.stdout).trim().to_string();

        // Second split (should be no-op)
        let output2 = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "idempotent-lib", "--yes", "--allow-dirty"],
        )?;
        assert!(
            output2.status.success(),
            "Second split should succeed. stderr: {}",
            String::from_utf8_lossy(&output2.stderr)
        );

        // Verify "already up-to-date" message (progress output goes to stderr)
        let stderr2 = String::from_utf8_lossy(&output2.stderr);
        assert!(
            stderr2.contains("already up-to-date") || stderr2.contains("already split"),
            "Second run should indicate already up-to-date. stderr: {}",
            stderr2
        );

        // Get commit count after second split
        let log2 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
        let commit_count2: usize = String::from_utf8_lossy(&log2.stdout).trim().parse()?;

        // Get HEAD SHA after second split
        let head2 = git(split_dir.path(), &["rev-parse", "HEAD"])?;
        let head_sha2 = String::from_utf8_lossy(&head2.stdout).trim().to_string();

        // Verify no new commits were created
        assert_eq!(
            commit_count1, commit_count2,
            "Commit count should not change on second split"
        );
        assert_eq!(head_sha1, head_sha2, "HEAD should not change on second split");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that split is incremental - new commits are added without duplicating existing ones
#[test]
fn test_split_incremental_new_commits() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("incremental-lib", "0.1.0", &[])?;
        ws.commit("Initial commit")?;

        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.incremental-lib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        // First split
        run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "incremental-lib", "--yes", "--allow-dirty"],
        )?;

        // Get commit count after first split
        let log1 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
        let commit_count1: usize = String::from_utf8_lossy(&log1.stdout).trim().parse()?;

        // Add new commits to monorepo
        ws.modify_file("incremental-lib", "src/lib.rs", "// New feature 1")?;
        ws.commit("Add feature 1")?;

        ws.modify_file("incremental-lib", "src/lib.rs", "// New feature 2")?;
        ws.commit("Add feature 2")?;

        // Second split (should add only new commits)
        let output2 = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "incremental-lib", "--yes", "--allow-dirty"],
        )?;
        assert!(
            output2.status.success(),
            "Incremental split should succeed. stderr: {}",
            String::from_utf8_lossy(&output2.stderr)
        );

        // Get commit count after second split
        let log2 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
        let commit_count2: usize = String::from_utf8_lossy(&log2.stdout).trim().parse()?;

        // Should have exactly 2 more commits (for the 2 new features)
        assert_eq!(
            commit_count2,
            commit_count1 + 2,
            "Should have exactly 2 new commits. Before: {}, After: {}",
            commit_count1,
            commit_count2
        );

        // Verify the new commits are there
        let log_output = git(split_dir.path(), &["log", "--oneline"])?;
        let log = String::from_utf8_lossy(&log_output.stdout);
        assert!(log.contains("Add feature 1"), "Should contain feature 1 commit");
        assert!(log.contains("Add feature 2"), "Should contain feature 2 commit");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that split combined mode is idempotent
#[test]
fn test_split_combined_mode_idempotent() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("lib-core", "0.1.0", &[])?;
        ws.add_crate("service-api", "0.1.0", &[("lib-core", r#"{ path = "../lib-core" }"#)])?;
        ws.commit("Initial combined crates")?;

        // Make commits to both crates
        ws.modify_file("lib-core", "src/lib.rs", "// Core v1")?;
        ws.commit("Update lib-core")?;

        ws.modify_file("service-api", "src/lib.rs", "// API v1")?;
        ws.commit("Update service-api")?;

        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.combined.split]
remote = "{}"
branch = "main"
mode = "combined"
members = ["lib-core", "service-api"]
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        // First split
        let output1 = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "combined", "--yes", "--allow-dirty"],
        )?;
        assert!(
            output1.status.success(),
            "First combined split should succeed. stderr: {}",
            String::from_utf8_lossy(&output1.stderr)
        );

        // Get state after first split
        let log1 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
        let commit_count1: usize = String::from_utf8_lossy(&log1.stdout).trim().parse()?;
        let head1 = git(split_dir.path(), &["rev-parse", "HEAD"])?;
        let head_sha1 = String::from_utf8_lossy(&head1.stdout).trim().to_string();

        // Second split (should be no-op)
        let output2 = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "combined", "--yes", "--allow-dirty"],
        )?;
        assert!(
            output2.status.success(),
            "Second combined split should succeed. stderr: {}",
            String::from_utf8_lossy(&output2.stderr)
        );

        // Verify "already up-to-date" message (progress output goes to stderr)
        let stderr2 = String::from_utf8_lossy(&output2.stderr);
        assert!(
            stderr2.contains("already up-to-date") || stderr2.contains("already split"),
            "Second run should indicate already up-to-date. stderr: {}",
            stderr2
        );

        // Verify no changes
        let log2 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
        let commit_count2: usize = String::from_utf8_lossy(&log2.stdout).trim().parse()?;
        let head2 = git(split_dir.path(), &["rev-parse", "HEAD"])?;
        let head_sha2 = String::from_utf8_lossy(&head2.stdout).trim().to_string();

        assert_eq!(commit_count1, commit_count2, "Commit count should not change");
        assert_eq!(head_sha1, head_sha2, "HEAD should not change");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that split recovers gracefully from partial/interrupted state
#[test]
fn test_split_partial_state_recovery() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("partial-lib", "0.1.0", &[])?;
        ws.commit("Initial commit")?;

        ws.modify_file("partial-lib", "src/lib.rs", "// V1")?;
        ws.commit("Version 1")?;

        ws.modify_file("partial-lib", "src/lib.rs", "// V2")?;
        ws.commit("Version 2")?;

        ws.modify_file("partial-lib", "src/lib.rs", "// V3")?;
        ws.commit("Version 3")?;

        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.partial-lib.split]
remote = "{}"
branch = "main"
mode = "single"
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        // First split - creates full history
        run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "partial-lib", "--yes", "--allow-dirty"],
        )?;

        // Simulate partial state by manually removing mappings for later commits
        // We'll delete the git-notes to simulate an interrupted split
        let notes_ref = "refs/notes/rail/partial-lib".to_string();

        // Get commit count before manipulation
        let log_before = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
        let count_before: usize = String::from_utf8_lossy(&log_before.stdout).trim().parse()?;

        // Delete notes in both repos to simulate interrupted state
        let ws_notes_ref = git(&ws.path, &["for-each-ref", "--format=%(refname)", &notes_ref])?;
        if !String::from_utf8_lossy(&ws_notes_ref.stdout).trim().is_empty() {
            git(&ws.path, &["update-ref", "-d", &notes_ref])?;
        }
        let split_notes_ref = git(split_dir.path(), &["for-each-ref", "--format=%(refname)", &notes_ref])?;
        if !String::from_utf8_lossy(&split_notes_ref.stdout).trim().is_empty() {
            git(split_dir.path(), &["update-ref", "-d", &notes_ref])?;
        }

        // Now add a new commit
        ws.modify_file("partial-lib", "src/lib.rs", "// V4 after interruption")?;
        ws.commit("Version 4")?;

        // Re-run split - should handle the missing mappings gracefully
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "split", "run", "partial-lib", "--yes", "--allow-dirty"],
        )?;
        assert!(
            output.status.success(),
            "Split after partial state should succeed. stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Verify split repo has commits (exact count depends on implementation)
        let log_after = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
        let count_after: usize = String::from_utf8_lossy(&log_after.stdout).trim().parse()?;

        // Should have at least as many commits as before (recovery may add more or same)
        assert!(
            count_after >= count_before,
            "Should have at least {} commits after recovery, got {}",
            count_before,
            count_after
        );

        // Verify the new content is there
        let content = std::fs::read_to_string(split_dir.path().join("src/lib.rs"))?;
        assert!(
            content.contains("V4 after interruption"),
            "Should have the latest content"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that split with auxiliary files is idempotent (final commit doesn't duplicate)
#[test]
fn test_split_auxiliary_files_idempotent() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("aux-lib", "0.1.0", &[])?;
        ws.commit("Initial commit")?;

        // Add some auxiliary files at workspace root
        std::fs::write(ws.path.join("rustfmt.toml"), "max_width = 100")?;
        std::fs::write(ws.path.join(".editorconfig"), "root = true")?;
        git(&ws.path, &["add", "."])?;
        git(&ws.path, &["commit", "-m", "Add config files"])?;

        let split_dir = TempDir::new()?;
        let config = format!(
            r#"[workspace]
root = "."

[crates.aux-lib.split]
remote = "{}"
branch = "main"
mode = "single"
include = ["rustfmt.toml", ".editorconfig"]
"#,
            split_dir.path().display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(ws.path.join("rail.toml"), config)?;

        // First split
        run_cargo_rail(&ws.path, &["rail", "split", "run", "aux-lib", "--yes", "--allow-dirty"])?;

        // Count commits including auxiliary files commit
        let log1 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
        let commit_count1: usize = String::from_utf8_lossy(&log1.stdout).trim().parse()?;

        // Second split - auxiliary files commit should not be duplicated
        let output2 = run_cargo_rail(&ws.path, &["rail", "split", "run", "aux-lib", "--yes", "--allow-dirty"])?;
        assert!(output2.status.success());

        // Progress output goes to stderr
        let stderr2 = String::from_utf8_lossy(&output2.stderr);
        assert!(
            stderr2.contains("already up-to-date") || stderr2.contains("already split"),
            "Should be up-to-date. stderr: {}",
            stderr2
        );

        // Count should be the same
        let log2 = git(split_dir.path(), &["rev-list", "--count", "HEAD"])?;
        let commit_count2: usize = String::from_utf8_lossy(&log2.stdout).trim().parse()?;

        assert_eq!(
            commit_count1, commit_count2,
            "Auxiliary files commit should not be duplicated"
        );

        // Verify auxiliary files exist
        assert!(split_dir.path().join("rustfmt.toml").exists());
        assert!(split_dir.path().join(".editorconfig").exists());

        Ok(())
    })();
    super::helpers::finish_test(result);
}
