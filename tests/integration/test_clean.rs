use anyhow::Result;
use cargo_rail::backup::{BackupManager, BackupMetadata};
use cargo_rail::commands::clean::{CleanContext, CleanOptions};
use cargo_rail::commands::{TextJsonOutputFormat, run_clean};
use std::fs;
use tempfile::TempDir;

fn create_test_workspace() -> Result<TempDir> {
    let temp = TempDir::new()?;
    let workspace = temp.path();

    // Create Cargo.toml
    fs::write(
        workspace.join("Cargo.toml"),
        r#"
    [workspace]
    members = ["crates/*"]
    "#,
    )?;

    // Create dummy crate
    fs::create_dir_all(workspace.join("crates/foo"))?;
    fs::write(
        workspace.join("crates/foo/Cargo.toml"),
        r#"
    [package]
    name = "foo"
    version = "0.1.0"
    edition = "2021"
    "#,
    )?;
    // Create dummy source file
    fs::create_dir_all(workspace.join("crates/foo/src"))?;
    fs::write(workspace.join("crates/foo/src/lib.rs"), "")?;

    // Create target directories
    fs::create_dir_all(workspace.join("target/cargo-rail"))?;
    fs::write(workspace.join("target/cargo-rail/report.md"), "# Report")?;
    fs::create_dir_all(workspace.join("target/cargo-rail/compiler-artifacts-v1"))?;
    fs::write(
        workspace.join("target/cargo-rail/compiler-artifacts-v1/result"),
        "compiled output",
    )?;
    // Initialize git repo
    super::helpers::git(workspace, &["init", "--initial-branch=main"])?;

    // Configure git user for commits (needed for some git ops, though maybe not strict requirement for clean)
    super::helpers::git(workspace, &["config", "user.email", "you@example.com"])?;
    super::helpers::git(workspace, &["config", "user.name", "Your Name"])?;

    // Create deterministic backup fixtures (avoid time-based sleeps).
    let backup_root = workspace.join("target/cargo-rail/backups");
    fs::create_dir_all(&backup_root)?;
    for i in 1..=5 {
        let backup_dir = backup_root.join(format!("2024-01-01-00000{}-000", i));
        fs::create_dir_all(&backup_dir)?;
        let metadata = BackupMetadata {
            timestamp: format!("2024-01-01T00:00:0{}+00:00", i),
            command: format!("test {}", i),
            files_modified: vec![],
            description: None,
        };
        metadata.save(&backup_dir)?;
    }

    Ok(temp)
}

fn run_cache_command(workspace: &TempDir, cache: &TempDir, args: &[&str]) -> std::process::Output {
    let mut command =
        super::helpers::isolated_cargo_rail_command(workspace.path()).expect("test command should be isolated");
    command
        .env("CARGO_RAIL_CACHE_DIR", cache.path())
        .args(args)
        .output()
        .expect("cache command should run")
}

fn create_empty_local_cas(cache: &TempDir) -> Result<std::path::PathBuf> {
    const TRUST_DOMAIN: &str = "0000000000000000000000000000000000000000000000000000000000000000";
    let owner = cache.path().join("cargo-rail");
    let root = owner.join("local-cas-v3");
    fs::create_dir_all(&root)?;
    fs::write(owner.join("LOCAL_TRUST_DOMAIN"), format!("{TRUST_DOMAIN}\n"))?;
    fs::write(
        root.join("OWNER"),
        format!("cargo-rail-local-cas\nschema=3\ntrust-domain={TRUST_DOMAIN}\n"),
    )?;
    fs::write(
        root.join("CAPACITY.json"),
        b"{\"version\":3,\"result_bytes\":0,\"metadata_bytes\":0}",
    )?;
    fs::write(
        root.join("NATIVE_LEDGER.json"),
        b"{\"version\":2,\"terminal_states\":0,\"terminal_bytes\":0,\"disabled\":false}",
    )?;
    for directory in [
        "blobs",
        "results",
        "pins",
        "leases",
        "staging",
        "native-actions-v2",
        "native-dynamic-input-selectors-v1",
        "native-link-candidates-v1",
        "native-restore-locks-v1",
        "compiler-evidence-candidates",
    ] {
        fs::create_dir(root.join(directory))?;
    }
    for shard in 0..64_u8 {
        fs::write(
            root.join("native-restore-locks-v1").join(format!("{shard:02x}.lock")),
            b"",
        )?;
    }
    #[cfg(target_os = "macos")]
    fs::create_dir(root.join("sysroot-identities"))?;
    fs::write(owner.join("local-cas-v3.lock"), b"")?;
    Ok(root)
}

#[test]
fn local_status_and_cleanup_use_the_configured_domain_without_workspace_state() {
    let result: Result<()> = (|| {
        let workspace = create_test_workspace()?;
        let cache = TempDir::new().unwrap();
        let root = create_empty_local_cas(&cache)?;
        let status = run_cache_command(
            &workspace,
            &cache,
            &["rail", "cache", "status", "--scope", "local", "-f", "json"],
        );
        assert!(status.status.success(), "local status failed: {status:?}");
        let status_json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(status_json["status"]["local"]["present"], true);
        assert_eq!(
            status_json["status"]["local"]["cache"]["root"],
            fs::canonicalize(&root).unwrap().display().to_string()
        );

        let preview = run_cache_command(
            &workspace,
            &cache,
            &["rail", "cache", "clean", "--scope", "local", "--check", "-f", "json"],
        );
        assert_eq!(preview.status.code(), Some(1), "local preview result: {preview:?}");
        let preview_json: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
        assert!(root.is_dir());

        let cleaned = run_cache_command(
            &workspace,
            &cache,
            &["rail", "cache", "clean", "--scope", "local", "-f", "json"],
        );
        assert!(cleaned.status.success(), "local cleanup failed: {cleaned:?}");
        let cleaned_json: serde_json::Value = serde_json::from_slice(&cleaned.stdout).unwrap();
        assert_eq!(
            cleaned_json["reclaimed_bytes"], preview_json["would_reclaim_bytes"],
            "an unchanged cache must report the bytes actually removed"
        );
        assert!(!root.exists());
        let lock = cache.path().join("cargo-rail/local-cas-v3.lock");
        assert!(lock.is_file(), "the lifecycle authority must survive root reclamation");
        assert_eq!(fs::metadata(lock).unwrap().len(), 0);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn local_cleanup_previews_and_removes_only_the_owned_retired_store() {
    let result: Result<()> = (|| {
        const TRUST_DOMAIN: &str = "0000000000000000000000000000000000000000000000000000000000000000";
        let workspace = create_test_workspace()?;
        let cache = TempDir::new().unwrap();
        let root = create_empty_local_cas(&cache)?;
        let owner = cache.path().join("cargo-rail");
        let retired = owner.join("local-cas-v2");
        fs::create_dir_all(retired.join("results/old-result"))?;
        fs::write(
            retired.join("OWNER"),
            format!("cargo-rail-local-cas\nschema=2\ntrust-domain={TRUST_DOMAIN}\n"),
        )?;
        fs::write(retired.join("results/old-result/blob"), vec![0_u8; 4096])?;
        // Another directory with a retired-looking name but no provable ownership is never touched.
        let decoy = owner.join("local-cas-v2-not-a-trust-domain");
        fs::create_dir_all(&decoy)?;
        fs::write(decoy.join("data"), b"not cargo-rail state")?;

        let preview = run_cache_command(
            &workspace,
            &cache,
            &["rail", "cache", "clean", "--scope", "local", "--check", "-f", "json"],
        );
        assert_eq!(preview.status.code(), Some(1), "retired preview: {preview:?}");
        let preview_json: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
        let current_bytes = preview_json["status"]["local"]["cache"]["bytes"].as_u64().unwrap();
        let would_reclaim = preview_json["would_reclaim_bytes"].as_u64().unwrap();
        assert!(
            would_reclaim >= current_bytes + 4096,
            "the preview includes the retired store: {preview_json}"
        );
        assert!(retired.is_dir(), "a preview removes nothing");

        let cleaned = run_cache_command(
            &workspace,
            &cache,
            &["rail", "cache", "clean", "--scope", "local", "-f", "json"],
        );
        assert!(cleaned.status.success(), "local cleanup failed: {cleaned:?}");
        let cleaned_json: serde_json::Value = serde_json::from_slice(&cleaned.stdout).unwrap();
        assert_eq!(cleaned_json["reclaimed_bytes"], preview_json["would_reclaim_bytes"]);
        let expected = fs::canonicalize(&owner)?.join("local-cas-v2").display().to_string();
        assert!(
            cleaned_json["removed"]
                .as_array()
                .unwrap()
                .iter()
                .any(|path| path.as_str() == Some(expected.as_str())),
            "{cleaned_json}"
        );
        assert!(!root.exists() && !retired.exists());
        assert_eq!(fs::read(decoy.join("data"))?, b"not cargo-rail state");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn workspace_cleanup_never_removes_the_shared_local_cas() {
    let result: Result<()> = (|| {
        for arguments in [&["rail", "clean", "--all"][..], &["rail", "clean", "--cache"][..]] {
            let workspace = create_test_workspace()?;
            let cache = TempDir::new().unwrap();
            let root = create_empty_local_cas(&cache)?;
            let lock = cache.path().join("cargo-rail/local-cas-v3.lock");
            let shared_result = root.join("results/shared-result");
            fs::write(&shared_result, b"shared across workspaces")?;

            let cleaned = run_cache_command(&workspace, &cache, arguments);

            assert!(cleaned.status.success(), "workspace cleanup failed: {cleaned:?}");
            assert_eq!(fs::read(&shared_result)?, b"shared across workspaces");
            assert!(lock.is_file());
            assert_eq!(fs::metadata(lock).unwrap().len(), 0);
            assert!(
                !workspace
                    .path()
                    .join("target/cargo-rail/compiler-artifacts-v1")
                    .exists()
            );
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn cache_status_and_cleanup_keep_workspace_and_shared_scopes_explicit() {
    let result: Result<()> = (|| {
        let workspace = create_test_workspace()?;
        let cache = TempDir::new().unwrap();
        let status = run_cache_command(
            &workspace,
            &cache,
            &["rail", "cache", "status", "--scope", "workspace", "-f", "json"],
        );
        assert!(status.status.success(), "status failed: {status:?}");
        let status_json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(status_json["command"], "cache");
        assert_eq!(status_json["mode"], "status");
        assert_eq!(status_json["scope"], "workspace");
        assert!(status_json["status"]["local"].is_null());
        assert!(status_json["status"]["workspace"]["bytes"].as_u64().unwrap() > 0);
        assert_eq!(status_json["status"]["workspace"]["fully_bounded"], false);

        let missing_scope = run_cache_command(&workspace, &cache, &["rail", "cache", "clean"]);
        assert_eq!(missing_scope.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&missing_scope.stderr).contains("--scope"),
            "missing-scope diagnostic: {missing_scope:?}"
        );

        let preview = run_cache_command(
            &workspace,
            &cache,
            &[
                "rail",
                "cache",
                "clean",
                "--scope",
                "workspace",
                "--check",
                "-f",
                "json",
            ],
        );
        assert_eq!(preview.status.code(), Some(1), "preview result: {preview:?}");
        let preview_json: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
        assert_eq!(preview_json["mode"], "clean_check");
        assert!(preview_json["would_reclaim_bytes"].as_u64().unwrap() > 0);
        assert!(
            workspace
                .path()
                .join("target/cargo-rail/compiler-artifacts-v1")
                .exists()
        );

        let cleaned = run_cache_command(
            &workspace,
            &cache,
            &["rail", "cache", "clean", "--scope", "workspace", "-f", "json"],
        );
        assert!(cleaned.status.success(), "cleanup failed: {cleaned:?}");
        assert!(
            !workspace
                .path()
                .join("target/cargo-rail/compiler-artifacts-v1")
                .exists()
        );
        assert!(workspace.path().join("target/cargo-rail/report.md").exists());
        assert!(workspace.path().join("target/cargo-rail/backups").exists());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_clean_all() {
    let result: Result<()> = (|| {
        let temp = create_test_workspace()?;
        let cache = TempDir::new().unwrap();

        // Verify artifacts exist
        assert!(
            temp.path()
                .join("target/cargo-rail/compiler-artifacts-v1/result")
                .exists()
        );
        assert!(temp.path().join("target/cargo-rail/report.md").exists());
        let manager = BackupManager::new(temp.path());
        assert_eq!(manager.list_backups().unwrap().len(), 5);

        // Every artifact class requires explicit all-authority.
        let output = run_cache_command(&temp, &cache, &["rail", "clean", "--all"]);
        assert!(output.status.success(), "clean all failed: {output:?}");

        // Verify artifacts removed
        assert!(!temp.path().join("target/cargo-rail/compiler-artifacts-v1").exists());
        assert!(!temp.path().join("target/cargo-rail/report.md").exists());
        assert_eq!(manager.list_backups().unwrap().len(), 0);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_clean_cache_only() {
    let result: Result<()> = (|| {
        let temp = create_test_workspace()?;
        let cache = TempDir::new().unwrap();

        // Run clean --cache
        let output = run_cache_command(&temp, &cache, &["rail", "clean", "--cache"]);
        assert!(output.status.success(), "cache cleanup failed: {output:?}");

        // Verify cache removed, others remain
        assert!(!temp.path().join("target/cargo-rail/compiler-artifacts-v1").exists());
        assert!(temp.path().join("target/cargo-rail/report.md").exists());
        let manager = BackupManager::new(temp.path());
        assert_eq!(manager.list_backups().unwrap().len(), 5);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_clean_refuses_to_follow_a_cache_state_symlink() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::symlink;

        let temp = create_test_workspace()?;
        let cache = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("keep"), "outside state\n").unwrap();
        let cache_state = temp.path().join("target/cargo-rail/compiler-artifacts-v1");
        fs::remove_dir_all(&cache_state).unwrap();
        symlink(outside.path(), &cache_state).unwrap();

        let output = run_cache_command(&temp, &cache, &["rail", "clean", "--cache"]);
        assert_eq!(output.status.code(), Some(2), "cache cleanup must fail: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("cache status refused linked path"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read_to_string(outside.path().join("keep")).unwrap(),
            "outside state\n"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(target_os = "linux")]
#[test]
fn user_workspace_storage_cleans_cache_and_preserves_receipts() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::{DirBuilderExt as _, symlink};

        let workspace = TempDir::new()?;
        let managed = TempDir::new()?;
        let storage = TempDir::new()?;
        let cache = storage.path().join("cache");
        let state = storage.path().join("state");
        let cargo_home = storage.path().join("cargo");
        fs::create_dir(&cargo_home)?;
        let target = workspace.path().join("target");
        symlink(managed.path(), &target)?;
        fs::write(managed.path().join("keep"), "mbx artifact")?;
        let run = |args: &[&str]| -> Result<std::process::Output> {
            Ok(super::helpers::cargo_rail_command(workspace.path())?
                .env("CARGO_HOME", &cargo_home)
                .env("CARGO_RAIL_WORKSPACE_STORAGE", "user")
                .env("XDG_CACHE_HOME", &cache)
                .env("XDG_STATE_HOME", &state)
                .args(args)
                .output()?)
        };

        let first = run(&["rail", "cache", "status", "--scope", "workspace", "-f", "json"])?;
        assert!(first.status.success(), "{first:?}");
        let status: serde_json::Value = serde_json::from_slice(&first.stdout)?;
        let store = std::path::PathBuf::from(
            status["status"]["workspace"]["root"]
                .as_str()
                .expect("workspace cache root"),
        );
        let namespace = store.file_name().expect("checkout identifier");
        let receipts = state.join("cargo-rail/workspaces").join(namespace).join("receipts");
        let artifact = store.join("compiler-artifacts-v1/result");
        let mut directories = fs::DirBuilder::new();
        directories.recursive(true).mode(0o700);
        directories.create(artifact.parent().unwrap())?;
        fs::write(&artifact, "disposable")?;
        directories.create(&receipts)?;
        fs::write(receipts.join("receipt.json"), "recoverable")?;

        let preview = run(&["rail", "clean", "--cache", "--check"])?;
        assert_eq!(preview.status.code(), Some(1), "{preview:?}");
        assert!(artifact.is_file());
        let cleaned = run(&["rail", "clean", "--cache"])?;
        assert!(cleaned.status.success(), "{cleaned:?}");
        assert!(!artifact.exists());
        assert!(receipts.join("receipt.json").is_file());
        assert!(target.is_symlink());
        assert_eq!(fs::read_to_string(managed.path().join("keep"))?, "mbx artifact");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(target_os = "linux")]
#[test]
fn user_adopt_copies_legacy_state_without_overwriting_it() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let workspace = TempDir::new()?;
        let managed = TempDir::new()?;
        let storage = TempDir::new()?;
        let cache = storage.path().join("cache");
        let state = storage.path().join("state");
        let legacy = managed.path().join("cargo-rail");
        symlink(managed.path(), workspace.path().join("target"))?;
        for directory in [
            "receipts",
            "backups/snapshot",
            "compiler-artifacts-v1",
            "surface-acquisitions-v3",
        ] {
            fs::create_dir_all(legacy.join(directory))?;
        }
        let backup = legacy.join("backups/snapshot/script");
        fs::write(&backup, "#!/bin/sh\n")?;
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o755))?;
        let mut metadata = BackupMetadata::new("test adoption");
        metadata.add_file("script");
        metadata.save(&legacy.join("backups/snapshot"))?;
        fs::create_dir(legacy.join("backups/unfinished"))?;
        fs::write(legacy.join("backups/unfinished/partial-file"), "in-progress backup")?;
        fs::write(legacy.join("receipts/receipt.json"), "receipt")?;
        fs::write(legacy.join("receipts/.writing.tmp"), "partial")?;
        fs::write(
            legacy.join("compiler-artifacts-v1/cache-readiness-v1.json"),
            "readiness",
        )?;
        fs::write(legacy.join("compiler-artifacts-v1/disposable"), "build output")?;
        fs::write(legacy.join("surface-acquisitions-v3/journal.json"), "journal")?;
        fs::write(legacy.join("generated-lockfile-v1"), "lockfile provenance")?;
        fs::write(legacy.join("unify-report.md"), "report")?;
        let run = |args: &[&str], layout: &str| -> Result<std::process::Output> {
            Ok(super::helpers::cargo_rail_command(workspace.path())?
                .env("CARGO_RAIL_WORKSPACE_STORAGE", layout)
                .env("XDG_CACHE_HOME", &cache)
                .env("XDG_STATE_HOME", &state)
                .args(["rail", "cache", "adopt", "--json"])
                .args(args)
                .output()?)
        };

        let missing_opt_in = run(&[], "")?;
        assert_eq!(missing_opt_in.status.code(), Some(2), "{missing_opt_in:?}");
        let preview = run(&["--check"], "user")?;
        assert_eq!(preview.status.code(), Some(1), "{preview:?}");
        assert!(!cache.exists() && !state.exists());
        assert!(!workspace.path().join(".cargo-rail").exists());
        let planned: serde_json::Value = serde_json::from_slice(&preview.stdout)?;
        let files = planned["files"].as_array().unwrap();
        assert_eq!(files.len(), 7, "{planned}");

        let adopted = run(&[], "user")?;
        assert!(adopted.status.success(), "{adopted:?}");
        let actual: serde_json::Value = serde_json::from_slice(&adopted.stdout)?;
        assert_eq!(actual["files"], planned["files"]);
        for path in files {
            assert!(std::path::Path::new(path.as_str().unwrap()).is_file(), "{path}");
        }
        let copied_backup = files
            .iter()
            .filter_map(|path| path.as_str())
            .find(|path| path.ends_with("/script"))
            .unwrap();
        assert_eq!(fs::metadata(copied_backup)?.permissions().mode() & 0o777, 0o755);
        assert!(backup.is_file());
        assert!(legacy.join("compiler-artifacts-v1/disposable").is_file());
        let repeated = run(&["--from", legacy.to_str().unwrap()], "user")?;
        assert!(repeated.status.success(), "{repeated:?}");
        let repeated: serde_json::Value = serde_json::from_slice(&repeated.stdout)?;
        assert_eq!(repeated["files"], serde_json::json!([]));

        let receipt = files
            .iter()
            .filter_map(|path| path.as_str())
            .find(|path| path.ends_with("/receipt.json"))
            .unwrap();
        fs::write(receipt, "newer user-storage receipt")?;
        fs::write(legacy.join("receipts/another.json"), "another receipt")?;
        let rejected = run(&[], "user")?;
        assert_eq!(rejected.status.code(), Some(2), "{rejected:?}");
        assert_eq!(fs::read_to_string(receipt)?, "newer user-storage receipt");
        assert!(!std::path::Path::new(receipt).with_file_name("another.json").exists());
        fs::remove_file(legacy.join("receipts/receipt.json"))?;
        symlink(receipt, legacy.join("receipts/receipt.json"))?;
        let linked = run(&[], "user")?;
        assert_eq!(linked.status.code(), Some(2), "{linked:?}");
        assert!(
            String::from_utf8_lossy(&linked.stdout).contains("must be a real file")
                || String::from_utf8_lossy(&linked.stderr).contains("must be a real file"),
            "{linked:?}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn user_storage_uses_native_cache_and_state_directories() {
    let result: Result<()> = (|| {
        let workspace = TempDir::new()?;
        let receipts = workspace.path().join("target/cargo-rail/receipts");
        fs::create_dir_all(&receipts)?;
        fs::write(receipts.join("receipt.json"), "{}")?;
        let run = |args: &[&str]| -> Result<std::process::Output> {
            Ok(super::helpers::isolated_cargo_rail_command(workspace.path())?
                .env("CARGO_RAIL_WORKSPACE_STORAGE", "user")
                .env("XDG_CACHE_HOME", workspace.path().join("unused-cache"))
                .env("XDG_STATE_HOME", workspace.path().join("unused-state"))
                .args(args)
                .output()?)
        };
        let status = run(&["rail", "cache", "status", "--scope", "workspace", "--json"])?;
        assert!(status.status.success(), "{status:?}");
        let status: serde_json::Value = serde_json::from_slice(&status.stdout)?;
        let cache = std::path::Path::new(status["status"]["workspace"]["root"].as_str().unwrap());
        let mut cache_base = dirs::cache_dir().expect("native cache directory").join("cargo-rail");
        if cfg!(windows) {
            cache_base.push("cache");
        }
        let cache_base = cargo_rail::utils::canonicalize_allow_missing(&cache_base)?.join("workspaces");
        assert_eq!(cache.parent(), Some(cache_base.as_path()));

        let preview = run(&["rail", "cache", "adopt", "--check", "--json"])?;
        assert_eq!(preview.status.code(), Some(1), "{preview:?}");
        let preview: serde_json::Value = serde_json::from_slice(&preview.stdout)?;
        let state_base = dirs::data_local_dir().expect("native local data directory");
        let state_base =
            cargo_rail::utils::canonicalize_allow_missing(&state_base.join("cargo-rail/state"))?.join("workspaces");
        let receipt = state_base
            .join(cache.file_name().unwrap())
            .join("receipts/receipt.json");
        assert_eq!(
            std::path::Path::new(preview["files"][0].as_str().unwrap()),
            receipt.as_path()
        );
        assert!(!receipt.exists(), "preview wrote a receipt to user storage");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_clean_reports_only() {
    let result: Result<()> = (|| {
        let temp = create_test_workspace()?;
        let ctx = CleanContext::capture(temp.path(), None).unwrap();

        // Run clean --reports
        run_clean(
            &ctx,
            CleanOptions {
                all: false,
                cache: false,
                prune_backups: false,
                all_backups: false,
                reports: true,
                check: false,
                format: TextJsonOutputFormat::default(),
            },
        )
        .unwrap();

        // Verify reports removed, others remain
        assert!(temp.path().join("target/cargo-rail/compiler-artifacts-v1").exists());
        assert!(!temp.path().join("target/cargo-rail/report.md").exists());
        let manager = BackupManager::new(temp.path());
        assert_eq!(manager.list_backups().unwrap().len(), 5);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_clean_reports_without_cargo_or_git_discovery() {
    let result: Result<()> = (|| {
        let workspace = TempDir::new()?;
        let report = workspace.path().join("target/cargo-rail/report.md");
        fs::create_dir_all(report.parent().unwrap())?;
        fs::write(&report, "generated report\n")?;

        let cache = TempDir::new()?;
        let output = run_cache_command(&workspace, &cache, &["rail", "clean", "--reports"]);
        assert!(output.status.success(), "narrow clean failed: {output:?}");
        assert!(!report.exists());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_clean_backups_prune() {
    let result: Result<()> = (|| {
        let temp = create_test_workspace()?;
        let ctx = CleanContext::capture(temp.path(), None).unwrap();

        // Run clean --prune-backups (should prune to default 3)
        run_clean(
            &ctx,
            CleanOptions {
                all: false,
                cache: false,
                prune_backups: true,
                all_backups: false,
                reports: false,
                check: false,
                format: TextJsonOutputFormat::default(),
            },
        )
        .unwrap();

        // Verify backups pruned, others remain
        assert!(temp.path().join("target/cargo-rail/compiler-artifacts-v1").exists());
        assert!(temp.path().join("target/cargo-rail/report.md").exists());
        let manager = BackupManager::new(temp.path());
        assert_eq!(manager.list_backups().unwrap().len(), 3);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_bare_clean_is_a_usage_error_without_mutation() {
    let result: Result<()> = (|| {
        let temp = create_test_workspace()?;
        let cache = TempDir::new().unwrap();

        // Bare clean has no deletion authority.
        let output = run_cache_command(&temp, &cache, &["rail", "clean"]);
        assert_eq!(output.status.code(), Some(2), "bare clean must fail: {output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("explicit artifact selector"));

        // Verify nothing was removed.
        assert!(temp.path().join("target/cargo-rail/compiler-artifacts-v1").exists());
        assert!(temp.path().join("target/cargo-rail/report.md").exists());
        let manager = BackupManager::new(temp.path());
        assert_eq!(manager.list_backups().unwrap().len(), 5);
        Ok(())
    })();
    super::helpers::finish_test(result);
}
