//! Release preparation: version bumping and changelog generation

use crate::core::error::RailResult;

/// Run release prepare command
pub fn run_release_prepare(
    _crate_name: Option<&str>,
    _apply: bool,
    _no_changelog: bool,
) -> RailResult<()> {
    println!("🚧 Release prepare command (coming soon)");
    Ok(())
}
