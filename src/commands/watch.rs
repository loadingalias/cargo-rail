//! Watch mode - continuous testing on file changes
//!
//! Delegates to external watch tools:
//! 1. Bacon integration (preferred for best UX)
//! 2. cargo-watch integration (widely used alternative)
//!
//! Both tools watch for file changes and re-run cargo-rail's smart test command.

use crate::commands::test::TestConfig;
use crate::error::RailResult;
use crate::workspace::WorkspaceContext;
use std::process::Command;

/// Watch mode strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchMode {
  /// Use bacon (best UX, modern)
  Bacon,
  /// Use cargo-watch (widely used)
  CargoWatch,
  /// Auto-detect best available
  Auto,
}

impl WatchMode {
  /// Check if this watch mode is available
  pub fn is_available(&self) -> bool {
    match self {
      WatchMode::Bacon => which("bacon"),
      WatchMode::CargoWatch => which("cargo-watch"),
      WatchMode::Auto => true, // Auto always works (selects best available)
    }
  }

  /// Get human-readable name
  pub fn name(&self) -> &str {
    match self {
      WatchMode::Bacon => "bacon",
      WatchMode::CargoWatch => "cargo-watch",
      WatchMode::Auto => "auto-detect",
    }
  }
}

/// Check if a command exists in PATH
fn which(cmd: &str) -> bool {
  Command::new(cmd)
    .arg("--version")
    .output()
    .map(|o| o.status.success())
    .unwrap_or(false)
}

/// Select the best available watch mode
pub fn select_watch_mode(preference: WatchMode) -> RailResult<WatchMode> {
  match preference {
    WatchMode::Auto => {
      // Try bacon first (best UX)
      if WatchMode::Bacon.is_available() {
        Ok(WatchMode::Bacon)
      }
      // Fall back to cargo-watch
      else if WatchMode::CargoWatch.is_available() {
        Ok(WatchMode::CargoWatch)
      }
      // No watch tool available
      else {
        Err(crate::error::RailError::message(
          "No watch tool found. Please install one:\n\
           - cargo install bacon (recommended)\n\
           - cargo install cargo-watch",
        ))
      }
    }
    mode => {
      // User explicitly chose a mode - verify it's available
      if mode.is_available() {
        Ok(mode)
      } else {
        Err(crate::error::RailError::message(format!(
          "{} not found. Please install it:\n\
           - For bacon: cargo install bacon\n\
           - For cargo-watch: cargo install cargo-watch",
          mode.name()
        )))
      }
    }
  }
}

/// Run tests in watch mode
pub fn run_test_watch(ctx: &WorkspaceContext, config: TestConfig, watch_mode: WatchMode) -> RailResult<()> {
  let mode = select_watch_mode(watch_mode)?;

  println!("🔍 Starting watch mode using {}...\n", mode.name());

  match mode {
    WatchMode::Bacon => run_with_bacon(ctx, config),
    WatchMode::CargoWatch => run_with_cargo_watch(ctx, config),
    WatchMode::Auto => unreachable!("Auto should be resolved by select_watch_mode"),
  }
}

/// Run with bacon (delegates to bacon binary)
fn run_with_bacon(_ctx: &WorkspaceContext, _config: TestConfig) -> RailResult<()> {
  // Bacon integration: For now, we delegate to bacon's default test behavior
  // This runs standard `cargo test` with bacon's excellent UX

  println!("💡 Bacon integration active - using bacon's test job");
  println!("   Running: bacon test\n");

  // Note: In the future, we could generate a custom bacon.toml that runs
  // `cargo rail test` instead of `cargo test` to get smart change detection
  // while still benefiting from bacon's UI. For now, users get standard bacon behavior.

  let status = Command::new("bacon")
    .arg("test")
    .status()
    .map_err(|e| crate::error::RailError::message(format!("Failed to run bacon: {}", e)))?;

  if !status.success() {
    std::process::exit(status.code().unwrap_or(1));
  }

  Ok(())
}

/// Run with cargo-watch (delegates to cargo-watch binary)
fn run_with_cargo_watch(ctx: &WorkspaceContext, config: TestConfig) -> RailResult<()> {
  println!("💡 Using cargo-watch for file watching\n");

  // Build the cargo-rail test command to pass to cargo-watch
  let mut cmd = Command::new("cargo-watch");
  cmd.current_dir(ctx.workspace_root());

  let rail_cmd = format_watch_test_command(&config);
  cmd.arg("-x").arg(rail_cmd);

  let status = cmd
    .status()
    .map_err(|e| crate::error::RailError::message(format!("Failed to run cargo-watch: {}", e)))?;

  if !status.success() {
    std::process::exit(status.code().unwrap_or(1));
  }

  Ok(())
}

/// Build the inner `cargo rail test ...` command string for watch integrations.
fn format_watch_test_command(config: &TestConfig) -> String {
  let mut rail_cmd = String::from("rail test");

  if let Some(ref since) = config.since {
    rail_cmd.push_str(&format!(" --since {}", since));
  }

  if config.full {
    rail_cmd.push_str(" --full");
  }

  if !config.prefer_nextest {
    rail_cmd.push_str(" --no-nextest");
  }

  if !config.test_args.is_empty() {
    rail_cmd.push_str(" -- ");
    rail_cmd.push_str(&config.test_args.join(" "));
  }

  rail_cmd
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_watch_mode_names() {
    assert_eq!(WatchMode::Bacon.name(), "bacon");
    assert_eq!(WatchMode::CargoWatch.name(), "cargo-watch");
    assert_eq!(WatchMode::Auto.name(), "auto-detect");
  }

  #[test]
  fn test_auto_always_available() {
    assert!(WatchMode::Auto.is_available());
  }

  #[test]
  fn test_select_watch_mode() {
    // Auto should select something available or return error
    let result = select_watch_mode(WatchMode::Auto);

    // Either we get a valid mode (bacon or cargo-watch is installed)
    // Or we get an error (neither is installed)
    match result {
      Ok(mode) => {
        assert!(mode != WatchMode::Auto, "Should resolve to concrete mode");
        assert!(mode.is_available(), "Selected mode should be available");
      }
      Err(_) => {
        // Neither bacon nor cargo-watch is installed - that's okay for tests
        assert!(!WatchMode::Bacon.is_available());
        assert!(!WatchMode::CargoWatch.is_available());
      }
    }
  }

  #[test]
  fn test_format_watch_test_command_includes_flags_and_args() {
    let cfg = TestConfig {
      since: Some("main".to_string()),
      full: false,
      explain: false,
      prefer_nextest: true,
      test_args: vec!["--nocapture".into(), "some::test".into()],
    };

    let cmd = format_watch_test_command(&cfg);
    assert!(cmd.starts_with("rail test"));
    assert!(cmd.contains("--since main"));
    assert!(!cmd.contains("--no-nextest"));
    assert!(cmd.contains("-- --nocapture some::test"));
  }

  #[test]
  fn test_format_watch_test_command_no_nextest() {
    let cfg = TestConfig {
      since: None,
      full: true,
      explain: false,
      prefer_nextest: false,
      test_args: vec![],
    };

    let cmd = format_watch_test_command(&cfg);
    assert!(cmd.contains("--full"));
    assert!(cmd.contains("--no-nextest"));
  }
}
