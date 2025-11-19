//! Test runner abstraction supporting cargo test and cargo-nextest
//!
//! Provides a unified interface for running tests across different test runners,
//! with automatic detection and fallback behavior.

use std::process::Command;

/// Trait for test runners (cargo test, cargo-nextest, etc.)
pub trait TestRunner {
  /// Get the name of this test runner
  fn name(&self) -> &str;

  /// Check if this test runner is available in the environment
  fn is_available(&self) -> bool;

  /// Build a command to run tests for the given packages
  fn build_command(&self, packages: &[String], args: &[String]) -> Command;
}

/// Standard cargo test runner
pub struct CargoTestRunner;

impl TestRunner for CargoTestRunner {
  fn name(&self) -> &str {
    "cargo test"
  }

  fn is_available(&self) -> bool {
    // cargo is always available if we got here
    true
  }

  fn build_command(&self, packages: &[String], args: &[String]) -> Command {
    let mut cmd = Command::new("cargo");
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

    cmd
  }
}

/// cargo-nextest test runner
pub struct NextestRunner;

impl TestRunner for NextestRunner {
  fn name(&self) -> &str {
    "cargo nextest"
  }

  fn is_available(&self) -> bool {
    Command::new("cargo")
      .arg("nextest")
      .arg("--version")
      .output()
      .map(|o| o.status.success())
      .unwrap_or(false)
  }

  fn build_command(&self, packages: &[String], args: &[String]) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.arg("nextest").arg("run");

    // Add package filters for each affected crate
    for pkg in packages {
      cmd.arg("-p").arg(pkg);
    }

    // Add user-provided test arguments
    cmd.args(args);

    cmd
  }
}

/// Select the appropriate test runner based on preferences and availability
///
/// If prefer_nextest is true and nextest is available, use it.
/// Otherwise, fall back to cargo test.
pub fn select_runner(prefer_nextest: bool) -> Box<dyn TestRunner> {
  if prefer_nextest {
    let nextest = NextestRunner;
    if nextest.is_available() {
      return Box::new(nextest);
    }
  }

  Box::new(CargoTestRunner)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_cargo_test_runner_always_available() {
    let runner = CargoTestRunner;
    assert!(runner.is_available());
    assert_eq!(runner.name(), "cargo test");
  }

  #[test]
  fn test_cargo_test_command_building() {
    let runner = CargoTestRunner;
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
    let runner = NextestRunner;
    assert_eq!(runner.name(), "cargo nextest");
  }

  #[test]
  fn test_select_runner_fallback() {
    // Should always return a valid runner
    let runner = select_runner(false);
    assert_eq!(runner.name(), "cargo test");
  }

  #[test]
  fn test_select_runner_with_nextest_preference() {
    let runner = select_runner(true);
    // Should return either nextest (if available) or cargo test (fallback)
    assert!(
      runner.name() == "cargo nextest" || runner.name() == "cargo test",
      "Runner should be either nextest or cargo test"
    );
  }
}
