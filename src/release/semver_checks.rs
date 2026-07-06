//! External `cargo-semver-checks` integration.
//!
//! The binary is optional and invoked through `cargo semver-checks`; cargo-rail
//! never vendors the semver checker as a dependency.

use crate::error::{RailError, RailResult};
use crate::release::process;
use crate::workspace::WorkspaceContext;
use std::path::Path;

/// Result of checking one crate against its previous public API.
#[derive(Debug, Clone)]
pub(crate) struct SemverCheckOutcome {
  /// Whether cargo-semver-checks reported semver-breaking API changes.
  pub breaking: bool,
  /// Human-readable summary from the checker.
  pub message: String,
}

/// Whether the optional `cargo-semver-checks` subcommand is installed.
pub(crate) fn is_available(workspace_root: &Path) -> bool {
  process::succeeds("cargo", &["semver-checks", "--version"], Some(workspace_root))
}

/// Whether a workspace package has a library-like public API target.
pub(crate) fn has_library_target(ctx: &WorkspaceContext, crate_name: &str) -> bool {
  let Some(package) = ctx.cargo.get_package(crate_name) else {
    return false;
  };

  package.targets.iter().any(|target| {
    target.kind.iter().any(|kind| {
      matches!(
        kind,
        cargo_metadata::TargetKind::Lib
          | cargo_metadata::TargetKind::RLib
          | cargo_metadata::TargetKind::DyLib
          | cargo_metadata::TargetKind::CDyLib
          | cargo_metadata::TargetKind::StaticLib
          | cargo_metadata::TargetKind::ProcMacro
      )
    })
  })
}

/// Run `cargo semver-checks check-release` for one crate.
pub(crate) fn check_release(ctx: &WorkspaceContext, crate_name: &str) -> RailResult<SemverCheckOutcome> {
  let package = ctx
    .cargo
    .get_package(crate_name)
    .ok_or_else(|| RailError::message(format!("crate '{}' not found", crate_name)))?;

  let manifest_path = package.manifest_path.as_std_path();
  let Some(manifest) = manifest_path.to_str() else {
    return Err(RailError::message("manifest path is not valid UTF-8"));
  };

  let output = process::run(
    "cargo",
    &["semver-checks", "check-release", "--manifest-path", manifest],
    Some(ctx.workspace_root()),
  )?;

  if output.status.success() {
    return Ok(SemverCheckOutcome {
      breaking: false,
      message: "no semver-breaking API changes detected".to_string(),
    });
  }

  Ok(SemverCheckOutcome {
    breaking: true,
    message: first_message(&output.stderr, &output.stdout),
  })
}

fn first_message(stderr: &[u8], stdout: &[u8]) -> String {
  let stderr = String::from_utf8_lossy(stderr);
  let stdout = String::from_utf8_lossy(stdout);
  for line in stderr.lines().chain(stdout.lines()) {
    let line = line.trim();
    if !line.is_empty() {
      return line.to_string();
    }
  }
  "cargo-semver-checks reported API changes".to_string()
}
