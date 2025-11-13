//! Error types for cargo-rail with contextual messages and exit codes
//!
//! This module provides a unified error type that categorizes errors and provides
//! contextual help messages to users. Every error includes a helpful suggestion
//! to guide users toward resolution.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Exit codes for cargo-rail
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
  /// User error (config, invalid args, missing files)
  User = 1,
  /// System error (git, network, I/O)
  System = 2,
  /// Validation failure (checks failed, SSH, remotes)
  Validation = 3,
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

  /// Validation errors (SSH, paths, etc.)
  Validation(ValidationError),

  /// I/O errors
  Io(io::Error),

  /// Generic error with message and optional context
  Message {
    message: String,
    context: Option<String>,
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
    match self {
      RailError::Config(_) => ExitCode::User,
      RailError::Git(_) => ExitCode::System,
      RailError::Validation(_) => ExitCode::Validation,
      RailError::Io(_) => ExitCode::System,
      RailError::Message { .. } => ExitCode::User,
    }
  }

  /// Get contextual help message for this error
  pub fn help_message(&self) -> Option<String> {
    match self {
      RailError::Config(e) => e.help_message(),
      RailError::Git(e) => e.help_message(),
      RailError::Validation(e) => e.help_message(),
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
      RailError::Validation(e) => write!(f, "{}", e),
      RailError::Io(e) => write!(f, "I/O error: {}", e),
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
    RailError::message(format!("TOML parse error: {}", err))
  }
}

impl From<cargo_metadata::Error> for RailError {
  fn from(err: cargo_metadata::Error) -> Self {
    RailError::message(format!("Cargo metadata error: {}", err))
  }
}

impl From<std::num::ParseIntError> for RailError {
  fn from(err: std::num::ParseIntError) -> Self {
    RailError::message(format!("Parse error: {}", err))
  }
}

impl From<toml_edit::de::Error> for RailError {
  fn from(err: toml_edit::de::Error) -> Self {
    RailError::message(format!("TOML deserialization error: {}", err))
  }
}

impl From<toml_edit::ser::Error> for RailError {
  fn from(err: toml_edit::ser::Error) -> Self {
    RailError::message(format!("TOML serialization error: {}", err))
  }
}

impl From<serde_json::Error> for RailError {
  fn from(err: serde_json::Error) -> Self {
    RailError::message(format!("JSON error: {}", err))
  }
}

impl From<std::str::Utf8Error> for RailError {
  fn from(err: std::str::Utf8Error) -> Self {
    RailError::message(format!("UTF-8 error: {}", err))
  }
}

impl From<std::string::FromUtf8Error> for RailError {
  fn from(err: std::string::FromUtf8Error) -> Self {
    RailError::message(format!("UTF-8 conversion error: {}", err))
  }
}

// Gix (gitoxide) error types
impl From<gix::open::Error> for RailError {
  fn from(err: gix::open::Error) -> Self {
    RailError::message(format!("Git repository error: {}", err))
  }
}

impl From<gix::reference::find::existing::Error> for RailError {
  fn from(err: gix::reference::find::existing::Error) -> Self {
    RailError::message(format!("Git reference error: {}", err))
  }
}

impl From<gix::object::find::existing::Error> for RailError {
  fn from(err: gix::object::find::existing::Error) -> Self {
    RailError::message(format!("Git object error: {}", err))
  }
}

impl From<gix::object::peel::to_kind::Error> for RailError {
  fn from(err: gix::object::peel::to_kind::Error) -> Self {
    RailError::message(format!("Git object peel error: {}", err))
  }
}

impl From<gix::traverse::tree::breadthfirst::Error> for RailError {
  fn from(err: gix::traverse::tree::breadthfirst::Error) -> Self {
    RailError::message(format!("Git tree traversal error: {}", err))
  }
}

impl From<gix::object::commit::Error> for RailError {
  fn from(err: gix::object::commit::Error) -> Self {
    RailError::message(format!("Git commit error: {}", err))
  }
}

// Additional gix error types for comprehensive coverage
impl From<gix::object::try_into::Error> for RailError {
  fn from(err: gix::object::try_into::Error) -> Self {
    RailError::message(format!("Git object conversion error: {}", err))
  }
}

impl From<gix::head::peel::to_commit::Error> for RailError {
  fn from(err: gix::head::peel::to_commit::Error) -> Self {
    RailError::message(format!("Git HEAD peel error: {}", err))
  }
}

impl From<gix::worktree::open_index::Error> for RailError {
  fn from(err: gix::worktree::open_index::Error) -> Self {
    RailError::message(format!("Git worktree index error: {}", err))
  }
}

impl From<gix::path::Utf8Error> for RailError {
  fn from(err: gix::path::Utf8Error) -> Self {
    RailError::message(format!("Git path UTF-8 error: {}", err))
  }
}

impl From<std::path::StripPrefixError> for RailError {
  fn from(err: std::path::StripPrefixError) -> Self {
    RailError::message(format!("Path strip prefix error: {}", err))
  }
}

impl From<std::env::VarError> for RailError {
  fn from(err: std::env::VarError) -> Self {
    RailError::message(format!("Environment variable error: {}", err))
  }
}

impl From<gix::init::Error> for RailError {
  fn from(err: gix::init::Error) -> Self {
    RailError::message(format!("Git init error: {}", err))
  }
}

/// Configuration-related errors
#[derive(Debug)]
pub enum ConfigError {
  /// rail.toml not found
  NotFound { workspace_root: PathBuf },

  /// Missing required field
  MissingField { field: String },

  /// Crate not found in configuration
  CrateNotFound { name: String },
}

impl ConfigError {
  fn help_message(&self) -> Option<String> {
    match self {
      ConfigError::NotFound { .. } => Some("Run `cargo rail init` to create a configuration file.".to_string()),
      ConfigError::CrateNotFound { name } => Some(format!(
        "Available crates can be listed with `cargo rail status`. Did you mean to run `cargo rail init` for '{}'?",
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
          "No cargo-rail configuration found.\nExpected file: {}/.rail/config.toml",
          workspace_root.display()
        )
      }
      ConfigError::MissingField { field } => {
        write!(f, "Missing required field in config: {}", field)
      }
      ConfigError::CrateNotFound { name } => {
        write!(f, "Crate '{}' not found in configuration", name)
      }
    }
  }
}

/// Git operation errors
#[derive(Debug)]
pub enum GitError {
  /// Git command failed
  CommandFailed { command: String, stderr: String },

  /// Repository not found
  RepoNotFound { path: PathBuf },

  /// Commit not found
  CommitNotFound { sha: String },

  /// Branch operation failed
  BranchError { message: String },

  /// Push failed
  PushFailed {
    remote: String,
    branch: String,
    reason: String,
  },
}

impl GitError {
  fn help_message(&self) -> Option<String> {
    match self {
      GitError::PushFailed { reason, .. } => {
        if reason.contains("non-fast-forward") {
          Some("The remote has commits you don't have. Pull first or use --force (dangerous).".to_string())
        } else if reason.contains("permission denied") || reason.contains("403") {
          Some("Check your SSH key permissions and GitHub access. Run `cargo rail doctor` to diagnose.".to_string())
        } else {
          None
        }
      }
      GitError::RepoNotFound { path } => Some(format!(
        "Initialize the repository first or check the path: {}",
        path.display()
      )),
      _ => None,
    }
  }
}

impl fmt::Display for GitError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      GitError::CommandFailed { command, stderr } => {
        write!(f, "Git command failed: {}\n{}", command, stderr)
      }
      GitError::RepoNotFound { path } => {
        write!(f, "Git repository not found at: {}", path.display())
      }
      GitError::CommitNotFound { sha } => {
        write!(f, "Commit not found: {}", sha)
      }
      GitError::BranchError { message } => {
        write!(f, "Branch operation failed: {}", message)
      }
      GitError::PushFailed { remote, branch, reason } => {
        write!(f, "Push to {}/{} failed: {}", remote, branch, reason)
      }
    }
  }
}

/// Validation errors
#[derive(Debug)]
pub enum ValidationError {
  /// SSH key not found or invalid
  SshKey { message: String },

  /// Workspace validation failed
  WorkspaceInvalid { reason: String },
}

impl ValidationError {
  fn help_message(&self) -> Option<String> {
    match self {
      ValidationError::SshKey { .. } => {
        Some("Create an SSH key with: ssh-keygen -t ed25519 -C \"your_email@example.com\"".to_string())
      }
      ValidationError::WorkspaceInvalid { .. } => {
        Some("Run `cargo rail doctor` to diagnose workspace issues.".to_string())
      }
    }
  }
}

impl fmt::Display for ValidationError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      ValidationError::SshKey { message } => {
        write!(f, "SSH key validation failed: {}", message)
      }
      ValidationError::WorkspaceInvalid { reason } => {
        write!(f, "Workspace validation failed: {}", reason)
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

/// Pretty-print an error to stderr with colors and help text
pub fn print_error(error: &RailError) {
  eprintln!("\n❌ {}\n", error);

  if let Some(help) = error.help_message() {
    eprintln!("💡 Help: {}\n", help);
  }
}

/// Convert anyhow::Error to RailError (for transition period)
impl From<anyhow::Error> for RailError {
  fn from(err: anyhow::Error) -> Self {
    RailError::message(err.to_string())
  }
}
