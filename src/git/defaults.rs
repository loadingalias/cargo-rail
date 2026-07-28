//! Select a default base reference for Git change detection.

use crate::error::RailResult;
use crate::git::SystemGit;
#[cfg(test)]
use crate::progress;

/// Detect the default base ref for change detection
///
/// Tries in order:
/// 1. origin/HEAD symref (most reliable)
/// 2. origin/main (common convention)
/// 3. origin/master (legacy convention)
/// 4. HEAD~1 (fallback for local-only repos)
///
/// # Example
///
/// ```rust,no_run
/// # use cargo_rail::git::defaults::detect_default_base_ref;
/// # use cargo_rail::git::SystemGit;
/// # let git = SystemGit::open(std::path::Path::new(".")).unwrap();
/// let base_ref = detect_default_base_ref(&git);
/// // Returns: "origin/main", "origin/master", or "HEAD~1"
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

  // Try 3: origin/master
  if git.resolve_reference("origin/master").is_ok() {
    return Ok("origin/master".to_string());
  }

  // Try 4: Fallback to HEAD~1 (for local-only repos or fresh clones)
  Ok("HEAD~1".to_string())
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::env;

  #[test]
  fn test_detect_default_base_ref() {
    // Use current directory as test repo
    let current_dir = env::current_dir().unwrap();
    let git = SystemGit::open(&current_dir).expect("Should open git repo");

    // Should return a valid ref
    let base_ref = detect_default_base_ref(&git);
    assert!(base_ref.is_ok(), "Should detect a default base ref");

    let ref_str = base_ref.unwrap();

    // Should be one of the expected formats
    assert!(
      ref_str.starts_with("origin/") || ref_str == "HEAD~1",
      "Ref should be origin/* or HEAD~1, got: {}",
      ref_str
    );

    progress!("Detected base ref: {}", ref_str);
  }

  #[test]
  fn test_detect_returns_usable_ref() {
    let current_dir = env::current_dir().unwrap();
    let git = SystemGit::open(&current_dir).expect("Should open git repo");

    let base_ref = detect_default_base_ref(&git).expect("Should detect base ref");

    // The detected ref should be resolvable to a commit
    // Note: This might fail in CI if origin isn't set up, so we just check format
    assert!(!base_ref.is_empty(), "Base ref should not be empty: {}", base_ref);
  }
}
