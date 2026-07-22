use cargo_rail::backup::{BackupManager, BackupMetadata};
use cargo_rail::commands::{TextJsonOutputFormat, run_clean};
use cargo_rail::workspace::WorkspaceContext;
use std::fs;
use tempfile::TempDir;

fn create_test_workspace() -> TempDir {
  let temp = TempDir::new().unwrap();
  let workspace = temp.path();

  // Create Cargo.toml
  fs::write(
    workspace.join("Cargo.toml"),
    r#"
    [workspace]
    members = ["crates/*"]
    "#,
  )
  .unwrap();

  // Create dummy crate
  fs::create_dir_all(workspace.join("crates/foo")).unwrap();
  fs::write(
    workspace.join("crates/foo/Cargo.toml"),
    r#"
    [package]
    name = "foo"
    version = "0.1.0"
    edition = "2021"
    "#,
  )
  .unwrap();
  // Create dummy source file
  fs::create_dir_all(workspace.join("crates/foo/src")).unwrap();
  fs::write(workspace.join("crates/foo/src/lib.rs"), "").unwrap();

  // Create target directories
  fs::create_dir_all(workspace.join("target/cargo-rail")).unwrap();
  fs::write(workspace.join("target/cargo-rail/metadata.json"), "{}").unwrap();
  fs::write(workspace.join("target/cargo-rail/report.md"), "# Report").unwrap();
  fs::create_dir_all(workspace.join("target/cargo-rail/cache")).unwrap();
  fs::write(
    workspace.join("target/cargo-rail/cache/compiler-diags-v1.json"),
    "{\"version\":1,\"entries\":{}}",
  )
  .unwrap();
  fs::create_dir_all(workspace.join("target/cargo-rail/hermetic/inventories/example/cargo-home/registry")).unwrap();
  fs::write(
    workspace.join("target/cargo-rail/hermetic/inventories/example/cargo-home/registry/source.rs"),
    "pub fn fetched() {}\n",
  )
  .unwrap();

  // Initialize git repo
  std::process::Command::new("git")
    .arg("init")
    .current_dir(workspace)
    .output()
    .unwrap();

  // Configure git user for commits (needed for some git ops, though maybe not strict requirement for clean)
  std::process::Command::new("git")
    .args(["config", "user.email", "you@example.com"])
    .current_dir(workspace)
    .output()
    .unwrap();
  std::process::Command::new("git")
    .args(["config", "user.name", "Your Name"])
    .current_dir(workspace)
    .output()
    .unwrap();

  // Create deterministic backup fixtures (avoid time-based sleeps).
  let backup_root = workspace.join("target/cargo-rail/backups");
  fs::create_dir_all(&backup_root).unwrap();
  for i in 1..=5 {
    let backup_dir = backup_root.join(format!("2024-01-01-00000{}-000", i));
    fs::create_dir_all(&backup_dir).unwrap();
    let metadata = BackupMetadata {
      timestamp: format!("2024-01-01T00:00:0{}+00:00", i),
      command: format!("test {}", i),
      files_modified: vec![],
      config_snapshot: None,
      description: None,
    };
    metadata.save(&backup_dir).unwrap();
  }

  temp
}

#[test]
fn test_clean_all() {
  let temp = create_test_workspace();
  let ctx = WorkspaceContext::build(temp.path()).unwrap();

  // Verify artifacts exist
  assert!(temp.path().join("target/cargo-rail/metadata.json").exists());
  assert!(
    temp
      .path()
      .join("target/cargo-rail/cache/compiler-diags-v1.json")
      .exists()
  );
  assert!(temp.path().join("target/cargo-rail/report.md").exists());
  let manager = BackupManager::new(temp.path());
  assert_eq!(manager.list_backups().unwrap().len(), 5);

  // Run clean (no flags = clean all)
  run_clean(&ctx, false, false, false, false, TextJsonOutputFormat::default()).unwrap();

  // Verify artifacts removed
  assert!(!temp.path().join("target/cargo-rail/metadata.json").exists());
  assert!(!temp.path().join("target/cargo-rail/cache").exists());
  assert!(!temp.path().join("target/cargo-rail/hermetic").exists());
  assert!(!temp.path().join("target/cargo-rail/report.md").exists());
  assert_eq!(manager.list_backups().unwrap().len(), 0);
}

#[test]
fn test_clean_cache_only() {
  let temp = create_test_workspace();
  let ctx = WorkspaceContext::build(temp.path()).unwrap();

  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;

    let inventory = temp
      .path()
      .join("target/cargo-rail/hermetic/inventories/example/cargo-home/registry");
    fs::set_permissions(inventory.join("source.rs"), fs::Permissions::from_mode(0o444)).unwrap();
    fs::set_permissions(&inventory, fs::Permissions::from_mode(0o555)).unwrap();
  }

  // Run clean --cache
  run_clean(&ctx, true, false, false, false, TextJsonOutputFormat::default()).unwrap();

  // Verify cache removed, others remain
  assert!(!temp.path().join("target/cargo-rail/metadata.json").exists());
  assert!(!temp.path().join("target/cargo-rail/cache").exists());
  assert!(!temp.path().join("target/cargo-rail/hermetic").exists());
  assert!(temp.path().join("target/cargo-rail/report.md").exists());
  let manager = BackupManager::new(temp.path());
  assert_eq!(manager.list_backups().unwrap().len(), 5);
}

#[cfg(unix)]
#[test]
fn test_clean_refuses_to_follow_a_hermetic_state_symlink() {
  use std::os::unix::fs::symlink;

  let temp = create_test_workspace();
  let ctx = WorkspaceContext::build(temp.path()).unwrap();
  let outside = TempDir::new().unwrap();
  fs::write(outside.path().join("keep"), "outside state\n").unwrap();
  let hermetic = temp.path().join("target/cargo-rail/hermetic");
  fs::remove_dir_all(&hermetic).unwrap();
  symlink(outside.path(), &hermetic).unwrap();

  let error = run_clean(&ctx, true, false, false, false, TextJsonOutputFormat::default())
    .expect_err("cache cleanup must reject a hermetic-state symlink");
  assert!(error.to_string().contains("not a real directory"), "{error}");
  assert_eq!(
    fs::read_to_string(outside.path().join("keep")).unwrap(),
    "outside state\n"
  );
}

#[test]
fn test_clean_reports_only() {
  let temp = create_test_workspace();
  let ctx = WorkspaceContext::build(temp.path()).unwrap();

  // Run clean --reports
  run_clean(&ctx, false, false, true, false, TextJsonOutputFormat::default()).unwrap();

  // Verify reports removed, others remain
  assert!(temp.path().join("target/cargo-rail/metadata.json").exists());
  assert!(temp.path().join("target/cargo-rail/cache").exists());
  assert!(temp.path().join("target/cargo-rail/hermetic").exists());
  assert!(!temp.path().join("target/cargo-rail/report.md").exists());
  let manager = BackupManager::new(temp.path());
  assert_eq!(manager.list_backups().unwrap().len(), 5);
}

#[test]
fn test_clean_backups_prune() {
  let temp = create_test_workspace();
  let ctx = WorkspaceContext::build(temp.path()).unwrap();

  // Run clean --backups (should prune to default 3)
  run_clean(&ctx, false, true, false, false, TextJsonOutputFormat::default()).unwrap();

  // Verify backups pruned, others remain
  assert!(temp.path().join("target/cargo-rail/metadata.json").exists());
  assert!(temp.path().join("target/cargo-rail/cache").exists());
  assert!(temp.path().join("target/cargo-rail/hermetic").exists());
  assert!(temp.path().join("target/cargo-rail/report.md").exists());
  let manager = BackupManager::new(temp.path());
  assert_eq!(manager.list_backups().unwrap().len(), 3);
}

#[test]
fn test_clean_default() {
  let temp = create_test_workspace();
  let ctx = WorkspaceContext::build(temp.path()).unwrap();

  // Run clean (no flags) -> should clean everything now
  run_clean(&ctx, false, false, false, false, TextJsonOutputFormat::default()).unwrap();

  // Verify everything removed
  assert!(!temp.path().join("target/cargo-rail/metadata.json").exists());
  assert!(!temp.path().join("target/cargo-rail/cache").exists());
  assert!(!temp.path().join("target/cargo-rail/hermetic").exists());
  assert!(!temp.path().join("target/cargo-rail/report.md").exists());
  let manager = BackupManager::new(temp.path());
  assert_eq!(manager.list_backups().unwrap().len(), 0);
}
