//! Workspace-only rustc wrapper for unused-dependency diagnostics.
//!
//! Cargo invokes this mode through `RUSTC_WORKSPACE_WRAPPER`, so the lint is
//! applied only to workspace members. Third-party dependency fingerprints and
//! compiler-cache keys remain untouched.

use std::ffi::OsString;
use std::process::Command;

/// Marker set by the diagnostics collector when this executable is acting as a
/// rustc workspace wrapper.
pub const WRAPPER_MARKER: &str = "CARGO_RAIL_RUSTC_WRAPPER";

/// Existing workspace wrapper saved by the collector for transparent chaining.
pub const INNER_WRAPPER_ENV: &str = "CARGO_RAIL_INNER_WORKSPACE_WRAPPER";

/// Run rustc wrapper mode when requested by the diagnostics collector.
///
/// Returns `None` during normal cargo-rail CLI execution.
#[must_use]
pub fn run_if_requested() -> Option<i32> {
  std::env::var_os(WRAPPER_MARKER)?;
  Some(run())
}

fn run() -> i32 {
  let mut args = std::env::args_os().skip(1);
  let Some(rustc) = args.next() else {
    eprintln!("cargo-rail rustc wrapper: missing rustc executable");
    return 1;
  };

  let remaining: Vec<OsString> = args.collect();
  let inner_wrapper = std::env::var_os(INNER_WRAPPER_ENV);
  let mut command = match inner_wrapper {
    Some(wrapper) => {
      let mut command = Command::new(wrapper);
      command.arg(rustc);
      command
    }
    None => Command::new(rustc),
  };

  let status = command
    .args(remaining)
    .arg("--warn=unused-crate-dependencies")
    .env_remove(WRAPPER_MARKER)
    .env_remove(INNER_WRAPPER_ENV)
    .status();

  match status {
    Ok(status) => status.code().unwrap_or(1),
    Err(error) => {
      eprintln!("cargo-rail rustc wrapper: failed to execute compiler: {error}");
      1
    }
  }
}
