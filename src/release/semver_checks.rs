//! External `cargo-semver-checks` integration.
//!
//! The binary is optional and invoked through `cargo semver-checks`; cargo-rail
//! never vendors the semver checker as a dependency.
//!
//! Exit status alone cannot distinguish "breaking API change found" from
//! "the check could not run" (no published baseline, network failure, rustdoc
//! build error) — cargo-semver-checks uses non-zero for both. A crate's very
//! first release has no baseline and must never be escalated to a major bump
//! because of it, so classification requires the breaking-summary marker in
//! the output, and everything else non-zero is [`SemverCheck::Inconclusive`].

use crate::error::{RailError, RailResult};
use crate::release::process;
use crate::workspace::WorkspaceContext;
use std::path::Path;

/// The summary line cargo-semver-checks prints when breaking changes require
/// a major version bump.
const BREAKING_MARKER: &str = "semver requires new major version";

/// Outcome of checking one crate against its previous public API.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SemverCheck {
  /// No semver-breaking API changes detected.
  Pass,
  /// Breaking API changes require a major version bump.
  Breaking {
    /// Human-readable summary from the checker.
    message: String,
  },
  /// The check could not produce a verdict (missing baseline, network or
  /// build failure). Never escalates bumps and never fails a release.
  Inconclusive {
    /// Why no verdict was possible.
    message: String,
  },
}

/// Whether the optional `cargo-semver-checks` subcommand is installed.
pub(crate) fn is_available(workspace_root: &Path) -> bool {
  process::succeeds("cargo", &["semver-checks", "--version"], Some(workspace_root))
}

/// Whether a workspace package has a library-like public API target.
pub(crate) fn has_library_target(ctx: &WorkspaceContext, crate_name: &str) -> bool {
  let Some(package) = ctx.cargo().get_package(crate_name) else {
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
pub(crate) fn check_release(ctx: &WorkspaceContext, crate_name: &str) -> RailResult<SemverCheck> {
  let package = ctx
    .cargo()
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

  Ok(classify_output(
    output.status.success(),
    &String::from_utf8_lossy(&output.stdout),
    &String::from_utf8_lossy(&output.stderr),
  ))
}

/// Classify checker output into a verdict.
fn classify_output(success: bool, stdout: &str, stderr: &str) -> SemverCheck {
  if success {
    return SemverCheck::Pass;
  }

  if let Some(line) = marker_line(stdout).or_else(|| marker_line(stderr)) {
    return SemverCheck::Breaking { message: line };
  }

  SemverCheck::Inconclusive {
    message: first_message(stderr, stdout),
  }
}

fn marker_line(text: &str) -> Option<String> {
  text
    .lines()
    .find(|line| line.contains(BREAKING_MARKER))
    .map(|line| line.trim().to_string())
}

fn first_message(stderr: &str, stdout: &str) -> String {
  for line in stderr.lines().chain(stdout.lines()) {
    let line = line.trim();
    if !line.is_empty() {
      return line.to_string();
    }
  }
  "cargo-semver-checks exited without output".to_string()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn success_is_pass() {
    assert_eq!(classify_output(true, "", ""), SemverCheck::Pass);
  }

  #[test]
  fn breaking_marker_is_breaking() {
    let stdout = "Checking lib-a v1.2.3\nSummary semver requires new major version: 1 major check failed\n";
    let verdict = classify_output(false, stdout, "");
    assert_eq!(
      verdict,
      SemverCheck::Breaking {
        message: "Summary semver requires new major version: 1 major check failed".to_string(),
      }
    );
  }

  #[test]
  fn marker_on_stderr_is_breaking() {
    let stderr = "Summary semver requires new major version: 2 major checks failed";
    assert!(matches!(
      classify_output(false, "", stderr),
      SemverCheck::Breaking { .. }
    ));
  }

  #[test]
  fn nonzero_without_marker_is_inconclusive() {
    // First release: no published baseline on crates.io.
    let stderr = "error: the crate lib-a has no published versions to use as a baseline\n";
    let verdict = classify_output(false, "", stderr);
    assert_eq!(
      verdict,
      SemverCheck::Inconclusive {
        message: "error: the crate lib-a has no published versions to use as a baseline".to_string(),
      }
    );
  }

  #[test]
  fn silent_failure_is_inconclusive_with_note() {
    assert_eq!(
      classify_output(false, "", ""),
      SemverCheck::Inconclusive {
        message: "cargo-semver-checks exited without output".to_string(),
      }
    );
  }
}
