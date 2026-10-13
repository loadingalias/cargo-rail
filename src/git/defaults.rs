//! Select a default base reference for Git change detection.

use crate::error::{RailError, RailResult};
use crate::git::SystemGit;

/// Detect the default branch that change detection compares against.
///
/// Tries `origin/HEAD`, then `origin/main`, `origin/master`, `main`, and `master`.
/// Fails when none exists instead of guessing a nearer commit, because a nearer base
/// would silently leave earlier commits out of the comparison.
///
/// # Example
///
/// ```rust,no_run
/// # use cargo_rail::git::defaults::detect_default_base_ref;
/// # use cargo_rail::git::SystemGit;
/// # let git = SystemGit::open(std::path::Path::new(".")).unwrap();
/// let base_ref = detect_default_base_ref(&git)?;
/// // Returns: the remote default, or the first conventional default branch that exists
/// # Ok::<(), cargo_rail::error::RailError>(())
/// ```
pub fn detect_default_base_ref(git: &SystemGit) -> RailResult<String> {
    let output = git
        .git_cmd()
        .args(["symbolic-ref", "refs/remotes/origin/HEAD", "--short"])
        .output();

    if let Ok(out) = output
        && out.status.success()
    {
        let symref = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !symref.is_empty() {
            return Ok(symref);
        }
    }

    for candidate in ["origin/main", "origin/master", "main", "master"] {
        if git.resolve_reference(candidate).is_ok() {
            return Ok(candidate.to_string());
        }
    }
    Err(RailError::with_help(
        "cannot determine the default branch: none of origin/HEAD, origin/main, origin/master, main, or master exists",
        "pass --since <REF> to name the comparison base",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_base_prefers_remote_head_then_conventional_branches_and_never_guesses() {
        let directory = tempfile::tempdir().unwrap();
        crate::git::init_repo(directory.path(), "main").unwrap();
        let git = SystemGit::open(directory.path()).unwrap();
        git.set_config("user.name", "Test User").unwrap();
        git.set_config("user.email", "test@example.com").unwrap();
        git.set_config("commit.gpgsign", "false").unwrap();
        // An unborn repository has no default branch to compare against.
        let error = detect_default_base_ref(&git).unwrap_err();
        assert!(
            error.to_string().contains("cannot determine the default branch"),
            "{error}"
        );

        let commit = git
            .git_cmd()
            .args(["commit", "--allow-empty", "-m", "initial"])
            .output()
            .unwrap();
        assert!(commit.status.success(), "{commit:?}");
        assert_eq!(detect_default_base_ref(&git).unwrap(), "main");
        for name in ["main", "stable"] {
            let output = git
                .git_cmd()
                .args(["update-ref", &format!("refs/remotes/origin/{name}"), "HEAD"])
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        }
        assert_eq!(detect_default_base_ref(&git).unwrap(), "origin/main");

        let output = git
            .git_cmd()
            .args(["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/stable"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(detect_default_base_ref(&git).unwrap(), "origin/stable");
    }
}
