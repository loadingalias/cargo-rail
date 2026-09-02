//! Git operations and commit mapping storage.
//!
//! This module provides:
//! - SystemGit backend using system git binary
//! - Git-native commit-origin mapping in ordinary history
//! - Low-level git operations (rev-parse, cat-file, push, pull, etc.)
//! - Smart defaults for git references

use std::path::Path;
use std::process::Command;

/// Smart defaults for git references
pub mod defaults;
/// Git-native commit mapping storage
pub mod mappings;
/// Git operations (commit, branch, push, pull, etc.)
pub mod ops;
/// SystemGit backend using system git binary
pub mod system;

pub use defaults::detect_default_base_ref;
pub use ops::LogEntry;
pub use system::{CommitInfo, CommitMetadata, SystemGit, init_repo};

pub(crate) fn git_command() -> Command {
    crate::instrumentation::record_git_subprocess();
    Command::new("git")
}

/// Create a properly configured Git command for a repository path.
///
/// Commands inherit the caller environment so credentials, signing agents,
/// toolchains, caches, proxies, and hooks behave exactly as they do for a
/// direct Git invocation. Repository-redirection variables are removed because
/// cargo-rail owns the repository boundary through `git -C <path>`.
pub(crate) fn git_cmd_for_path(repo_path: &Path) -> Command {
    let mut cmd = git_command();
    cmd.arg("-C").arg(repo_path);
    sanitize_git_environment(&mut cmd);
    // Authority reads must never hydrate promisor objects implicitly. Explicit
    // fetch operations remain explicit command effects, while rev-list,
    // cat-file, ancestry, and exact-object checks fail closed when local
    // history is incomplete.
    cmd.env("GIT_NO_LAZY_FETCH", "1");

    // Stable machine-facing behavior without disabling user configuration.
    cmd.arg("-c").arg("protocol.version=2");
    cmd.arg("-c").arg("advice.detachedHead=false");
    cmd.arg("-c").arg("core.quotePath=false"); // Don't escape non-ASCII

    cmd
}

fn sanitize_git_environment(cmd: &mut Command) {
    // Git documents these as repository-local or repository-selection inputs.
    // Inheriting them would let an ambient shell override the explicit -C path.
    for key in [
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_DIR",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_GRAFT_FILE",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_NAMESPACE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_PREFIX",
        "GIT_QUARANTINE_PATH",
        "GIT_REPLACE_REF_BASE",
        "GIT_SHALLOW_FILE",
        "GIT_WORK_TREE",
    ] {
        cmd.env_remove(key);
    }
}
