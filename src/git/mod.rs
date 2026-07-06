//! Git operations and commit mapping storage.
//!
//! This module provides:
//! - SystemGit backend using system git binary
//! - Git-notes based commit mapping (rebase-safe)
//! - Low-level git operations (rev-parse, cat-file, push, pull, etc.)
//! - Smart defaults for git references

use std::path::Path;
use std::process::Command;

/// Smart defaults for git references
pub mod defaults;
/// Git-notes based commit mapping storage
pub mod mappings;
/// Git operations (commit, branch, push, pull, etc.)
pub mod ops;
/// SystemGit backend using system git binary
pub mod system;

pub use defaults::detect_default_base_ref;
pub use ops::LogEntry;
pub use system::{CommitInfo, SystemGit, init_repo};

/// Create a properly-configured git command for any repository path.
///
/// This provides the same environment isolation as `SystemGit::git_cmd()` but
/// can be used standalone when you don't need a full `SystemGit` instance.
///
/// Features:
/// - Isolated environment (clears and whitelists only essential vars)
/// - Preserves PATH, HOME, and auth-related variables (SSH_AUTH_SOCK, etc.)
/// - Sets safe git config overrides (protocol.version=2, etc.)
pub(crate) fn git_cmd_for_path(repo_path: &Path) -> Command {
  let mut cmd = Command::new("git");

  // Set working directory
  cmd.arg("-C").arg(repo_path);

  // Isolated environment (don't trust global config)
  cmd.env_clear();

  // Always preserve PATH for process execution.
  if let Some(path) = std::env::var_os("PATH") {
    cmd.env("PATH", path);
  }

  // Preserve common home directory variables for git config/credentials.
  for key in ["HOME", "USERPROFILE", "HOMEDRIVE", "HOMEPATH"] {
    if let Some(val) = std::env::var_os(key) {
      cmd.env(key, val);
    }
  }

  // Preserve common auth-related variables so fetch/push work with SSH agents.
  for key in [
    "SSH_AUTH_SOCK",
    "SSH_ASKPASS",
    "DISPLAY",
    "GIT_ASKPASS",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_TERMINAL_PROMPT",
  ] {
    if let Some(val) = std::env::var_os(key) {
      cmd.env(key, val);
    }
  }

  // Force safe behavior (override user config)
  cmd.arg("-c").arg("protocol.version=2");
  cmd.arg("-c").arg("advice.detachedHead=false");
  cmd.arg("-c").arg("core.quotePath=false"); // Don't escape non-ASCII

  cmd
}
