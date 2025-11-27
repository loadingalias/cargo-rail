//! Error types with contextual messages and exit codes.
//!
//! Provides [`RailError`] with categorized variants, help text, and process exit codes.
//!
//! # Examples
//!
//! ```rust
//! use cargo_rail::error::{RailError, ResultExt};
//!
//! fn parse_number(s: &str) -> Result<u32, RailError> {
//!     s.parse().context("Invalid number format")
//! }
//!
//! let result = parse_number("42");
//! assert!(result.is_ok());
//!
//! let result = parse_number("invalid");
//! assert!(result.is_err());
//! ```

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Exit codes for cargo-rail
///
/// Exit codes follow CI-friendly semantics:
/// - 0 = Success (or check passed with no changes needed)
/// - 1 = Check mode found changes would be made (not an error, but actionable)
/// - 2 = Error occurred (actual failure)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
  /// Success - everything is good
  Success = 0,
  /// Check failed - changes would be made (use in --check mode)
  CheckFailed = 1,
  /// Error occurred (user or system error)
  Error = 2,
}

impl ExitCode {
  /// Convert to i32 for process exit
  pub fn as_i32(self) -> i32 {
    self as i32
  }
}

/// Main error type for cargo-rail
#[derive(Debug)]
pub enum RailError {
  /// Configuration errors
  Config(ConfigError),

  /// Git operation errors
  Git(GitError),

  /// I/O errors
  Io(io::Error),

  /// Generic error with message and optional context
  Message {
    /// Error message
    message: String,
    /// Additional context about the error
    context: Option<String>,
    /// Help text to guide the user
    help: Option<String>,
  },
}

impl RailError {
  /// Create a simple error message
  pub fn message(msg: impl Into<String>) -> Self {
    RailError::Message {
      message: msg.into(),
      context: None,
      help: None,
    }
  }

  /// Create an error with help text
  pub fn with_help(msg: impl Into<String>, help: impl Into<String>) -> Self {
    RailError::Message {
      message: msg.into(),
      context: None,
      help: Some(help.into()),
    }
  }

  /// Add context to an existing error
  pub fn context(self, ctx: impl Into<String>) -> Self {
    let ctx_str = ctx.into();
    match self {
      RailError::Message { message, context, help } => RailError::Message {
        message,
        context: Some(context.map(|c| format!("{}\n{}", ctx_str, c)).unwrap_or(ctx_str)),
        help,
      },
      _ => self,
    }
  }

  /// Get the appropriate exit code for this error
  pub fn exit_code(&self) -> ExitCode {
    // All errors use exit code 2
    ExitCode::Error
  }

  /// Get contextual help message for this error
  pub fn help_message(&self) -> Option<String> {
    match self {
      RailError::Config(e) => e.help_message(),
      RailError::Git(e) => e.help_message(),
      RailError::Message { help, .. } => help.clone(),
      _ => None,
    }
  }
}

impl fmt::Display for RailError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      RailError::Config(e) => write!(f, "{}", e),
      RailError::Git(e) => write!(f, "{}", e),
      RailError::Io(e) => write!(f, "{}", e),
      RailError::Message { message, context, .. } => {
        write!(f, "{}", message)?;
        if let Some(ctx) = context {
          write!(f, "\n{}", ctx)?;
        }
        Ok(())
      }
    }
  }
}

impl std::error::Error for RailError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      RailError::Io(e) => Some(e),
      _ => None,
    }
  }
}

impl From<io::Error> for RailError {
  fn from(err: io::Error) -> Self {
    RailError::Io(err)
  }
}

impl From<String> for RailError {
  fn from(msg: String) -> Self {
    RailError::message(msg)
  }
}

impl From<&str> for RailError {
  fn from(msg: &str) -> Self {
    RailError::message(msg)
  }
}

impl From<toml_edit::TomlError> for RailError {
  fn from(err: toml_edit::TomlError) -> Self {
    RailError::message(format!("invalid TOML: {}", err))
  }
}

impl From<cargo_metadata::Error> for RailError {
  fn from(err: cargo_metadata::Error) -> Self {
    RailError::message(format!("cargo metadata failed: {}", err))
  }
}

impl From<std::num::ParseIntError> for RailError {
  fn from(err: std::num::ParseIntError) -> Self {
    RailError::message(format!("invalid number: {}", err))
  }
}

impl From<toml_edit::de::Error> for RailError {
  fn from(err: toml_edit::de::Error) -> Self {
    RailError::message(format!("invalid TOML: {}", err))
  }
}

impl From<toml_edit::ser::Error> for RailError {
  fn from(err: toml_edit::ser::Error) -> Self {
    RailError::message(format!("TOML serialization failed: {}", err))
  }
}

impl From<serde_json::Error> for RailError {
  fn from(err: serde_json::Error) -> Self {
    RailError::message(format!("invalid JSON: {}", err))
  }
}

impl From<std::str::Utf8Error> for RailError {
  fn from(err: std::str::Utf8Error) -> Self {
    RailError::message(format!("invalid UTF-8: {}", err))
  }
}

impl From<std::string::FromUtf8Error> for RailError {
  fn from(err: std::string::FromUtf8Error) -> Self {
    RailError::message(format!("invalid UTF-8: {}", err))
  }
}

impl From<std::path::StripPrefixError> for RailError {
  fn from(err: std::path::StripPrefixError) -> Self {
    RailError::message(format!("path error: {}", err))
  }
}

impl From<std::env::VarError> for RailError {
  fn from(err: std::env::VarError) -> Self {
    RailError::message(format!("environment variable error: {}", err))
  }
}

/// Configuration-related errors
#[derive(Debug)]
pub enum ConfigError {
  /// rail.toml not found
  NotFound {
    /// Workspace root where config was searched
    workspace_root: PathBuf,
  },

  /// Missing required field
  MissingField {
    /// Name of the missing field
    field: String,
  },

  /// Crate not found in configuration
  CrateNotFound {
    /// Name of the crate that wasn't found
    name: String,
  },
}

impl ConfigError {
  fn help_message(&self) -> Option<String> {
    match self {
      ConfigError::NotFound { .. } => Some("run 'cargo rail init' to create configuration".to_string()),
      ConfigError::CrateNotFound { name } => Some(format!(
        "run 'cargo rail split --check' to list configured crates (did you mean '{}'?)",
        name
      )),
      _ => None,
    }
  }
}

impl fmt::Display for ConfigError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      ConfigError::NotFound { workspace_root } => {
        write!(
          f,
          "no configuration found in {}\n       searched: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml",
          workspace_root.display()
        )
      }
      ConfigError::MissingField { field } => {
        write!(f, "missing required field: {}", field)
      }
      ConfigError::CrateNotFound { name } => {
        write!(f, "crate '{}' not found in configuration", name)
      }
    }
  }
}

/// Git operation errors
#[derive(Debug)]
pub enum GitError {
  /// Git command failed
  CommandFailed {
    /// Command that was executed
    command: String,
    /// Standard error output
    stderr: String,
  },

  /// Repository not found
  RepoNotFound {
    /// Path where repository was expected
    path: PathBuf,
  },

  /// Commit not found
  CommitNotFound {
    /// SHA of the missing commit
    sha: String,
  },

  /// Push failed
  PushFailed {
    /// Remote name
    remote: String,
    /// Branch name
    branch: String,
    /// Failure reason
    reason: String,
  },
}

impl GitError {
  fn help_message(&self) -> Option<String> {
    match self {
      GitError::PushFailed { reason, .. } => {
        if reason.contains("non-fast-forward") {
          Some("pull first, or use --force".to_string())
        } else if reason.contains("permission denied") || reason.contains("403") {
          Some("check SSH key and repository permissions".to_string())
        } else {
          None
        }
      }
      GitError::RepoNotFound { path } => Some(format!("run 'git init {}' or verify the path", path.display())),
      _ => None,
    }
  }
}

impl fmt::Display for GitError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      GitError::CommandFailed { command, stderr } => {
        let stderr = stderr.trim();
        if stderr.is_empty() {
          write!(f, "git {} failed", command)
        } else {
          write!(f, "git {} failed: {}", command, stderr)
        }
      }
      GitError::RepoNotFound { path } => {
        write!(f, "not a git repository: {}", path.display())
      }
      GitError::CommitNotFound { sha } => {
        write!(f, "commit not found: {}", sha)
      }
      GitError::PushFailed { remote, branch, reason } => {
        write!(f, "push to {}/{} failed: {}", remote, branch, reason.trim())
      }
    }
  }
}

/// Result type alias for cargo-rail
pub type RailResult<T> = Result<T, RailError>;

/// Helper trait to add context to Results
pub trait ResultExt<T> {
  /// Add context to an error result
  fn context(self, ctx: impl Into<String>) -> RailResult<T>;

  /// Add context using a closure (lazy evaluation)
  fn with_context<F>(self, f: F) -> RailResult<T>
  where
    F: FnOnce() -> String;
}

impl<T, E> ResultExt<T> for Result<T, E>
where
  E: Into<RailError>,
{
  fn context(self, ctx: impl Into<String>) -> RailResult<T> {
    self.map_err(|e| e.into().context(ctx))
  }

  fn with_context<F>(self, f: F) -> RailResult<T>
  where
    F: FnOnce() -> String,
  {
    self.map_err(|e| e.into().context(f()))
  }
}

/// Print an error to stderr with optional help text
pub fn print_error(error: &RailError) {
  eprintln!("error: {}", error);

  if let Some(help) = error.help_message() {
    eprintln!("help: {}", help);
  }
}

/// Convert anyhow::Error to RailError (for transition period)
impl From<anyhow::Error> for RailError {
  fn from(err: anyhow::Error) -> Self {
    RailError::message(err.to_string())
  }
}
