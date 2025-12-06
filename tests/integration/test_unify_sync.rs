//! Integration tests for `cargo rail unify sync` command

use crate::helpers::{TestWorkspace, run_cargo_rail};
use anyhow::Result;

#[test]
fn test_unify_sync_detects_targets_from_rust_toolchain() -> Result<()> {
  let ws = TestWorkspace::new_named("unify-sync-toolchain")?;

  // Create rust-toolchain.toml with targets
  std::fs::write(
    ws.path.join("rust-toolchain.toml"),
    r#"[toolchain]
channel = "stable"
targets = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]
"#,
  )?;

  // Create rail.toml with no targets
  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"targets = []

[unify]
include_paths = true
"#,
  )?;

  // Run unify sync
  let output = run_cargo_rail(&ws.path, &["rail", "unify", "sync"])?;
  assert!(output.status.success(), "unify sync should succeed");

  // Verify targets were added
  let config = std::fs::read_to_string(ws.path.join(".config/rail.toml"))?;
  assert!(
    config.contains("x86_64-unknown-linux-gnu"),
    "should contain x86_64 target"
  );
  assert!(
    config.contains("aarch64-unknown-linux-gnu"),
    "should contain aarch64 target"
  );

  Ok(())
}

#[test]
fn test_unify_sync_clean_replaces_targets() -> Result<()> {
  let ws = TestWorkspace::new_named("unify-sync-replace")?;

  // Create rust-toolchain.toml with one target
  std::fs::write(
    ws.path.join("rust-toolchain.toml"),
    r#"[toolchain]
channel = "stable"
targets = ["x86_64-unknown-linux-gnu"]
"#,
  )?;

  // Create rail.toml with a different target (manually added, not in any TOML)
  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"targets = ["aarch64-apple-darwin"]

[unify]
include_paths = true
"#,
  )?;

  // Run unify sync
  let output = run_cargo_rail(&ws.path, &["rail", "unify", "sync"])?;
  assert!(output.status.success(), "unify sync should succeed");

  // Verify targets were REPLACED (not merged)
  let config = std::fs::read_to_string(ws.path.join(".config/rail.toml"))?;
  assert!(
    config.contains("x86_64-unknown-linux-gnu"),
    "should contain detected target"
  );
  assert!(
    !config.contains("aarch64-apple-darwin"),
    "should NOT contain manually added target"
  );

  Ok(())
}

#[test]
fn test_unify_sync_check_mode_does_not_modify() -> Result<()> {
  let ws = TestWorkspace::new_named("unify-sync-check")?;

  // Create rust-toolchain.toml with targets
  std::fs::write(
    ws.path.join("rust-toolchain.toml"),
    r#"[toolchain]
channel = "stable"
targets = ["x86_64-unknown-linux-gnu"]
"#,
  )?;

  // Create rail.toml with no targets
  let original_config = r#"targets = []

[unify]
include_paths = true
"#;
  std::fs::write(ws.path.join(".config/rail.toml"), original_config)?;

  // Run unify sync --check
  let output = run_cargo_rail(&ws.path, &["rail", "unify", "sync", "--check"])?;
  assert!(output.status.success(), "unify sync --check should succeed");

  // Verify config was NOT modified
  let config = std::fs::read_to_string(ws.path.join(".config/rail.toml"))?;
  assert_eq!(config, original_config, "config should not be modified in check mode");

  // Verify output shows what would be added
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(stdout.contains("would add"), "should show what would be added");
  assert!(stdout.contains("x86_64-unknown-linux-gnu"), "should list the target");

  Ok(())
}

#[test]
fn test_unify_sync_preserves_other_config() -> Result<()> {
  let ws = TestWorkspace::new_named("unify-sync-preserve")?;

  // Create rust-toolchain.toml with target
  std::fs::write(
    ws.path.join("rust-toolchain.toml"),
    r#"[toolchain]
channel = "stable"
targets = ["x86_64-unknown-linux-gnu"]
"#,
  )?;

  // Create rail.toml with custom config
  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"# My custom comment
targets = []

[unify]
include_paths = true
pin_transitives = true
exclude = ["some-dep"]

[release]
tag_prefix = "v"
"#,
  )?;

  // Run unify sync
  let output = run_cargo_rail(&ws.path, &["rail", "unify", "sync"])?;
  assert!(output.status.success(), "unify sync should succeed");

  // Verify other config was preserved
  let config = std::fs::read_to_string(ws.path.join(".config/rail.toml"))?;
  assert!(
    config.contains("pin_transitives = true"),
    "should preserve unify settings"
  );
  assert!(
    config.contains(r#"exclude = ["some-dep"]"#),
    "should preserve exclude list"
  );
  assert!(config.contains("[release]"), "should preserve release section");
  assert!(config.contains("tag_prefix"), "should preserve release settings");

  Ok(())
}

#[test]
fn test_unify_sync_already_in_sync() -> Result<()> {
  let ws = TestWorkspace::new_named("unify-sync-noop")?;

  // Create rust-toolchain.toml with target
  std::fs::write(
    ws.path.join("rust-toolchain.toml"),
    r#"[toolchain]
channel = "stable"
targets = ["x86_64-unknown-linux-gnu"]
"#,
  )?;

  // Create rail.toml with same target
  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"targets = ["x86_64-unknown-linux-gnu"]

[unify]
include_paths = true
"#,
  )?;

  // Run unify sync
  let output = run_cargo_rail(&ws.path, &["rail", "unify", "sync"])?;
  assert!(output.status.success(), "unify sync should succeed");

  // Verify output says already in sync
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(stdout.contains("already in sync"), "should report already in sync");

  Ok(())
}

#[test]
fn test_unify_sync_fails_without_config() -> Result<()> {
  let ws = TestWorkspace::new_named("unify-sync-no-config")?;

  // Remove config
  ws.remove_config()?;

  // Run unify sync
  let output = run_cargo_rail(&ws.path, &["rail", "unify", "sync"])?;
  assert!(!output.status.success(), "unify sync should fail without config");

  // Verify error message
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(stderr.contains("no rail.toml found"), "should mention missing config");

  Ok(())
}

#[test]
fn test_unify_sync_from_cargo_config() -> Result<()> {
  let ws = TestWorkspace::new_named("unify-sync-cargo-config")?;

  // Create .cargo/config.toml with target
  std::fs::create_dir_all(ws.path.join(".cargo"))?;
  std::fs::write(
    ws.path.join(".cargo/config.toml"),
    r#"[build]
target = "x86_64-unknown-linux-musl"
"#,
  )?;

  // Create rail.toml with no targets
  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"targets = []

[unify]
include_paths = true
"#,
  )?;

  // Run unify sync
  let output = run_cargo_rail(&ws.path, &["rail", "unify", "sync"])?;
  assert!(output.status.success(), "unify sync should succeed");

  // Verify target was detected from .cargo/config.toml
  let config = std::fs::read_to_string(ws.path.join(".config/rail.toml"))?;
  assert!(
    config.contains("x86_64-unknown-linux-musl"),
    "should detect target from .cargo/config.toml"
  );

  Ok(())
}
