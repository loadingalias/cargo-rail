//! Publishing crates to crates.io in topological order

use crate::core::error::RailResult;

/// Run release publish command
pub fn run_release_publish(
  _crate_name: Option<&str>,
  _apply: bool,
  _yes: bool,
  _delay: u64,
  _dry_run: bool,
) -> RailResult<()> {
  println!("🚧 Release publish command (coming soon)");
  Ok(())
}
