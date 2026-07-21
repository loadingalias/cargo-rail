//! Workspace-only rustc wrapper for unused-dependency diagnostics.
//!
//! Cargo invokes this mode through `RUSTC_WORKSPACE_WRAPPER`, so the lint is
//! applied only to workspace members. Third-party dependency fingerprints and
//! compiler-cache keys remain untouched.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

/// Marker set by the diagnostics collector when this executable is acting as a
/// rustc workspace wrapper.
pub const WRAPPER_MARKER: &str = "CARGO_RAIL_RUSTC_WRAPPER";

/// Existing workspace wrapper saved by the collector for transparent chaining.
pub const INNER_WRAPPER_ENV: &str = "CARGO_RAIL_INNER_WORKSPACE_WRAPPER";

/// Private directory where diagnostics wrappers publish immutable invocation evidence.
pub const OBSERVATION_DIRECTORY_ENV: &str = "CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY";

/// Physical source root used only to normalize and revalidate observation paths.
pub const OBSERVATION_SOURCE_ROOT_ENV: &str = "CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT";

/// Compose Cargo's stable wrapper order: global wrapper, workspace wrapper, rustc.
pub(crate) fn rustc_command(
  rustc: &std::ffi::OsStr,
  rustc_wrapper: Option<&std::ffi::OsStr>,
  workspace_wrapper: Option<&std::ffi::OsStr>,
) -> Command {
  match (rustc_wrapper, workspace_wrapper) {
    (Some(wrapper), Some(workspace_wrapper)) => {
      let mut command = Command::new(wrapper);
      command.arg(workspace_wrapper).arg(rustc);
      command
    }
    (Some(wrapper), None) => {
      let mut command = Command::new(wrapper);
      command.arg(rustc);
      command
    }
    (None, Some(workspace_wrapper)) => {
      let mut command = Command::new(workspace_wrapper);
      command.arg(rustc);
      command
    }
    (None, None) => Command::new(rustc),
  }
}

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
  let recorder = std::env::var_os(OBSERVATION_DIRECTORY_ENV)
    .zip(std::env::var_os(OBSERVATION_SOURCE_ROOT_ENV))
    .and_then(|(directory, source_root)| {
      crate::compiler::observation::begin_invocation(
        &PathBuf::from(directory),
        &PathBuf::from(source_root),
        &rustc,
        &remaining,
      )
      .ok()
    });
  let mut command = rustc_command(&rustc, None, inner_wrapper.as_deref());

  let status = command
    .args(remaining)
    .arg("--warn=unused-crate-dependencies")
    .env_remove(WRAPPER_MARKER)
    .env_remove(INNER_WRAPPER_ENV)
    .env_remove(OBSERVATION_DIRECTORY_ENV)
    .env_remove(OBSERVATION_SOURCE_ROOT_ENV)
    .status();

  if let Some(recorder) = recorder {
    let _ = recorder.finish(status.as_ref().is_ok_and(std::process::ExitStatus::success));
  }

  match status {
    Ok(status) => status.code().unwrap_or(1),
    Err(error) => {
      eprintln!("cargo-rail rustc wrapper: failed to execute compiler: {error}");
      1
    }
  }
}
