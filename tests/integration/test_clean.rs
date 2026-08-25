use anyhow::Result;
use cargo_rail::backup::{BackupManager, BackupMetadata};
use cargo_rail::commands::{TextJsonOutputFormat, run_clean};
use cargo_rail::workspace::WorkspaceContext;
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
    fs::write(workspace.join("target/cargo-rail/metadata.json"), "{}")?;
    fs::write(workspace.join("target/cargo-rail/report.md"), "# Report")?;
    fs::create_dir_all(workspace.join("target/cargo-rail/cache"))?;
    fs::write(
        workspace.join("target/cargo-rail/cache/compiler-diags-v1.json"),
        "{\"version\":1,\"entries\":{}}",
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
            config_snapshot: None,
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
    let root = owner.join("local-cas-v2");
    fs::create_dir_all(&root)?;
    fs::write(owner.join("LOCAL_TRUST_DOMAIN"), format!("{TRUST_DOMAIN}\n"))?;
    fs::write(
        root.join("OWNER"),
        format!("cargo-rail-local-cas\nschema=2\ntrust-domain={TRUST_DOMAIN}\n"),
    )?;
    fs::write(root.join("CAPACITY.json"), b"{\"version\":2,\"result_bytes\":0}")?;
    fs::write(
        root.join("NATIVE_LEDGER.json"),
        b"{\"version\":1,\"terminal_states\":0,\"terminal_bytes\":0,\"disabled\":false}",
    )?;
    for directory in [
        "results",
        "pins",
        "leases",
        "staging",
        "native-actions",
        "compiler-evidence-candidates",
    ] {
        fs::create_dir(root.join(directory))?;
    }
    #[cfg(target_os = "macos")]
    fs::create_dir(root.join("sysroot-identities"))?;
    fs::write(owner.join("local-cas-v2.lock"), b"")?;
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
        let lock = cache.path().join("cargo-rail/local-cas-v2.lock");
        assert!(lock.is_file(), "the lifecycle authority must survive root reclamation");
        assert_eq!(fs::metadata(lock).unwrap().len(), 0);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn apply_cleanup_migrates_an_owned_pre_lifecycle_local_cas() {
    let result: Result<()> = (|| {
        let workspace = create_test_workspace()?;
        let cache = TempDir::new().unwrap();
        let root = create_empty_local_cas(&cache)?;
        let lock = cache.path().join("cargo-rail/local-cas-v2.lock");
        fs::remove_file(&lock).unwrap();

        let cleaned = run_cache_command(
            &workspace,
            &cache,
            &["rail", "cache", "clean", "--scope", "local", "-f", "json"],
        );

        assert!(cleaned.status.success(), "transition cleanup failed: {cleaned:?}");
        let cleaned_json: serde_json::Value = serde_json::from_slice(&cleaned.stdout).unwrap();
        assert!(cleaned_json["reclaimed_bytes"].as_u64().unwrap() > 0);
        assert!(!root.exists());
        assert!(
            lock.is_file(),
            "apply must establish the persistent lifecycle authority"
        );
        assert_eq!(fs::metadata(lock).unwrap().len(), 0);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn compatibility_cleanup_never_removes_the_shared_local_cas() {
    let result: Result<()> = (|| {
        for arguments in [&["rail", "clean"][..], &["rail", "clean", "--cache"][..]] {
            let workspace = create_test_workspace()?;
            let cache = TempDir::new().unwrap();
            let root = create_empty_local_cas(&cache)?;
            let lock = cache.path().join("cargo-rail/local-cas-v2.lock");
            let shared_result = root.join("results/shared-result");
            fs::write(&shared_result, b"shared across workspaces")?;

            let cleaned = run_cache_command(&workspace, &cache, arguments);

            assert!(cleaned.status.success(), "compatibility cleanup failed: {cleaned:?}");
            assert_eq!(fs::read(&shared_result)?, b"shared across workspaces");
            assert!(lock.is_file());
            assert_eq!(fs::metadata(lock).unwrap().len(), 0);
            assert!(!workspace.path().join("target/cargo-rail/metadata.json").exists());
            assert!(!workspace.path().join("target/cargo-rail/cache").exists());
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
        assert_eq!(status_json["status"]["workspace"]["fully_bounded"], true);

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
        assert!(workspace.path().join("target/cargo-rail/metadata.json").exists());

        let cleaned = run_cache_command(
            &workspace,
            &cache,
            &["rail", "cache", "clean", "--scope", "workspace", "-f", "json"],
        );
        assert!(cleaned.status.success(), "cleanup failed: {cleaned:?}");
        assert!(!workspace.path().join("target/cargo-rail/metadata.json").exists());
        assert!(!workspace.path().join("target/cargo-rail/cache").exists());
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
        assert!(temp.path().join("target/cargo-rail/metadata.json").exists());
        assert!(
            temp.path()
                .join("target/cargo-rail/cache/compiler-diags-v1.json")
                .exists()
        );
        assert!(temp.path().join("target/cargo-rail/report.md").exists());
        let manager = BackupManager::new(temp.path());
        assert_eq!(manager.list_backups().unwrap().len(), 5);

        // Run clean (no flags = clean all)
        let output = run_cache_command(&temp, &cache, &["rail", "clean"]);
        assert!(output.status.success(), "clean all failed: {output:?}");

        // Verify artifacts removed
        assert!(!temp.path().join("target/cargo-rail/metadata.json").exists());
        assert!(!temp.path().join("target/cargo-rail/cache").exists());
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
        assert!(!temp.path().join("target/cargo-rail/metadata.json").exists());
        assert!(!temp.path().join("target/cargo-rail/cache").exists());
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
        let cache_state = temp.path().join("target/cargo-rail/cache");
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

#[test]
fn test_clean_reports_only() {
    let result: Result<()> = (|| {
        let temp = create_test_workspace()?;
        let ctx = WorkspaceContext::build(temp.path()).unwrap();

        // Run clean --reports
        run_clean(&ctx, false, false, true, false, TextJsonOutputFormat::default()).unwrap();

        // Verify reports removed, others remain
        assert!(temp.path().join("target/cargo-rail/metadata.json").exists());
        assert!(temp.path().join("target/cargo-rail/cache").exists());
        assert!(!temp.path().join("target/cargo-rail/report.md").exists());
        let manager = BackupManager::new(temp.path());
        assert_eq!(manager.list_backups().unwrap().len(), 5);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_clean_backups_prune() {
    let result: Result<()> = (|| {
        let temp = create_test_workspace()?;
        let ctx = WorkspaceContext::build(temp.path()).unwrap();

        // Run clean --backups (should prune to default 3)
        run_clean(&ctx, false, true, false, false, TextJsonOutputFormat::default()).unwrap();

        // Verify backups pruned, others remain
        assert!(temp.path().join("target/cargo-rail/metadata.json").exists());
        assert!(temp.path().join("target/cargo-rail/cache").exists());
        assert!(temp.path().join("target/cargo-rail/report.md").exists());
        let manager = BackupManager::new(temp.path());
        assert_eq!(manager.list_backups().unwrap().len(), 3);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_clean_default() {
    let result: Result<()> = (|| {
        let temp = create_test_workspace()?;
        let cache = TempDir::new().unwrap();

        // Run clean (no flags) -> should clean everything now
        let output = run_cache_command(&temp, &cache, &["rail", "clean"]);
        assert!(output.status.success(), "default cleanup failed: {output:?}");

        // Verify everything removed
        assert!(!temp.path().join("target/cargo-rail/metadata.json").exists());
        assert!(!temp.path().join("target/cargo-rail/cache").exists());
        assert!(!temp.path().join("target/cargo-rail/report.md").exists());
        let manager = BackupManager::new(temp.path());
        assert_eq!(manager.list_backups().unwrap().len(), 0);
        Ok(())
    })();
    super::helpers::finish_test(result);
}
