//! Shared subprocess helpers for release flows.
//!
//! Centralizes process execution for cargo/gh/rustup checks and actions so
//! release code paths behave consistently.

use crate::error::{RailError, RailResult};
use std::path::Path;
use std::process::{Command, Output};

fn format_command(program: &str, args: &[&str]) -> String {
  if args.is_empty() {
    program.to_string()
  } else {
    format!("{} {}", program, args.join(" "))
  }
}

/// Run a command and map spawn failures into `RailError`.
pub(crate) fn run(program: &str, args: &[&str], current_dir: Option<&Path>) -> RailResult<Output> {
  let mut cmd = Command::new(program);
  if let Some(dir) = current_dir {
    cmd.current_dir(dir);
  }
  cmd
    .args(args)
    .output()
    .map_err(|e| RailError::message(format!("Failed to run {}: {}", format_command(program, args), e)))
}

/// Return true if command invocation succeeds with a zero exit status.
pub(crate) fn succeeds(program: &str, args: &[&str], current_dir: Option<&Path>) -> bool {
  run(program, args, current_dir)
    .map(|o| o.status.success())
    .unwrap_or(false)
}
