//! Utility functions for cross-platform path handling

use std::io::{self, Write};
use std::path::Path;

use crate::error::RailResult;

/// Check if a path is a local filesystem path (not a remote URL)
///
/// Returns true for:
/// - Absolute paths on Unix: /path/to/repo
/// - Absolute paths on Windows: C:\path\to\repo or C:/path/to/repo
/// - Relative paths: ./path or ../path
/// - UNC paths on Windows: \\server\share
///
/// Returns false for:
/// - SSH URLs: `git@github.com:user/repo.git`
/// - HTTPS URLs: `https://github.com/user/repo.git`
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

/// Prompt user for confirmation (Enter to confirm, Ctrl+C or any input to cancel)
///
/// Returns Ok(true) if user presses Enter without typing anything.
/// Returns Ok(false) if user types anything before pressing Enter.
pub fn prompt_for_confirmation(message: &str) -> RailResult<bool> {
  print!("\n{}: ", message);
  io::stdout().flush()?;

  let mut input = String::new();
  io::stdin().read_line(&mut input)?;

  // If user just presses Enter (empty line), that's a confirmation
  Ok(input.trim().is_empty())
}

/// Detect CHANGELOG file in a crate directory
///
/// Searches for common changelog file patterns and returns the first match found.
pub fn detect_crate_changelog(crate_dir: &cargo_metadata::camino::Utf8Path) -> Option<std::path::PathBuf> {
  let changelog_patterns = [
    "CHANGELOG.md",
    "CHANGELOG.txt",
    "CHANGELOG",
    "Changelog.md",
    "changelog.md",
    "CHANGES.md",
    "CHANGES.txt",
    "CHANGES",
    "Changes.md",
    "changes.md",
  ];

  for pattern in &changelog_patterns {
    let changelog = crate_dir.join(pattern);
    if changelog.exists() {
      // Return relative path from crate root
      return Some(std::path::PathBuf::from(pattern));
    }
  }

  None
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
  fn test_is_local_path_classification() {
    // Local paths - absolute
    assert!(is_local_path("/home/user/repo"));
    assert!(is_local_path("C:\\Users\\test\\repo"));
    assert!(is_local_path("C:/Users/test/repo"));

    // Local paths - relative
    assert!(is_local_path("./repo"));
    assert!(is_local_path("../path/to/repo"));

    // Windows UNC paths (platform-specific)
    #[cfg(target_os = "windows")]
    assert!(is_local_path("\\\\server\\share\\repo"));

    // Remote URLs - various protocols
    assert!(!is_local_path("git@github.com:user/repo.git"));
    assert!(!is_local_path("https://github.com/user/repo.git"));
    assert!(!is_local_path("ssh://git@github.com/user/repo.git"));

    // Edge cases - ambiguous bare names
    assert!(!is_local_path("repo"));
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
