//! Git tagging and post-release sync

use crate::core::error::RailResult;

/// Run release finalize command
pub fn run_release_finalize(
  _crate_name: Option<&str>,
  _apply: bool,
  _no_github: bool,
  _no_sync: bool,
) -> RailResult<()> {
  println!("🚧 Release finalize command (coming soon)");
  Ok(())
}
