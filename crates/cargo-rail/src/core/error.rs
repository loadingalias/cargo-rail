//! Error types for cargo-rail with contextual messages and exit codes
//!
//! This module provides a unified error type that categorizes errors and provides
//! contextual help messages to users.
//!
//! Note: Many error types are defined but not yet fully integrated. They will be used
//! as we migrate commands to use the RailError type with contextual help messages.

#![allow(dead_code)]

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Exit codes for cargo-rail
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
  /// Success
  Success = 0,
  /// User error (config, invalid args, missing files)
  UserError = 1,
  /// System error (git, network, I/O)
  SystemError = 2,
  /// Validation failure (checks failed, SSH, remotes)
  ValidationError = 3,
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

  /// Network/remote errors
  Network(NetworkError),

  /// Validation errors (SSH, paths, etc.)
  Validation(ValidationError),

  /// I/O errors
  Io(io::Error),

  /// Generic error with context
  Other(anyhow::Error),
}

impl RailError {
  /// Get the appropriate exit code for this error
  pub fn exit_code(&self) -> ExitCode {
    match self {
      RailError::Config(_) => ExitCode::UserError,
      RailError::Git(_) => ExitCode::SystemError,
      RailError::Network(_) => ExitCode::SystemError,
      RailError::Validation(_) => ExitCode::ValidationError,
      RailError::Io(_) => ExitCode::SystemError,
      RailError::Other(_) => ExitCode::UserError,
    }
  }

  /// Get contextual help message for this error
  pub fn help_message(&self) -> Option<String> {
    match self {
      RailError::Config(e) => e.help_message(),
      RailError::Git(e) => e.help_message(),
      RailError::Network(e) => e.help_message(),
      RailError::Validation(e) => e.help_message(),
      _ => None,
    }
  }
}

impl fmt::Display for RailError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      RailError::Config(e) => write!(f, "{}", e),
      RailError::Git(e) => write!(f, "{}", e),
      RailError::Network(e) => write!(f, "{}", e),
      RailError::Validation(e) => write!(f, "{}", e),
      RailError::Io(e) => write!(f, "I/O error: {}", e),
      RailError::Other(e) => write!(f, "{}", e),
    }
  }
}

impl std::error::Error for RailError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      RailError::Io(e) => Some(e),
      RailError::Other(e) => e.source(),
      _ => None,
    }
  }
}

impl From<io::Error> for RailError {
  fn from(err: io::Error) -> Self {
    RailError::Io(err)
  }
}

impl From<anyhow::Error> for RailError {
  fn from(err: anyhow::Error) -> Self {
    RailError::Other(err)
  }
}

/// Configuration-related errors
#[derive(Debug)]
pub enum ConfigError {
  /// rail.toml not found
  NotFound { workspace_root: PathBuf },

  /// Invalid TOML syntax
  InvalidToml { path: PathBuf, message: String },

  /// Missing required field
  MissingField { field: String },

  /// Invalid remote URL
  InvalidRemote { url: String, reason: String },

  /// Crate not found in configuration
  CrateNotFound { name: String },

  /// Path validation failed
  InvalidPath { path: PathBuf, reason: String },
}

impl ConfigError {
  fn help_message(&self) -> Option<String> {
    match self {
      ConfigError::NotFound { .. } => Some("Run `cargo rail init` to create a configuration file.".to_string()),
      ConfigError::InvalidToml { .. } => {
        Some("Check your .rail/config.toml syntax. Use `cargo rail doctor` to validate.".to_string())
      }
      ConfigError::InvalidRemote { url, .. } => {
        if url.starts_with("http://") {
          Some("Use SSH URLs (git@github.com:...) or HTTPS (https://...) for remotes.".to_string())
        } else {
          Some("Ensure the remote URL is valid. Example: git@github.com:user/repo.git".to_string())
        }
      }
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
      ConfigError::InvalidToml { path, message } => {
        write!(f, "Invalid TOML in {}: {}", path.display(), message)
      }
      ConfigError::MissingField { field } => {
        write!(f, "Missing required field in config: {}", field)
      }
      ConfigError::InvalidRemote { url, reason } => {
        write!(f, "Invalid remote URL '{}': {}", url, reason)
      }
      ConfigError::CrateNotFound { name } => {
        write!(f, "Crate '{}' not found in configuration", name)
      }
      ConfigError::InvalidPath { path, reason } => {
        write!(f, "Invalid path '{}': {}", path.display(), reason)
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

  /// Pull failed
  PullFailed {
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
      GitError::PullFailed { remote, branch, reason } => {
        write!(f, "Pull from {}/{} failed: {}", remote, branch, reason)
      }
    }
  }
}

/// Network and remote operation errors
#[derive(Debug)]
pub enum NetworkError {
  /// Remote not accessible
  RemoteUnreachable { url: String, reason: String },

  /// SSH connection failed
  SshFailed { host: String, reason: String },

  /// Timeout
  Timeout { operation: String },
}

impl NetworkError {
  fn help_message(&self) -> Option<String> {
    match self {
      NetworkError::RemoteUnreachable { .. } => Some(
        "Check your network connection and remote URL. Use `cargo rail doctor --thorough` to test remotes.".to_string(),
      ),
      NetworkError::SshFailed { .. } => {
        Some("Verify SSH key is added to GitHub/GitLab. Test with: ssh -T git@github.com".to_string())
      }
      _ => None,
    }
  }
}

impl fmt::Display for NetworkError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      NetworkError::RemoteUnreachable { url, reason } => {
        write!(f, "Remote '{}' is unreachable: {}", url, reason)
      }
      NetworkError::SshFailed { host, reason } => {
        write!(f, "SSH connection to '{}' failed: {}", host, reason)
      }
      NetworkError::Timeout { operation } => {
        write!(f, "Operation timed out: {}", operation)
      }
    }
  }
}

/// Validation errors
#[derive(Debug)]
pub enum ValidationError {
  /// SSH key not found or invalid
  SshKey { message: String },

  /// Path validation failed
  PathValidation { path: PathBuf, reason: String },

  /// Workspace validation failed
  WorkspaceInvalid { reason: String },

  /// Check failed
  CheckFailed { check_name: String, message: String },
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
      _ => None,
    }
  }
}

impl fmt::Display for ValidationError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      ValidationError::SshKey { message } => {
        write!(f, "SSH key validation failed: {}", message)
      }
      ValidationError::PathValidation { path, reason } => {
        write!(f, "Path validation failed for '{}': {}", path.display(), reason)
      }
      ValidationError::WorkspaceInvalid { reason } => {
        write!(f, "Workspace validation failed: {}", reason)
      }
      ValidationError::CheckFailed { check_name, message } => {
        write!(f, "Check '{}' failed: {}", check_name, message)
      }
    }
  }
}

/// Result type alias for cargo-rail
pub type RailResult<T> = Result<T, RailError>;
