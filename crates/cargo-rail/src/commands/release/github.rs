//! GitHub release creation via gh CLI

use crate::core::error::RailResult;

/// Create GitHub release for a crate
pub fn create_github_release(_crate_name: &str, _version: &str, _changelog: &str) -> RailResult<()> {
  // TODO: Implement gh CLI integration
  Ok(())
}
