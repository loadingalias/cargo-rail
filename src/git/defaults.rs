//! Select a default base reference for Git change detection.

use crate::error::RailResult;
use crate::git::SystemGit;

/// Detect the default base ref for change detection
///
/// Tries in order:
/// 1. origin/HEAD symref (most reliable)
/// 2. origin/main (common convention)
/// 3. HEAD~1 (fallback for local-only repos)
///
/// # Example
///
/// ```rust,no_run
/// # use cargo_rail::git::defaults::detect_default_base_ref;
/// # use cargo_rail::git::SystemGit;
/// # let git = SystemGit::open(std::path::Path::new(".")).unwrap();
/// let base_ref = detect_default_base_ref(&git);
/// // Returns: the remote default, "origin/main", or "HEAD~1"
/// ```
pub fn detect_default_base_ref(git: &SystemGit) -> RailResult<String> {
    // Try 1: origin/HEAD symref (most reliable)
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

    // Try 2: origin/main
    if git.resolve_reference("origin/main").is_ok() {
        return Ok("origin/main".to_string());
    }

    // Try 3: Fallback to HEAD~1 (for local-only repos or fresh clones)
    Ok("HEAD~1".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_base_prefers_remote_head_then_main_then_local_fallback() {
        let directory = tempfile::tempdir().unwrap();
        crate::git::init_repo(directory.path(), "main").unwrap();
        let git = SystemGit::open(directory.path()).unwrap();
        git.set_config("user.name", "Test User").unwrap();
        git.set_config("user.email", "test@example.com").unwrap();
        git.set_config("commit.gpgsign", "false").unwrap();
        assert_eq!(detect_default_base_ref(&git).unwrap(), "HEAD~1");

        let commit = git
            .git_cmd()
            .args(["commit", "--allow-empty", "-m", "initial"])
            .output()
            .unwrap();
        assert!(commit.status.success(), "{commit:?}");
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
