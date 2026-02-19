//! Test runner supporting cargo test and cargo-nextest
//!
//! Provides a unified interface for running tests with automatic detection
//! and fallback behavior.

use std::process::Command;

fn cargo_command() -> Command {
  Command::new("cargo")
}

/// Test runner variants
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestRunner {
  /// Standard cargo test
  CargoTest,
  /// cargo-nextest (faster test runner)
  Nextest,
}

impl TestRunner {
  /// Get the name of this test runner
  pub fn name(&self) -> &str {
    match self {
      Self::CargoTest => "cargo test",
      Self::Nextest => "cargo nextest",
    }
  }

  /// Check if this test runner is available in the environment
  pub fn is_available(&self) -> bool {
    match self {
      Self::CargoTest => true, // cargo is always available
      Self::Nextest => cargo_command()
        .arg("nextest")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false),
    }
  }

  /// Build a command to run tests for the given packages
  pub fn build_command(&self, packages: &[String], args: &[String]) -> Command {
    let mut cmd = cargo_command();

    match self {
      Self::CargoTest => {
        cmd.arg("test");

        // Add package filters for each affected crate
        for pkg in packages {
          cmd.arg("-p").arg(pkg);
        }

        // Add user-provided test arguments
        if !args.is_empty() {
          cmd.arg("--");
          cmd.args(args);
        }
      }
      Self::Nextest => {
        cmd.arg("nextest").arg("run");

        // Add package filters for each affected crate
        for pkg in packages {
          cmd.arg("-p").arg(pkg);
        }

        // Add user-provided test arguments
        cmd.args(args);
      }
    }

    cmd
  }
}

/// Select the appropriate test runner based on preferences and availability
///
/// If prefer_nextest is true and nextest is available, use it.
/// Otherwise, fall back to cargo test.
pub fn select_runner(prefer_nextest: bool) -> TestRunner {
  if prefer_nextest && TestRunner::Nextest.is_available() {
    TestRunner::Nextest
  } else {
    TestRunner::CargoTest
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_cargo_test_runner_always_available() {
    let runner = TestRunner::CargoTest;
    assert!(runner.is_available());
    assert_eq!(runner.name(), "cargo test");
  }

  #[test]
  fn test_cargo_test_command_building() {
    let runner = TestRunner::CargoTest;
    let packages = vec!["crate-a".to_string(), "crate-b".to_string()];
    let args = vec!["--nocapture".to_string()];

    let cmd = runner.build_command(&packages, &args);

    // Convert command to string for inspection
    let cmd_str = format!("{:?}", cmd);
    assert!(cmd_str.contains("cargo"));
    assert!(cmd_str.contains("test"));
  }

  #[test]
  fn test_nextest_runner_name() {
    let runner = TestRunner::Nextest;
    assert_eq!(runner.name(), "cargo nextest");
  }

  #[test]
  fn test_select_runner_fallback() {
    // Should always return a valid runner
    let runner = select_runner(false);
    assert_eq!(runner, TestRunner::CargoTest);
  }

  #[test]
  fn test_select_runner_with_nextest_preference() {
    let runner = select_runner(true);
    // Should return either nextest (if available) or cargo test (fallback)
    assert!(
      runner == TestRunner::Nextest || runner == TestRunner::CargoTest,
      "Runner should be either nextest or cargo test"
    );
  }
}
