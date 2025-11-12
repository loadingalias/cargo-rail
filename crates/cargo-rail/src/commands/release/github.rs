//! GitHub release creation via gh CLI
//!
//! Planned for `cargo rail release finalize` command.
//! Will use `gh` CLI to create GitHub releases with changelog as release notes.

use crate::core::error::RailResult;

/// Create GitHub release for a crate
///
/// TODO (Phase 1.1): Implement gh CLI integration
/// - Create GitHub releases via `gh release create`
/// - Attach changelog as release notes
/// - Support custom release templates
#[allow(dead_code)]
pub fn create_github_release(_crate_name: &str, _version: &str, _changelog: &str) -> RailResult<()> {
  Ok(())
}
