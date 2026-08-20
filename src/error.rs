//! Error handling with contextual messages and CI-friendly exit codes.
//!
//! Exit code semantics: 0 = success, 1 = check mode found changes, 2 = error.
//! Errors carry optional help text that guides users toward resolution.

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
    Success,
    /// Check failed - changes would be made (use in --check mode)
    CheckFailed,
    /// Error occurred (user or system error)
    Error,
    /// Custom exit code (e.g., propagated from subprocess)
    Custom(i32),
}

impl ExitCode {
    /// Convert to i32 for process exit
    pub fn as_i32(self) -> i32 {
        match self {
            ExitCode::Success => 0,
            ExitCode::CheckFailed => 1,
            ExitCode::Error => 2,
            ExitCode::Custom(code) => code,
        }
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

    /// Check mode found pending changes (not an error, but exits with code 1)
    ///
    /// Used by --check commands to signal that changes would be made.
    /// This is not a failure, but CI should treat it as "action needed".
    CheckHasPendingChanges,

    /// Exit with specific code (no error message printed)
    ///
    /// Used for:
    /// - Propagating subprocess exit codes (e.g., cargo test failures)
    /// - Silent exits after JSON output has been written
    /// - Any case where we need a specific exit code without error output
    ExitWithCode {
        /// The exit code to use
        code: i32,
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
            RailError::CheckHasPendingChanges => ExitCode::CheckFailed,
            RailError::ExitWithCode { code } => ExitCode::Custom(*code),
            _ => ExitCode::Error,
        }
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
            RailError::CheckHasPendingChanges => Ok(()), // Silent - CI signal
            RailError::ExitWithCode { .. } => Ok(()),    // Silent - exit code only
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

    /// Config file exists but failed to parse
    ParseError {
        /// Path to the config file
        path: PathBuf,
        /// Parse error message
        message: String,
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

    /// Invalid configuration value
    InvalidValue {
        /// Field name
        field: String,
        /// Error message
        message: String,
    },

    /// Invalid field configuration
    InvalidField {
        /// Field name
        field: String,
        /// Reason why it's invalid
        reason: String,
    },

    /// Invalid glob pattern
    InvalidGlobPattern {
        /// The invalid pattern
        pattern: String,
        /// Error message
        message: String,
    },
}

impl ConfigError {
    fn help_message(&self) -> Option<String> {
        match self {
            ConfigError::NotFound { .. } => Some("run 'cargo rail init' to create configuration".to_string()),
            ConfigError::ParseError { .. } => Some("check the config file syntax and fix the error".to_string()),
            ConfigError::CrateNotFound { name } => Some(format!(
                "check the '[crates.{name}]' table in rail.toml or run 'cargo rail config validate'"
            )),
            ConfigError::InvalidValue { field, .. } => Some(format!("check the '{}' field in your config file", field)),
            ConfigError::InvalidField { field, .. } => Some(format!("check the '{}' field in your config file", field)),
            ConfigError::InvalidGlobPattern { pattern, .. } => {
                Some(format!("fix or remove the invalid glob pattern: '{}'", pattern))
            }
            ConfigError::MissingField { field } => {
                Some(format!("add the required '{}' field to your config file", field))
            }
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
            ConfigError::ParseError { path, message } => {
                write!(f, "failed to parse config file {}: {}", path.display(), message)
            }
            ConfigError::MissingField { field } => {
                write!(f, "missing required field: {}", field)
            }
            ConfigError::CrateNotFound { name } => {
                write!(f, "crate '{}' not found in configuration", name)
            }
            ConfigError::InvalidValue { field, message } => {
                write!(f, "invalid value for '{}': {}", field, message)
            }
            ConfigError::InvalidField { field, reason } => {
                write!(f, "invalid configuration for '{}': {}", field, reason)
            }
            ConfigError::InvalidGlobPattern { pattern, message } => {
                write!(f, "invalid glob pattern '{}': {}", pattern, message)
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

    /// Worktree has uncommitted changes
    DirtyWorktree {
        /// List of dirty files
        files: Vec<String>,
    },
}

impl GitError {
    fn help_message(&self) -> Option<String> {
        match self {
            GitError::PushFailed { reason, .. } => {
                if reason.contains("non-fast-forward") {
                    Some("fetch and reconcile the remote branch, then retry".to_string())
                } else if reason.contains("permission denied") || reason.contains("403") {
                    Some("check SSH key and repository permissions".to_string())
                } else {
                    None
                }
            }
            GitError::RepoNotFound { path } => Some(format!("run 'git init {}' or verify the path", path.display())),
            GitError::DirtyWorktree { .. } => Some("commit or stash changes, or use --allow-dirty".to_string()),
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
                    write!(f, "{} failed", command)
                } else {
                    write!(f, "{} failed: {}", command, stderr)
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
            GitError::DirtyWorktree { files } => {
                let count = files.len();
                if count <= 5 {
                    write!(f, "working tree has uncommitted changes:\n{}", files.join("\n"))
                } else {
                    write!(
                        f,
                        "working tree has uncommitted changes:\n{}\n  ... and {} more",
                        files[..5].join("\n"),
                        count - 5
                    )
                }
            }
        }
    }
}

/// Result type alias for cargo-rail
pub type RailResult<T> = Result<T, RailError>;

/// Preserve both subprocess streams in an existing Git error diagnostic.
pub(crate) fn git_command_diagnostics(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    let stdout = stdout.trim_end();
    let stderr = stderr.trim_end();
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (false, false) => format!("stdout:\n{}\nstderr:\n{}", stdout, stderr),
    }
}

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

/// Structured JSON error for machine-readable output
#[derive(serde::Serialize)]
struct JsonError {
    error: bool,
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    help: Option<String>,
}

/// Print an error to stderr with optional help text
///
/// In JSON mode, outputs a structured JSON error object instead of text.
pub fn print_error(error: &RailError) {
    // These are not errors to display - they're exit code signals
    // CheckHasPendingChanges: CI signal for "changes would be made"
    // ExitWithCode: subprocess errors or silent exits after JSON output
    if matches!(
        error,
        RailError::CheckHasPendingChanges | RailError::ExitWithCode { .. }
    ) {
        return;
    }

    if crate::output::is_json_mode() {
        print_error_json(error);
    } else {
        crate::error!("{}", error);

        if let Some(help) = error.help_message() {
            crate::help!("{}", help);
        }
    }
}

/// Print error as structured JSON to stdout
fn print_error_json(error: &RailError) {
    let (message, context) = match error {
        RailError::Message { message, context, .. } => (message.clone(), context.clone()),
        _ => (error.to_string(), None),
    };

    let json_error = JsonError {
        error: true,
        code: error.exit_code().as_i32(),
        message,
        context,
        help: error.help_message(),
    };

    // JSON errors go to stdout for consistent machine parsing
    // (stderr may have other output mixed in)
    if let Ok(json) = serde_json::to_string_pretty(&json_error) {
        println!("{}", json);
    } else {
        // Fallback to text if JSON serialization fails
        crate::error!("{}", error);
    }
}
