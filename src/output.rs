//! Centralized output control for CLI commands.
//!
//! Provides quiet mode support and consistent output handling across all commands.
//! When quiet mode is enabled, progress messages are suppressed.
//! JSON mode automatically enables quiet for stderr to prevent contamination.

use std::sync::atomic::{AtomicBool, Ordering};

/// Global quiet mode flag
static QUIET: AtomicBool = AtomicBool::new(false);

/// Global JSON mode flag (for structured error output)
static JSON_MODE: AtomicBool = AtomicBool::new(false);

/// Initialize output settings
///
/// Call this once at startup with the CLI quiet flag value.
pub fn init(quiet: bool) {
  QUIET.store(quiet, Ordering::Relaxed);
}

/// Check if quiet mode is enabled
pub fn is_quiet() -> bool {
  QUIET.load(Ordering::Relaxed)
}

/// Enable quiet mode (useful for JSON output)
pub fn set_quiet(quiet: bool) {
  QUIET.store(quiet, Ordering::Relaxed);
}

/// Check if JSON mode is enabled (for structured error output)
pub fn is_json_mode() -> bool {
  JSON_MODE.load(Ordering::Relaxed)
}

/// Enable JSON mode (enables quiet mode automatically)
///
/// When JSON mode is enabled:
/// - Progress messages are suppressed (quiet mode)
/// - Errors are output as structured JSON
pub fn set_json_mode(json: bool) {
  JSON_MODE.store(json, Ordering::Relaxed);
  if json {
    // JSON mode implies quiet mode for clean output
    QUIET.store(true, Ordering::Relaxed);
  }
}

/// Print a progress/status message to stderr (suppressed in quiet mode)
///
/// Use this for transient progress information like "creating backup...",
/// "writing files...", etc.
#[macro_export]
macro_rules! progress {
    ($($arg:tt)*) => {
        if !$crate::output::is_quiet() {
            eprintln!($($arg)*);
        }
    };
}

/// Print a status message to stderr (suppressed in quiet mode)
///
/// Alias for `progress!` - use whichever reads better in context.
#[macro_export]
macro_rules! status {
    ($($arg:tt)*) => {
        $crate::progress!($($arg)*);
    };
}

/// Print a warning to stderr (NOT suppressed in quiet mode)
///
/// Warnings are important enough to show even in quiet mode.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        eprintln!($($arg)*);
    };
}
