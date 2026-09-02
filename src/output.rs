//! Centralized output control for CLI commands.
//!
//! Provides consistent, ergonomic output handling with quiet mode support.
//!
//! # Categories
//!
//! **Critical** (always shown):
//! - [`error!`] - Error messages: `error: something went wrong`
//! - [`warn!`] - Warnings: `warning: configuration needs attention`
//! - [`help!`] - Help hints: `help: try --force`
//!
//! **Informational** (suppressed with `--quiet`):
//! - [`status!`] - Progress messages (no prefix)
//! - [`note!`] - Notes: `note: config found at /path`

use std::io::IsTerminal as _;
use std::io::Write as _;
use std::sync::OnceLock;

// Global State

static INVOCATION_OUTPUT: OnceLock<InvocationOutput> = OnceLock::new();

/// Transport selected once for the complete invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OutputProtocol {
    /// Human-readable stdout plus diagnostics on stderr.
    Text,
    /// Exactly one complete JSON value on stdout and no stderr.
    Json,
    /// Command-owned raw stdout stream with failures reported on stderr.
    Raw,
}

/// Immutable process-output context established immediately after Clap parsing.
#[derive(Debug)]
pub struct InvocationOutput {
    protocol: OutputProtocol,
    quiet: bool,
    verbose: bool,
    color: bool,
    stdout_terminal: bool,
    stderr_terminal: bool,
}

impl InvocationOutput {
    /// Capture transport and terminal state once for this process.
    pub fn capture(quiet: bool, verbose: bool, json: bool) -> Self {
        Self::capture_protocol(
            quiet,
            verbose,
            if json {
                OutputProtocol::Json
            } else {
                OutputProtocol::Text
            },
        )
    }

    /// Capture one explicitly selected transport and terminal state.
    #[doc(hidden)]
    pub fn capture_protocol(quiet: bool, verbose: bool, protocol: OutputProtocol) -> Self {
        let stdout_terminal = std::io::stdout().is_terminal();
        let stderr_terminal = std::io::stderr().is_terminal();
        let raw_or_json = protocol != OutputProtocol::Text;
        Self {
            protocol,
            quiet: quiet || raw_or_json,
            verbose: verbose && !raw_or_json,
            color: !raw_or_json && stderr_terminal && std::env::var_os("NO_COLOR").is_none(),
            stdout_terminal,
            stderr_terminal,
        }
    }

    /// Selected transport protocol.
    pub const fn protocol(&self) -> OutputProtocol {
        self.protocol
    }

    /// Whether stdout was a terminal at invocation start.
    pub const fn stdout_is_terminal(&self) -> bool {
        self.stdout_terminal
    }

    /// Whether stderr was a terminal at invocation start.
    pub const fn stderr_is_terminal(&self) -> bool {
        self.stderr_terminal
    }

    /// Whether bounded operational detail was requested.
    pub const fn verbose(&self) -> bool {
        self.verbose
    }

    /// Whether diagnostic color is permitted for this invocation.
    pub const fn color_enabled(&self) -> bool {
        self.color
    }
}

/// Stable schema version for machine-readable command output envelopes.
pub const MACHINE_OUTPUT_SCHEMA_VERSION: u32 = 1;

/// Install the immutable output context. Call exactly once at startup.
#[doc(hidden)]
pub fn init(output: InvocationOutput) {
    INVOCATION_OUTPUT
        .set(output)
        .expect("invocation output must be initialized exactly once");
}

fn invocation() -> &'static InvocationOutput {
    INVOCATION_OUTPUT.get_or_init(|| InvocationOutput::capture(false, false, false))
}

/// Check if quiet mode is enabled.
pub fn is_quiet() -> bool {
    invocation().quiet
}

/// Check if JSON mode is enabled.
pub fn is_json_mode() -> bool {
    invocation().protocol == OutputProtocol::Json
}

/// Check whether bounded operational detail was requested.
pub fn is_verbose() -> bool {
    invocation().verbose()
}

/// Check whether terminal-aware diagnostic color is permitted.
pub fn color_enabled() -> bool {
    invocation().color_enabled()
}

/// Write one human or machine stdout fragment without panicking on a closed pipe.
#[doc(hidden)]
pub fn write_stdout(arguments: std::fmt::Arguments<'_>, newline: bool) {
    let mut stdout = std::io::stdout().lock();
    let result = stdout
        .write_fmt(arguments)
        .and_then(|()| if newline { stdout.write_all(b"\n") } else { Ok(()) });
    if let Err(error) = result {
        if error.kind() == std::io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        if !is_json_mode()
            && let Err(_stderr_error) = writeln!(std::io::stderr().lock(), "error: failed writing stdout: {error}")
        {
        }
        std::process::exit(1);
    }
}

/// Build a stable machine-readable JSON envelope.
///
/// The returned object always contains:
/// - `schema_version`
/// - `command`
/// - `mode`
/// - `result`
/// - `exit_code`
///
/// If `payload` is an object, its keys are merged into the top-level envelope
/// without overriding existing standard keys. Non-object payloads are stored in
/// `payload`.
pub fn machine_json_envelope(
    command: &str,
    mode: &str,
    result: &str,
    exit_code: i32,
    payload: serde_json::Value,
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    out.insert(
        "schema_version".to_string(),
        serde_json::Value::Number(serde_json::Number::from(MACHINE_OUTPUT_SCHEMA_VERSION)),
    );
    out.insert("command".to_string(), serde_json::Value::String(command.to_string()));
    out.insert("mode".to_string(), serde_json::Value::String(mode.to_string()));
    out.insert("result".to_string(), serde_json::Value::String(result.to_string()));
    out.insert(
        "exit_code".to_string(),
        serde_json::Value::Number(serde_json::Number::from(exit_code)),
    );

    match payload {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if !out.contains_key(&key) {
                    out.insert(key, value);
                }
            }
        }
        other => {
            out.insert("payload".to_string(), other);
        }
    }

    serde_json::Value::Object(out)
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
    if !$crate::output::is_json_mode() {
      eprintln!("error: {}", format_args!($($arg)*))
    }
  };
}

/// Print a warning message to stderr.
///
/// Always shown, even in quiet mode. Adds `warning: ` prefix.
///
/// ```no_run
/// # fn main() {
/// cargo_rail::warn!("configuration needs attention");
/// // Output: warning: configuration needs attention
/// # }
/// ```
#[macro_export]
macro_rules! warn {
  ($($arg:tt)*) => {
    if !$crate::output::is_json_mode() {
      eprintln!("warning: {}", format_args!($($arg)*))
    }
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
    if !$crate::output::is_json_mode() {
      eprintln!("help: {}", format_args!($($arg)*))
    }
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

/// Print bounded operational detail only when `--verbose` is active.
#[macro_export]
macro_rules! verbose_progress {
  ($($arg:tt)*) => {
    if $crate::output::is_verbose() {
      $crate::status!($($arg)*)
    }
  };
}

#[cfg(test)]
mod tests {
    use super::{InvocationOutput, OutputProtocol};

    #[test]
    fn raw_protocol_suppresses_advisory_output_without_becoming_json() {
        let output = InvocationOutput::capture_protocol(false, true, OutputProtocol::Raw);

        assert_eq!(output.protocol(), OutputProtocol::Raw);
        assert!(output.quiet, "raw streams must suppress progress and advisory output");
        assert!(!output.verbose(), "raw streams must not enable text detail");
        assert!(!output.color_enabled(), "raw streams must remain byte-stable");
    }
}
