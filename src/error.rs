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

    /// A typed error from an external parser or subsystem.
    External {
        /// Stable operation-level description.
        message: &'static str,
        /// Native source error retained for inspection and diagnostics.
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// Operation context around another typed Cargo-Rail error.
    Context {
        /// The operation that failed.
        context: String,
        /// The error produced by that operation.
        source: Box<RailError>,
    },

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
        match self {
            RailError::CheckHasPendingChanges | RailError::ExitWithCode { .. } => self,
            source => RailError::Context {
                context: ctx.into(),
                source: Box::new(source),
            },
        }
    }

    fn external(message: &'static str, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::External {
            message,
            source: Box::new(source),
        }
    }

    /// Get the appropriate exit code for this error
    pub fn exit_code(&self) -> ExitCode {
        match self {
            RailError::CheckHasPendingChanges => ExitCode::CheckFailed,
            RailError::ExitWithCode { code } => ExitCode::Custom(*code),
            RailError::Context { source, .. } => source.exit_code(),
            _ => ExitCode::Error,
        }
    }

    /// Get contextual help message for this error
    pub fn help_message(&self) -> Option<String> {
        match self {
            RailError::Config(e) => e.help_message(),
            RailError::Git(e) => e.help_message(),
            RailError::Message { help, .. } => help.clone(),
            RailError::Context { source, .. } => source.help_message(),
            _ => None,
        }
    }

    /// Whether this error chain ends in a broken stdout or stderr pipe.
    pub fn is_broken_pipe(&self) -> bool {
        match self {
            RailError::Io(error) => error.kind() == io::ErrorKind::BrokenPipe,
            RailError::Context { source, .. } => source.is_broken_pipe(),
            _ => false,
        }
    }

    fn machine_parts(&self) -> (String, Option<String>) {
        let mut contexts = Vec::new();
        let mut current = self;
        while let RailError::Context { context, source } = current {
            contexts.push(context.as_str());
            current = source;
        }

        let (message, legacy_context) = match current {
            RailError::Message { message, context, .. } => (message.clone(), context.as_deref()),
            _ => (current.to_string(), None),
        };
        if let Some(context) = legacy_context {
            contexts.push(context);
        }

        let context = (!contexts.is_empty()).then(|| contexts.join("\n"));
        (message, context)
    }
}

impl fmt::Display for RailError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RailError::Config(e) => write!(f, "{}", e),
            RailError::Git(e) => write!(f, "{}", e),
            RailError::Io(e) => write!(f, "{}", e),
            RailError::External { message, source } => write!(f, "{}: {}", message, source),
            RailError::Context { context, source } => write!(f, "{}\n{}", source, context),
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
            RailError::Config(e) => Some(e),
            RailError::Git(e) => Some(e),
            RailError::Io(e) => Some(e),
            RailError::External { source, .. } => Some(source.as_ref()),
            RailError::Context { source, .. } => Some(source.as_ref()),
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
        RailError::external("invalid TOML", err)
    }
}

impl From<cargo_metadata::Error> for RailError {
    fn from(err: cargo_metadata::Error) -> Self {
        RailError::external("cargo metadata failed", err)
    }
}

impl From<std::num::ParseIntError> for RailError {
    fn from(err: std::num::ParseIntError) -> Self {
        RailError::external("invalid number", err)
    }
}

impl From<toml_edit::de::Error> for RailError {
    fn from(err: toml_edit::de::Error) -> Self {
        RailError::external("invalid TOML", err)
    }
}

impl From<toml_edit::ser::Error> for RailError {
    fn from(err: toml_edit::ser::Error) -> Self {
        RailError::external("TOML serialization failed", err)
    }
}

impl From<serde_json::Error> for RailError {
    fn from(err: serde_json::Error) -> Self {
        RailError::external("invalid JSON", err)
    }
}

impl From<std::str::Utf8Error> for RailError {
    fn from(err: std::str::Utf8Error) -> Self {
        RailError::external("invalid UTF-8", err)
    }
}

impl From<std::string::FromUtf8Error> for RailError {
    fn from(err: std::string::FromUtf8Error) -> Self {
        RailError::external("invalid UTF-8", err)
    }
}

impl From<std::path::StripPrefixError> for RailError {
    fn from(err: std::path::StripPrefixError) -> Self {
        RailError::external("path error", err)
    }
}

impl From<std::env::VarError> for RailError {
    fn from(err: std::env::VarError) -> Self {
        RailError::external("environment variable error", err)
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

impl std::error::Error for ConfigError {}

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

impl std::error::Error for GitError {}

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
    let (message, context) = error.machine_parts();

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    fn rail_source(error: &RailError) -> &RailError {
        error
            .source()
            .and_then(|source| source.downcast_ref::<RailError>())
            .expect("context must retain its RailError source")
    }

    #[test]
    fn context_retains_configuration_git_and_filesystem_sources() {
        let config = RailError::Config(ConfigError::MissingField {
            field: "release.source".to_string(),
        })
        .context("validating release policy");
        let config = rail_source(&config);
        assert!(matches!(config, RailError::Config(ConfigError::MissingField { .. })));
        assert!(
            config
                .source()
                .is_some_and(|source| source.downcast_ref::<ConfigError>().is_some())
        );

        let git = RailError::Git(GitError::CommitNotFound {
            sha: "deadbeef".to_string(),
        })
        .context("resolving release base");
        let git = rail_source(&git);
        assert!(matches!(git, RailError::Git(GitError::CommitNotFound { .. })));
        assert!(
            git.source()
                .is_some_and(|source| source.downcast_ref::<GitError>().is_some())
        );

        let io = RailError::from(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
            .context("opening recovery journal");
        let io = rail_source(&io);
        assert!(matches!(io, RailError::Io(_)));
        assert!(
            io.source()
                .is_some_and(|source| source.downcast_ref::<io::Error>().is_some())
        );
    }

    #[test]
    fn external_conversions_retain_cargo_and_serialization_sources() {
        let cargo = RailError::from(cargo_metadata::Error::NoJson).context("capturing Cargo metadata");
        let cargo = rail_source(&cargo);
        assert!(matches!(cargo, RailError::External { .. }));
        assert!(
            cargo
                .source()
                .is_some_and(|source| source.downcast_ref::<cargo_metadata::Error>().is_some())
        );

        let serde = serde_json::from_str::<serde_json::Value>("{").expect_err("JSON must be invalid");
        let serde = RailError::from(serde).context("reading recovery receipt");
        let serde = rail_source(&serde);
        assert!(matches!(serde, RailError::External { .. }));
        assert!(
            serde
                .source()
                .is_some_and(|source| source.downcast_ref::<serde_json::Error>().is_some())
        );
    }

    #[test]
    fn nested_context_survives_text_machine_help_and_exit_boundaries() {
        let error = RailError::with_help("recovery failed", "resume with the recorded transaction ID")
            .context("restoring manifests")
            .context("aborting release transaction");

        assert_eq!(
            error.to_string(),
            "recovery failed\nrestoring manifests\naborting release transaction"
        );
        assert_eq!(error.exit_code(), ExitCode::Error);
        assert_eq!(
            error.help_message().as_deref(),
            Some("resume with the recorded transaction ID")
        );

        let (message, context) = error.machine_parts();
        assert_eq!(message, "recovery failed");
        assert_eq!(
            context.as_deref(),
            Some("aborting release transaction\nrestoring manifests")
        );
    }

    #[test]
    fn context_preserves_control_outcomes_and_broken_pipe_detection() {
        assert!(matches!(
            RailError::CheckHasPendingChanges.context("ignored"),
            RailError::CheckHasPendingChanges
        ));
        assert!(matches!(
            RailError::ExitWithCode { code: 42 }.context("ignored"),
            RailError::ExitWithCode { code: 42 }
        ));

        let broken_pipe =
            RailError::from(io::Error::new(io::ErrorKind::BrokenPipe, "closed")).context("writing report");
        assert!(broken_pipe.is_broken_pipe());
    }
}
