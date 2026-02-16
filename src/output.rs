//! Centralized output control for CLI commands.
//!
//! Provides consistent, ergonomic output handling with quiet mode support.
//!
//! # Categories
//!
//! **Critical** (always shown):
//! - [`error!`] - Error messages: `error: something went wrong`
//! - [`warn!`] - Warnings: `warning: deprecated option`
//! - [`help!`] - Help hints: `help: try --force`
//!
//! **Informational** (suppressed with `--quiet`):
//! - [`status!`] - Progress messages (no prefix)
//! - [`note!`] - Notes: `note: config found at /path`

use std::sync::atomic::{AtomicBool, Ordering};

// Global State

static QUIET: AtomicBool = AtomicBool::new(false);
static JSON_MODE: AtomicBool = AtomicBool::new(false);

/// Initialize output settings. Call once at startup.
#[doc(hidden)]
pub fn init(quiet: bool) {
  QUIET.store(quiet, Ordering::Relaxed);
}

/// Check if quiet mode is enabled.
pub fn is_quiet() -> bool {
  QUIET.load(Ordering::Relaxed)
}

/// Check if JSON mode is enabled.
pub fn is_json_mode() -> bool {
  JSON_MODE.load(Ordering::Relaxed)
}

/// Enable JSON mode (automatically enables quiet mode).
#[doc(hidden)]
pub fn set_json_mode(json: bool) {
  JSON_MODE.store(json, Ordering::Relaxed);
  if json {
    QUIET.store(true, Ordering::Relaxed);
  }
}

// Critical Output (always shown)

/// Print an error message to stderr.
///
/// Always shown, even in quiet mode. Adds `error: ` prefix.
///
/// ```no_run
/// # fn main() {
/// cargo_rail::error!("failed to read file");
/// // Output: error: failed to read file
/// # }
/// ```
#[macro_export]
macro_rules! error {
  ($($arg:tt)*) => {
    eprintln!("error: {}", format_args!($($arg)*))
  };
}

/// Print a warning message to stderr.
///
/// Always shown, even in quiet mode. Adds `warning: ` prefix.
///
/// ```no_run
/// # fn main() {
/// cargo_rail::warn!("deprecated option will be removed in v2.0");
/// // Output: warning: deprecated option will be removed in v2.0
/// # }
/// ```
#[macro_export]
macro_rules! warn {
  ($($arg:tt)*) => {
    eprintln!("warning: {}", format_args!($($arg)*))
  };
}

/// Print a help hint to stderr.
///
/// Always shown, even in quiet mode. Adds `help: ` prefix.
/// Typically used after an error to suggest a fix.
///
/// ```no_run
/// # fn main() {
/// cargo_rail::error!("missing required argument");
/// cargo_rail::help!("run with --help for usage");
/// // Output:
/// // error: missing required argument
/// // help: run with --help for usage
/// # }
/// ```
#[macro_export]
macro_rules! help {
  ($($arg:tt)*) => {
    eprintln!("help: {}", format_args!($($arg)*))
  };
}

/// Print a status/progress message to stderr.
///
/// Suppressed in quiet mode. No prefix added.
/// Use for transient progress info like "analyzing...", "writing files...".
///
/// ```no_run
/// # fn main() {
/// # let crates = vec![1, 2, 3];
/// cargo_rail::status!("analyzing {} crates...", crates.len());
/// // Output: analyzing 3 crates...
/// # }
/// ```
#[macro_export]
macro_rules! status {
  ($($arg:tt)*) => {
    if !$crate::output::is_quiet() {
      eprintln!($($arg)*)
    }
  };
}

/// Print a note to stderr.
///
/// Suppressed in quiet mode. Adds `note: ` prefix.
/// Use for non-critical informational messages.
///
/// ```no_run
/// # fn main() {
/// # let path = std::path::Path::new("/project/.config/rail.toml");
/// cargo_rail::note!("existing config found at {}", path.display());
/// // Output: note: existing config found at /project/.config/rail.toml
/// # }
/// ```
#[macro_export]
macro_rules! note {
  ($($arg:tt)*) => {
    if !$crate::output::is_quiet() {
      eprintln!("note: {}", format_args!($($arg)*))
    }
  };
}

/// Alias for [`status!`]. Use whichever reads better in context.
#[macro_export]
macro_rules! progress {
  ($($arg:tt)*) => {
    $crate::status!($($arg)*)
  };
}
