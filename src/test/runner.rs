//! Test runner supporting cargo test and cargo-nextest
//!
//! Provides a unified interface for running tests with automatic detection
//! and fallback behavior.

use std::process::Command;

use clap::ValueEnum;
use serde::Serialize;

use crate::error::{RailError, RailResult};

fn cargo_command() -> Command {
  Command::new("cargo")
}

/// User preference for the test execution backend.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum TestRunnerPreference {
  /// Prefer nextest when installed and otherwise use Cargo.
  #[default]
  Auto,
  /// Require `cargo test`.
  Cargo,
  /// Require `cargo nextest run`.
  Nextest,
}

/// Structured arguments for one test invocation.
///
/// Backend options are deliberately separate. The filter and harness arguments
/// have equivalent positions in both Cargo and nextest command lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestCommandArgs {
  /// Options understood only by `cargo test`.
  pub cargo: Vec<String>,
  /// Options understood only by `cargo nextest run`.
  pub nextest: Vec<String>,
  /// Optional portable test-name filter.
  pub filter: Option<String>,
  /// Arguments forwarded to the selected test binary after `--`.
  pub harness: Vec<String>,
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
  /// Human-readable backend command name.
  pub fn name(&self) -> &'static str {
    match self {
      Self::CargoTest => "cargo test",
      Self::Nextest => "cargo nextest",
    }
  }

  /// Return whether the backend is available in the current environment.
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

  /// Build a command to run tests for the given packages.
  pub fn build_command(&self, packages: &[String], args: &TestCommandArgs) -> RailResult<Command> {
    let mut cmd = cargo_command();
    let (before_packages, after_packages) = self.command_argv_parts(args)?;
    cmd.args(before_packages);

    for pkg in packages {
      cmd.arg("-p").arg(pkg);
    }
    cmd.args(after_packages);

    Ok(cmd)
  }

  pub(crate) fn command_argv_parts(&self, args: &TestCommandArgs) -> RailResult<(Vec<String>, Vec<String>)> {
    let before_packages = match self {
      Self::CargoTest => {
        if !args.nextest.is_empty() {
          return Err(backend_argument_error("nextest", self.name()));
        }
        vec!["test".to_string()]
      }
      Self::Nextest => {
        if !args.cargo.is_empty() {
          return Err(backend_argument_error("Cargo test", self.name()));
        }
        vec!["nextest".to_string(), "run".to_string()]
      }
    };

    let mut after_packages = match self {
      Self::CargoTest => args.cargo.clone(),
      Self::Nextest => args.nextest.clone(),
    };
    after_packages.extend(args.filter.iter().cloned());
    if !args.harness.is_empty() {
      after_packages.push("--".to_string());
      after_packages.extend(args.harness.iter().cloned());
    }

    Ok((before_packages, after_packages))
  }
}

fn backend_argument_error(argument_backend: &str, selected_backend: &str) -> RailError {
  RailError::with_help(
    format!("{} options cannot be used with {}", argument_backend, selected_backend),
    "select the matching --test-runner or remove the backend-specific options",
  )
}

/// Select the test runner without silently reinterpreting backend arguments.
pub fn select_runner(preference: TestRunnerPreference, args: &TestCommandArgs) -> RailResult<TestRunner> {
  resolve_runner(preference, args, TestRunner::Nextest.is_available())
}

fn resolve_runner(
  preference: TestRunnerPreference,
  args: &TestCommandArgs,
  nextest_available: bool,
) -> RailResult<TestRunner> {
  if !args.cargo.is_empty() && !args.nextest.is_empty() {
    return Err(RailError::with_help(
      "Cargo test and nextest options cannot be combined",
      "select one backend and pass options only for that backend",
    ));
  }

  match preference {
    TestRunnerPreference::Cargo => {
      if !args.nextest.is_empty() {
        return Err(backend_argument_error("nextest", TestRunner::CargoTest.name()));
      }
      Ok(TestRunner::CargoTest)
    }
    TestRunnerPreference::Nextest => {
      if !args.cargo.is_empty() {
        return Err(backend_argument_error("Cargo test", TestRunner::Nextest.name()));
      }
      require_nextest(nextest_available)
    }
    TestRunnerPreference::Auto if !args.cargo.is_empty() => Ok(TestRunner::CargoTest),
    TestRunnerPreference::Auto if !args.nextest.is_empty() => require_nextest(nextest_available),
    TestRunnerPreference::Auto if nextest_available => Ok(TestRunner::Nextest),
    TestRunnerPreference::Auto => Ok(TestRunner::CargoTest),
  }
}

fn require_nextest(available: bool) -> RailResult<TestRunner> {
  if available {
    Ok(TestRunner::Nextest)
  } else {
    Err(RailError::with_help(
      "cargo-nextest is required by the selected test options but is not available",
      "install cargo-nextest or select --test-runner cargo",
    ))
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
    let args = TestCommandArgs {
      cargo: vec!["--all-features".to_string()],
      filter: Some("selected_test".to_string()),
      harness: vec!["--nocapture".to_string(), "--test-threads=1".to_string()],
      ..TestCommandArgs::default()
    };

    let cmd = runner
      .build_command(&packages, &args)
      .expect("Cargo arguments should render");

    assert_eq!(cmd.get_program(), "cargo");
    assert_eq!(
      command_args(&cmd),
      [
        "test",
        "-p",
        "crate-a",
        "-p",
        "crate-b",
        "--all-features",
        "selected_test",
        "--",
        "--nocapture",
        "--test-threads=1",
      ]
    );
  }

  #[test]
  fn test_nextest_runner_name() {
    let runner = TestRunner::Nextest;
    assert_eq!(runner.name(), "cargo nextest");
  }

  #[test]
  fn test_nextest_command_building() {
    let runner = TestRunner::Nextest;
    let packages = vec!["crate-a".to_string()];
    let args = TestCommandArgs {
      nextest: vec!["-P".to_string(), "commit".to_string()],
      filter: Some("selected_test".to_string()),
      harness: vec!["--nocapture".to_string()],
      ..TestCommandArgs::default()
    };

    let cmd = runner
      .build_command(&packages, &args)
      .expect("nextest arguments should render");

    assert_eq!(cmd.get_program(), "cargo");
    assert_eq!(
      command_args(&cmd),
      [
        "nextest",
        "run",
        "-p",
        "crate-a",
        "-P",
        "commit",
        "selected_test",
        "--",
        "--nocapture",
      ]
    );
  }

  #[test]
  fn test_auto_selection_never_reinterprets_backend_options() {
    let cargo_args = TestCommandArgs {
      cargo: vec!["--all-features".to_string()],
      ..TestCommandArgs::default()
    };
    let nextest_args = TestCommandArgs {
      nextest: vec!["-P".to_string(), "commit".to_string()],
      ..TestCommandArgs::default()
    };

    assert_eq!(
      resolve_runner(TestRunnerPreference::Auto, &cargo_args, true).expect("Cargo options select Cargo"),
      TestRunner::CargoTest
    );
    assert_eq!(
      resolve_runner(TestRunnerPreference::Auto, &nextest_args, true).expect("nextest is available"),
      TestRunner::Nextest
    );
    let error = resolve_runner(TestRunnerPreference::Auto, &nextest_args, false)
      .expect_err("nextest options must not fall back to Cargo");
    assert!(error.to_string().contains("cargo-nextest is required"));
  }

  #[test]
  fn test_selection_rejects_mixed_or_mismatched_backend_options() {
    let mixed = TestCommandArgs {
      cargo: vec!["--all-features".to_string()],
      nextest: vec!["-P".to_string(), "commit".to_string()],
      ..TestCommandArgs::default()
    };
    let nextest_only = TestCommandArgs {
      nextest: vec!["-P".to_string(), "commit".to_string()],
      ..TestCommandArgs::default()
    };

    let mixed_error =
      resolve_runner(TestRunnerPreference::Auto, &mixed, true).expect_err("mixed backend options must be rejected");
    assert!(mixed_error.to_string().contains("cannot be combined"));

    let mismatch_error = resolve_runner(TestRunnerPreference::Cargo, &nextest_only, true)
      .expect_err("nextest options must not be rendered by Cargo");
    assert!(
      mismatch_error
        .to_string()
        .contains("nextest options cannot be used with cargo test")
    );
  }

  fn command_args(command: &Command) -> Vec<String> {
    command
      .get_args()
      .map(|arg| arg.to_string_lossy().into_owned())
      .collect()
  }
}
