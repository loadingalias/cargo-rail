//! `cargo rail config sync` - Configuration synchronization
//!
//! Syncs configuration from .config/rail.toml to other files:
//! - rust-toolchain.toml (from `[toolchain]` section)
//!
//! Designed for extensibility - easy to add new sync targets in the future
//! (e.g., .cargo/config.toml, VS Code settings, etc.)

use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;
use std::path::Path;

/// Run config sync command
pub fn run_config_sync(ctx: &WorkspaceContext, check_only: bool) -> RailResult<()> {
  if check_only {
    println!("🔍 Checking configuration sync status...\n");
  } else {
    println!("🔄 Syncing configuration files...\n");
  }

  // Get rail config
  let rail_config = ctx.rail_config()?;

  // Initialize sync registry
  let mut registry = SyncRegistry::new();

  // Register all syncable config sections
  registry.register(Box::new(ToolchainSyncer));
  // Future: registry.register(Box::new(CargoConfigSyncer));
  // Future: registry.register(Box::new(VsCodeSyncer));

  // Run sync for all registered sections
  let mut has_errors = false;
  let mut sync_count = 0;

  for syncer in &registry.syncers {
    let result = if check_only {
      syncer.check(ctx.workspace_root(), rail_config)
    } else {
      syncer.sync(ctx.workspace_root(), rail_config)
    };

    match result {
      Ok(status) => match status {
        SyncStatus::InSync(msg) => {
          println!("  ✓ {}", msg);
        }
        SyncStatus::Synced(msg) => {
          println!("  ✓ {}", msg);
          sync_count += 1;
        }
        SyncStatus::OutOfSync(msg) => {
          println!("  ✗ {}", msg);
          has_errors = true;
        }
      },
      Err(e) => {
        println!("  ✗ Error: {}", e);
        has_errors = true;
      }
    }
  }

  if check_only {
    if has_errors {
      println!("\n❌ Configuration check FAILED - files are out of sync");
      println!("Run 'cargo rail config sync' to fix");
      return Err(RailError::message("Configuration files are out of sync"));
    } else {
      println!("\n✅ All configuration files are in sync!");
    }
  } else if has_errors {
    println!("\n⚠️  Some configuration files could not be synced");
    return Err(RailError::message("Configuration sync had errors"));
  } else {
    println!("\n✅ Configuration sync complete!");
    if sync_count > 0 {
      println!("  {} file(s) updated", sync_count);
    }
  }

  Ok(())
}

// ============================================================================
// Extensible Sync Infrastructure
// ============================================================================

/// Status of a sync operation
#[derive(Debug)]
pub enum SyncStatus {
  /// Files are already in sync (no action needed)
  InSync(String),
  /// Files were synced successfully
  Synced(String),
  /// Files are out of sync (check mode only)
  OutOfSync(String),
}

/// Trait for config section syncers
///
/// Implement this trait to add new sync targets (e.g., .cargo/config.toml, VS Code settings)
pub trait ConfigSyncer {
  /// Check if configuration is in sync (doesn't modify files)
  fn check(&self, workspace_root: &Path, config: &crate::config::RailConfig) -> RailResult<SyncStatus>;

  /// Sync configuration (writes/updates files)
  fn sync(&self, workspace_root: &Path, config: &crate::config::RailConfig) -> RailResult<SyncStatus>;
}

/// Registry of config syncers
pub struct SyncRegistry {
  syncers: Vec<Box<dyn ConfigSyncer>>,
}

impl Default for SyncRegistry {
  fn default() -> Self {
    Self::new()
  }
}

impl SyncRegistry {
  pub fn new() -> Self {
    Self { syncers: Vec::new() }
  }

  pub fn register(&mut self, syncer: Box<dyn ConfigSyncer>) {
    self.syncers.push(syncer);
  }
}

// ============================================================================
// Toolchain Syncer (rust-toolchain.toml)
// ============================================================================

struct ToolchainSyncer;

impl ConfigSyncer for ToolchainSyncer {
  fn check(&self, workspace_root: &Path, config: &crate::config::RailConfig) -> RailResult<SyncStatus> {
    let toolchain_path = workspace_root.join("rust-toolchain.toml");

    // If rust-toolchain.toml doesn't exist, it's out of sync
    if !toolchain_path.exists() {
      return Ok(SyncStatus::OutOfSync(
        "rust-toolchain.toml does not exist (will be created)".to_string(),
      ));
    }

    // Read existing file
    let existing_content = std::fs::read_to_string(&toolchain_path)
      .map_err(|e| RailError::message(format!("Failed to read rust-toolchain.toml: {}", e)))?;

    // Generate expected content from rail config
    let expected_content = generate_rust_toolchain_toml(&config.toolchain);

    // Compare
    if existing_content.trim() == expected_content.trim() {
      Ok(SyncStatus::InSync("rust-toolchain.toml is in sync".to_string()))
    } else {
      Ok(SyncStatus::OutOfSync(
        "rust-toolchain.toml is out of sync with rail.toml".to_string(),
      ))
    }
  }

  fn sync(&self, workspace_root: &Path, config: &crate::config::RailConfig) -> RailResult<SyncStatus> {
    let toolchain_path = workspace_root.join("rust-toolchain.toml");

    // Check if already in sync
    let check_status = self.check(workspace_root, config)?;
    if matches!(check_status, SyncStatus::InSync(_)) {
      return Ok(check_status);
    }

    // Generate and write rust-toolchain.toml
    let content = generate_rust_toolchain_toml(&config.toolchain);
    std::fs::write(&toolchain_path, content)
      .map_err(|e| RailError::message(format!("Failed to write rust-toolchain.toml: {}", e)))?;

    Ok(SyncStatus::Synced(
      "rust-toolchain.toml synced from rail.toml [toolchain]".to_string(),
    ))
  }
}

/// Generate rust-toolchain.toml content from ToolchainConfig
///
/// Supports all rust-toolchain.toml fields:
/// - channel: Rust version (stable, beta, nightly, 1.76.0, etc.)
/// - path: Custom local toolchain (mutually exclusive with channel)
/// - profile: Component group (minimal, default, complete)
/// - components: Additional components (ignored if path is set)
/// - targets: Cross-compilation targets (ignored if path is set)
fn generate_rust_toolchain_toml(config: &crate::config::ToolchainConfig) -> String {
  let mut content = String::new();

  content.push_str("[toolchain]\n");

  // Handle path vs channel (mutually exclusive)
  if let Some(ref path) = config.path {
    // path mode: only path is used, channel/components/targets are ignored
    content.push_str(&format!("path = \"{}\"\n", path));
  } else {
    // channel mode: use channel, profile, components, targets
    content.push_str(&format!("channel = \"{}\"\n", config.channel));
    content.push_str(&format!("profile = \"{}\"\n", config.profile));

    if !config.components.is_empty() {
      content.push_str("components = [");
      for (i, component) in config.components.iter().enumerate() {
        if i > 0 {
          content.push_str(", ");
        }
        content.push_str(&format!("\"{}\"", component));
      }
      content.push_str("]\n");
    }

    if !config.targets.is_empty() {
      content.push_str("\ntargets = [\n");
      for target in &config.targets {
        content.push_str(&format!("  \"{}\",\n", target));
      }
      content.push_str("]\n");
    }
  }

  content
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::ToolchainConfig;

  #[test]
  fn test_generate_rust_toolchain_toml_minimal() {
    let config = ToolchainConfig {
      channel: "stable".to_string(),
      path: None,
      profile: "default".to_string(),
      components: vec![],
      targets: vec![],
    };

    let content = generate_rust_toolchain_toml(&config);
    assert!(content.contains("channel = \"stable\""));
    assert!(content.contains("profile = \"default\""));
    assert!(!content.contains("path ="));
    assert!(!content.contains("components"));
    assert!(!content.contains("targets"));
  }

  #[test]
  fn test_generate_rust_toolchain_toml_full() {
    let config = ToolchainConfig {
      channel: "1.76.0".to_string(),
      path: None,
      profile: "minimal".to_string(),
      components: vec!["clippy".to_string(), "rustfmt".to_string()],
      targets: vec![
        "x86_64-unknown-linux-gnu".to_string(),
        "aarch64-apple-darwin".to_string(),
      ],
    };

    let content = generate_rust_toolchain_toml(&config);
    assert!(content.contains("channel = \"1.76.0\""));
    assert!(content.contains("profile = \"minimal\""));
    assert!(content.contains("components = [\"clippy\", \"rustfmt\"]"));
    assert!(content.contains("x86_64-unknown-linux-gnu"));
    assert!(content.contains("aarch64-apple-darwin"));
    assert!(!content.contains("path ="));
  }

  #[test]
  fn test_generate_rust_toolchain_toml_path_mode() {
    let config = ToolchainConfig {
      channel: "stable".to_string(), // Ignored in path mode
      path: Some("/custom/toolchain/path".to_string()),
      profile: "default".to_string(),                        // Ignored in path mode
      components: vec!["clippy".to_string()],                // Ignored in path mode
      targets: vec!["x86_64-unknown-linux-gnu".to_string()], // Ignored in path mode
    };

    let content = generate_rust_toolchain_toml(&config);
    assert!(content.contains("path = \"/custom/toolchain/path\""));
    // In path mode, these should NOT appear
    assert!(!content.contains("channel ="));
    assert!(!content.contains("profile ="));
    assert!(!content.contains("components"));
    assert!(!content.contains("targets"));
  }
}
