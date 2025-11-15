//! Utility functions for cross-platform path handling

use std::path::Path;

/// Check if a path is a local filesystem path (not a remote URL)
///
/// Returns true for:
/// - Absolute paths on Unix: /path/to/repo
/// - Absolute paths on Windows: C:\path\to\repo or C:/path/to/repo
/// - Relative paths: ./path or ../path
/// - UNC paths on Windows: \\server\share
///
/// Returns false for:
/// - SSH URLs: git@github.com:user/repo.git
/// - HTTPS URLs: <https://github.com/user/repo.git>
pub fn is_local_path(path: &str) -> bool {
  let p = Path::new(path);

  // Check for relative paths
  if path.starts_with("./") || path.starts_with("../") {
    return true;
  }

  // Check for Windows drive letter (C:\ or C:/)
  // Must check before URL check since Windows paths contain ':'
  if path.len() >= 3 {
    let bytes = path.as_bytes();
    if bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/') {
      return true;
    }
  }

  // Check for Windows UNC paths (\\server\share)
  if path.starts_with("\\\\") {
    return true;
  }

  // Check for Unix absolute paths (/path/to/repo)
  // Important: Check this BEFORE is_absolute() because on Windows,
  // Path::is_absolute() returns false for Unix-style paths
  if path.starts_with('/') {
    // Make sure it's not part of a URL pattern
    if !path.contains("://") && !path.contains('@') {
      return true;
    }
  }

  // Check for absolute paths (fallback for platform-specific cases)
  if p.is_absolute() {
    return true;
  }

  // If it contains :// it's a URL
  if path.contains("://") {
    return false;
  }

  // If it contains @ it's likely an SSH URL (git@github.com:user/repo.git)
  if path.contains('@') {
    return false;
  }

  // Default to false for safety (require preflight checks)
  false
}

/// Convert a path to Git format (always forward slashes)
///
/// Git expects paths with forward slashes, even on Windows.
/// This function converts backslashes to forward slashes for use in Git commands.
pub fn path_to_git_format(path: &Path) -> String {
  // On Windows, convert backslashes to forward slashes
  // On Unix, this is a no-op since paths already use forward slashes
  #[cfg(target_os = "windows")]
  {
    path.to_string_lossy().replace('\\', "/")
  }
  #[cfg(not(target_os = "windows"))]
  {
    path.to_string_lossy().to_string()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;

  #[test]
  fn test_unix_absolute_paths() {
    assert!(is_local_path("/home/user/repo"));
    assert!(is_local_path("/tmp/test-repo"));
    assert!(is_local_path("/var/lib/git"));
  }

  #[test]
  fn test_relative_paths() {
    assert!(is_local_path("./repo"));
    assert!(is_local_path("../repo"));
    assert!(is_local_path("./path/to/repo"));
    assert!(is_local_path("../path/to/repo"));
  }

  #[test]
  fn test_windows_absolute_paths() {
    // These work on all platforms because Path::is_absolute() handles them
    assert!(is_local_path("C:\\Users\\test\\repo"));
    assert!(is_local_path("C:/Users/test/repo"));
    assert!(is_local_path("D:\\path\\to\\repo"));
    assert!(is_local_path("E:/another/path"));
  }

  #[test]
  fn test_windows_unc_paths() {
    // This test only runs on Windows where the cfg is active
    #[cfg(target_os = "windows")]
    {
      assert!(is_local_path("\\\\server\\share\\repo"));
      assert!(is_local_path("\\\\nas\\backup\\git"));
    }
  }

  #[test]
  fn test_remote_urls() {
    assert!(!is_local_path("git@github.com:user/repo.git"));
    assert!(!is_local_path("git@gitlab.com:org/project.git"));
    assert!(!is_local_path("https://github.com/user/repo.git"));
    assert!(!is_local_path("http://gitlab.com/user/repo.git"));
    assert!(!is_local_path("ssh://git@github.com/user/repo.git"));
    assert!(!is_local_path("https://bitbucket.org/user/repo.git"));
  }

  #[test]
  fn test_edge_cases() {
    // Just a name, not a path - should be false for safety
    assert!(!is_local_path("repo"));
    assert!(!is_local_path("my-crate"));
    assert!(!is_local_path("some-name"));

    // Empty string
    assert!(!is_local_path(""));
  }

  #[test]
  fn test_path_to_git_format_unix() {
    #[cfg(not(target_os = "windows"))]
    {
      let path = PathBuf::from("/home/user/repo/src/main.rs");
      assert_eq!(path_to_git_format(&path), "/home/user/repo/src/main.rs");

      let path = PathBuf::from("./relative/path.rs");
      assert_eq!(path_to_git_format(&path), "./relative/path.rs");
    }
  }

  #[test]
  fn test_path_to_git_format_windows() {
    #[cfg(target_os = "windows")]
    {
      let path = PathBuf::from("C:\\Users\\test\\repo\\src\\main.rs");
      assert_eq!(path_to_git_format(&path), "C:/Users/test/repo/src/main.rs");

      let path = PathBuf::from("..\\relative\\path.rs");
      assert_eq!(path_to_git_format(&path), "../relative/path.rs");
    }
  }
}
